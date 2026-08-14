"""The per-context proxy handshake, and nothing else.

Chromeleon attaches an authenticated proxy to a browser context with two
browser-level CDP commands, in order, on one connection:

    Target.setProxyCredentials  {proxyServer, username, password}
    Target.createBrowserContext {proxyServer}

That is a PROTOCOL fact, not a Playwright fact. This module knows only the
protocol: it takes a ``send(method, params)`` callable and holds the lock that
the single-use registration requires. Every driver — Playwright, Puppeteer,
Selenium, a raw WebSocket — is an adapter of a few lines on top.

The rules encoded here, none of which are visible when you get them wrong:

  * the proxyServer string must be byte-identical in both commands;
  * credentials go only in the registration, never on the context;
  * the registration is single-use and carries no correlation token, so the
    register-then-create pair must be serialized per (connection, proxyServer);
    a second registration is rejected until the first is consumed.

Not enforced here, because the binary now enforces it: passing credentials at
LAUNCH leaves the exit IP unresolved, which unbinds persona geo and disables
WebRTC masking. Chromeleon fails closed on that with a remediation banner.
"""
from __future__ import annotations

import ipaddress
import threading
from contextlib import contextmanager
from urllib.parse import unquote, urlsplit
from typing import Any, Callable, Iterator, NamedTuple

__all__ = [
    "LAUNCH_ARGS",
    "SUPPRESSED_DEFAULT_ARGS",
    "ProxySpec",
    "normalize_server",
    "parse_proxy",
    "proxy_registration",
    # Captcha solver (Chromeleon CDP domain).
    "CAPTCHA_SOLVER_SWITCH",
    "CAPTCHA_MODEL_PATH_SWITCH",
    "ENABLE_METHOD",
    "DISABLE_METHOD",
    "SOLVER_EVAL_METHOD",
    "CAPTCHA_DETECTED",
    "CAPTCHA_SOLVING",
    "CAPTCHA_SOLVED",
    "CAPTCHA_FAILED",
    "SOLVER_EVAL_RESULT",
    "CAPTCHA_EVENTS",
    "captcha_launch_args",
    "solver_eval_params",
]

#: Launch flags a per-context proxy needs. A context-level proxy routes HTTP,
#: but WebRTC's UDP socket is browser-level and would otherwise gather ICE over
#: the real route. Chromeleon auto-appends this for LAUNCH-time proxies only,
#: which per-context proxies by definition are not.
LAUNCH_ARGS: tuple[str, ...] = (
    "--webrtc-ip-handling-policy=disable_non_proxied_udp",
)

#: Playwright default switches that :func:`chromeleon.launch` asks Playwright NOT
#: to add.
#:
#: Playwright launches Chromium with ~46 flags of its own. Measured 2026-08-10
#: over 113 paired trials on 113 distinct fresh exits, a Playwright-launched
#: Chromeleon was blocked by Google materially more often than the same binary
#: launched bare on the same exit in the same minute: 10.6% vs 32.7% pass,
#: discordant 27-2, McNemar p = 1.6e-6, effect +22.1 pp (95% CI [+12.8, +31.5]).
#: Attaching to a bare launch with connect_over_cdp instead recovers the whole
#: penalty, which locates the cost in how Playwright LAUNCHES, not in CDP itself.
#:
#: ⚠️ The individual flag responsible has NOT been isolated. An early reading
#: blamed --disable-field-trial-config; that did not replicate (30 pairs, p=0.50)
#: and is retracted. This list is the Google-services-adjacent subset of
#: Playwright's defaults, suppressed together; the supporting A/B for the list
#: itself (3/3) came from a measurement window that overstated the overall effect
#: ~5x, so treat the list as a considered default rather than a verified remedy,
#: and re-run a paired A/B before quoting it as a fix.
#:
#: Callers who pass ``ignore_default_args`` themselves keep full control.
SUPPRESSED_DEFAULT_ARGS: tuple[str, ...] = (
    "--disable-field-trial-config",
    "--disable-component-update",
    "--disable-client-side-phishing-detection",
    "--metrics-recording-only",
    "--disable-breakpad",
    "--no-service-autorun",
    "--disable-extensions",
    "--disable-default-apps",
    "--disable-component-extensions-with-background-pages",
    "--disable-search-engine-choice-screen",
    "--no-default-browser-check",
    "--unsafely-disable-devtools-self-xss-warnings",
    "--use-mock-keychain",
)

_locks: dict[tuple[Any, str], threading.Lock] = {}
_locks_guard = threading.Lock()


class ProxySpec(NamedTuple):
    """A proxy split into the parts each command is allowed to see."""

    server: str
    username: str | None = None
    password: str | None = None

    @property
    def authenticated(self) -> bool:
        return bool(self.username) and self.password is not None


#: Ports a scheme omits from its canonical authority, matching WHATWG URL.
_DEFAULT_PORTS = {"http": 80, "https": 443, "ws": 80, "wss": 443}


def _serialize_ipv6(address: ipaddress.IPv6Address) -> str:
    """WHATWG's IPv6 serializer, which is NOT ``IPv6Address.compressed``.

    ``compressed`` is not stable across CPython releases: 3.12.3 renders
    ``::ffff:192.168.0.1`` as ``::ffff:c0a8:1``, while 3.12.13 renders it back as
    the dotted-quad. WHATWG (and therefore the browser, and the driver that
    re-normalizes what we register) never uses the dotted-quad form, so on the
    newer interpreter this client registered a server string the context would
    not match and the registration was never consumed — a proxy that silently
    does nothing, appearing on a patch upgrade of Python.

    This is the algorithm from the URL Standard, "IPv6 serializer": the first
    longest run of two or more zero pieces becomes ``::``, everything else is
    lowercase hex.
    """
    pieces = [int.from_bytes(address.packed[i:i + 2], "big") for i in range(0, 16, 2)]

    compress, best_run = None, 1          # a run of one zero is not compressed
    run_start, run_length = None, 0
    for index, piece in enumerate((*pieces, None)):
        if piece == 0:
            run_start = index if run_start is None else run_start
            run_length += 1
            continue
        if run_length > best_run:
            compress, best_run = run_start, run_length
        run_start, run_length = None, 0

    out, ignore_zeros = "", False
    for index, piece in enumerate(pieces):
        if ignore_zeros:
            if piece == 0:
                continue
            ignore_zeros = False
        if index == compress:
            out += "::" if index == 0 else ":"
            ignore_zeros = True
            continue
        out += format(piece, "x")
        if index != 7:
            out += ":"
    return out


def _canonical_host(host: str) -> str:
    """The host as a WHATWG URL parser would render it.

    ``urlsplit().hostname`` already lowercases and unbrackets, but stops there;
    the parser Playwright uses also percent-decodes the host, compresses an IPv6
    literal, and punycodes a non-ASCII label. Skipping those leaves two spellings
    of one host, and the registration is matched by bytes.
    """
    host = unquote(host)
    if ":" in host:                       # IPv6 — hostname stripped the brackets
        try:
            return f"[{_serialize_ipv6(ipaddress.IPv6Address(host))}]"
        except ValueError:
            return f"[{host}]"
    if any(ord(c) > 127 for c in host):
        try:
            # Best effort: the stdlib codec is IDNA2003 where WHATWG specifies
            # UTS-46. They agree on the host names proxy gateways actually use,
            # and leaving the label undecoded is never the better failure.
            host = host.encode("idna").decode("ascii")
        except UnicodeError:
            pass
    return host.lower()


def normalize_server(server: str) -> str:
    """Canonicalise a proxy server the way Playwright will.

    Byte-identity between the two commands is required, but only ONE of them is
    ours: Playwright rewrites the server it sends to createBrowserContext with
    ``url.protocol + "//" + url.host`` (browserContext.js normalizeProxySettings),
    which lowercases the host, drops a default port, and prepends ``http://``
    when the scheme is missing. Registering the raw string would then not match
    the context's, and the registration would never be consumed. So we normalise
    first and use the result for both.

    Raises ``ValueError`` on an unparseable port. That is deliberate: a proxy
    string with a stray character or trailing space used to lose its port here
    and come back pointing at :80, which is both wrong and silent.
    """
    raw = str(server).strip()
    parsed = urlsplit(raw)
    if not parsed.scheme or not parsed.netloc:
        parsed = urlsplit("http://" + raw)
    scheme = (parsed.scheme or "http").lower()
    port = parsed.port                    # ValueError on a bad port — let it out
    host = _canonical_host(parsed.hostname or "")
    if port is not None and port != _DEFAULT_PORTS.get(scheme):
        host = f"{host}:{port}"
    return f"{scheme}://{host}"


def parse_proxy(proxy: str | dict[str, Any] | ProxySpec) -> ProxySpec:
    """Accept a proxy URL, a Playwright-style dict, or a ProxySpec."""
    if isinstance(proxy, ProxySpec):
        return ProxySpec(normalize_server(proxy.server),
                         proxy.username, proxy.password)
    if isinstance(proxy, dict):
        server = proxy.get("server") or ""
        if not server:
            raise ValueError("proxy dict requires a 'server' key")
        username, password = proxy.get("username"), proxy.get("password")
        if "@" in server and username is None and password is None:
            return parse_proxy(server)
        # Explicit credentials are literal — never percent-decoded.
        return ProxySpec(normalize_server(server), username, password)

    url = proxy
    server, username, password = url, None, None
    if "@" in url:
        scheme = url.find("://")
        # A scheme-less URL starts its credentials at 0; the naive find()+3
        # yields 2 and silently truncates the username.
        start = scheme + 3 if scheme != -1 else 0
        at = url.rfind("@")
        creds = url[start:at]
        if ":" in creds:
            username, password = creds.split(":", 1)
        else:
            username = creds or None
        # Credentials inside a URL are percent-encoded by definition — a
        # password containing '@' or ':' has to be. Sending them still encoded
        # authenticates with the wrong secret, and fails silently.
        username = unquote(username) if username is not None else None
        password = unquote(password) if password is not None else None
        server = url[:start] + url[at + 1:]
    return ProxySpec(normalize_server(server), username, password)


#: The registration command. Paired with :func:`credentials_params`.
CREDENTIALS_METHOD = "Target.setProxyCredentials"


def credentials_params(spec: ProxySpec) -> dict[str, str]:
    """Params for ``Target.setProxyCredentials``."""
    return {
        "proxyServer": spec.server,
        "username": spec.username or "",
        "password": spec.password or "",
    }


def check_registration(result: Any) -> None:
    """Raise if the browser refused the registration.

    Playwright returns ``{}``; a raw WebSocket returns the CDP envelope.
    """
    if isinstance(result, dict) and result.get("error"):
        raise RuntimeError(f"proxy credential registration refused: {result!r}")


@contextmanager
def proxy_registration(
    connection: Any,
    proxy: str | dict[str, Any] | ProxySpec,
) -> Iterator[ProxySpec]:
    """Hold the registration slot for ``proxy`` on ``connection``.

    Yields the validated :class:`ProxySpec`. Send the credentials AND create the
    context inside the ``with`` block — the lock must span both, because the
    registration is consumed by the matching createBrowserContext.

        with proxy_registration(browser, proxy) as spec:
            check_registration(send(CREDENTIALS_METHOD, credentials_params(spec)))
            context = make_the_context(spec.server)

    Sync and async callers use this identically; the core never awaits.
    ``connection`` only identifies the CDP connection for locking.
    """
    spec = parse_proxy(proxy)
    if not spec.authenticated:
        raise ValueError(
            "per-context preregistration needs a username and password; an "
            "unauthenticated proxy can be passed straight to the driver"
        )
    if not spec.server.startswith(("http://", "https://")):
        raise ValueError(
            f"credential preregistration requires an HTTP(S) proxy, got {spec.server!r}"
        )

    # Keyed by id() rather than the object: holding the browser would leak it.
    # If an id is reused after GC the worst case is two unrelated connections
    # sharing a lock — slower, never incorrect.
    key = (id(connection), spec.server)
    with _locks_guard:
        lock = _locks.setdefault(key, threading.Lock())

    lock.acquire()
    try:
        yield spec
    finally:
        lock.release()


# --- Captcha solver -------------------------------------------------------
#
# Chromeleon ships a built-in reCAPTCHA/hCaptcha solver. Once enabled at launch
# it works AUTOMATICALLY — it detects the widget, solves it (audio first, image
# fallback), and writes the token; you do not call it. The release binary embeds
# the models, so ``--captcha-solver`` alone is enough.
#
# The ``Chromeleon`` CDP domain only OBSERVES and STEERS that solver: enable it
# on a page CDP session to receive lifecycle events, and use ``solverEval`` to
# run JS in the solver's isolated world (world 10), which pierces CLOSED shadow
# roots. Like the proxy handshake, these are protocol facts — the constants and
# param builders live here; the one-line driver calls live in ``adapters``.

#: Launch switch that turns the solver on.
CAPTCHA_SOLVER_SWITCH = "--captcha-solver"
#: Launch switch overriding the model directory. Dev/self-host only — the
#: release binary embeds the models, so you normally omit it.
CAPTCHA_MODEL_PATH_SWITCH = "--captcha-model-path"

#: ``Chromeleon`` domain commands, sent on a page CDP session.
ENABLE_METHOD = "Chromeleon.enable"
DISABLE_METHOD = "Chromeleon.disable"
SOLVER_EVAL_METHOD = "Chromeleon.solverEval"

#: ``Chromeleon`` domain events. Subscribe with the driver's ``cdp.on(name, cb)``.
CAPTCHA_DETECTED = "Chromeleon.captchaDetected"    # {sitekey}
CAPTCHA_SOLVING = "Chromeleon.captchaSolving"      # {sitekey, method: audio|image}
CAPTCHA_SOLVED = "Chromeleon.captchaSolved"        # {sitekey, attempts, timeMs}
CAPTCHA_FAILED = "Chromeleon.captchaFailed"        # {sitekey, attempts, reason}
SOLVER_EVAL_RESULT = "Chromeleon.solverEvalResult"  # {result}

#: The four captcha lifecycle events, detected -> solving -> solved | failed.
CAPTCHA_EVENTS = (CAPTCHA_DETECTED, CAPTCHA_SOLVING, CAPTCHA_SOLVED, CAPTCHA_FAILED)


def captcha_launch_args(model_path: str | None = None) -> tuple[str, ...]:
    """Launch flags that turn on the built-in captcha solver.

    The release binary embeds the models, so this is just ``--captcha-solver``.
    ``model_path`` is a dev/self-host override that also appends
    ``--captcha-model-path=<dir>``.
    """
    args = [CAPTCHA_SOLVER_SWITCH]
    if model_path is not None:
        args.append(f"{CAPTCHA_MODEL_PATH_SWITCH}={model_path}")
    return tuple(args)


def solver_eval_params(expression: str, frame_url_contains: str = "") -> dict[str, str]:
    """Params for ``Chromeleon.solverEval``.

    ``expression`` runs in the solver's isolated world (pierces CLOSED shadow
    roots). ``frame_url_contains`` selects a subframe whose committed URL
    contains that substring (for cross-origin OOPIFs); the empty string targets
    the primary main frame. The string result arrives as a ``solverEvalResult``
    event, not as the command return.
    """
    return {"expression": expression, "frameUrlContains": frame_url_contains}
