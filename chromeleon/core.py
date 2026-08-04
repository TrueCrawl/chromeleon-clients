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

import threading
from contextlib import contextmanager
from urllib.parse import unquote, urlsplit
from typing import Any, Callable, Iterator, NamedTuple

__all__ = [
    "LAUNCH_ARGS",
    "ProxySpec",
    "normalize_server",
    "parse_proxy",
    "proxy_registration",
]

#: Launch flags a per-context proxy needs. A context-level proxy routes HTTP,
#: but WebRTC's UDP socket is browser-level and would otherwise gather ICE over
#: the real route. Chromeleon auto-appends this for LAUNCH-time proxies only,
#: which per-context proxies by definition are not.
LAUNCH_ARGS: tuple[str, ...] = (
    "--webrtc-ip-handling-policy=disable_non_proxied_udp",
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


def normalize_server(server: str) -> str:
    """Canonicalise a proxy server the way Playwright will.

    Byte-identity between the two commands is required, but only ONE of them is
    ours: Playwright rewrites the server it sends to createBrowserContext with
    ``url.protocol + "//" + url.host`` (browserContext.js normalizeProxySettings),
    which lowercases the host, drops a default port, and prepends ``http://``
    when the scheme is missing. Registering the raw string would then not match
    the context's, and the registration would never be consumed. So we normalise
    first and use the result for both.
    """
    parsed = urlsplit(server)
    if not parsed.scheme or not parsed.netloc:
        parsed = urlsplit("http://" + server)
    scheme = (parsed.scheme or "http").lower()
    host = (parsed.hostname or "").lower()
    if ":" in host:                       # IPv6 — hostname strips the brackets
        host = f"[{host}]"
    try:
        port = parsed.port
    except ValueError:
        port = None
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
