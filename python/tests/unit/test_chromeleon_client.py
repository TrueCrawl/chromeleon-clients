"""Contracts for chromeleon — the per-context proxy handshake.

The behaviour under test is the one that fails silently when hand-rolled: the
proxyServer string must match byte-for-byte across both CDP commands,
credentials must never reach the context, and the register-then-create pair must
be serialized because the registration is single-use with no correlation token.

No Playwright required — the drivers here are recording stubs. One of them is
deliberately NOT Playwright-shaped, to prove the core is transport-agnostic.
"""
import threading

import pytest

from chromeleon import (
    CREDENTIALS_METHOD,
    LAUNCH_ARGS,
    ProxySpec,
    browser_process_env,
    check_registration,
    credentials_params,
    launch,
    new_proxy_context,
    normalize_server,
    parse_proxy,
    proxy_registration,
)


# ── parsing ──────────────────────────────────────────────────────────────────

@pytest.mark.parametrize("url,expected", [
    ("http://user:pass@host:12321", ProxySpec("http://host:12321", "user", "pass")),
    ("https://user:pass@host:443", ProxySpec("https://host", "user", "pass")),
    # Scheme-less: a naive find("://")+3 yields 2 and truncates the username.
    ("user:pass@host:12321", ProxySpec("http://host:12321", "user", "pass")),
    ("http://host:12321", ProxySpec("http://host:12321", None, None)),
    # Passwords containing '@' and ':' must survive: rfind, then split once.
    ("http://user:p@ss:word@host:1", ProxySpec("http://host:1", "user", "p@ss:word")),
])
def test_parse_proxy_urls(url, expected):
    assert parse_proxy(url) == expected


def test_parse_proxy_accepts_playwright_dict():
    assert parse_proxy({"server": "http://h:1", "username": "u", "password": "p"}) \
        == ProxySpec("http://h:1", "u", "p")


def test_parse_proxy_dict_with_credentials_in_server():
    assert parse_proxy({"server": "http://u:p@h:1"}) == ProxySpec("http://h:1", "u", "p")


# ── server normalization ─────────────────────────────────────────────────────
# Only ONE of the two commands is ours. Playwright rewrites the server it sends
# to createBrowserContext (browserContext.js normalizeProxySettings:
# `url.protocol + "//" + url.host`). If we register the raw string it will not
# match the context's and the registration is never consumed.

@pytest.mark.parametrize("raw,normalized", [
    ("http://GATEWAY:12321", "http://gateway:12321"),   # host lowercased
    ("http://gateway:80", "http://gateway"),            # default port dropped
    ("https://gateway:443", "https://gateway"),         # ditto for https
    ("https://gateway:8443", "https://gateway:8443"),   # non-default kept
    ("gateway:12321", "http://gateway:12321"),          # scheme-less gets http://
    ("http://[2001:db8::1]:9000", "http://[2001:db8::1]:9000"),  # IPv6 brackets kept
    ("HTTP://Up.Case:8080", "http://up.case:8080"),
    # Surrounding whitespace is stripped, NOT read as part of the port. Losing
    # the port here silently re-pointed the proxy at :80.
    ("http://gateway:12321 ", "http://gateway:12321"),
    (" http://gateway:12321", "http://gateway:12321"),
    # The WHATWG host rules Playwright's parser applies and urlsplit does not.
    ("http://%67ateway:12321", "http://gateway:12321"),          # percent-decoded
    ("http://[0:0:0:0:0:0:0:1]:8080", "http://[::1]:8080"),      # IPv6 compressed
    ("http://münchen.example:12321",
     "http://xn--mnchen-3ya.example:12321"),                     # IDN punycoded
])
def test_normalize_server_matches_playwright(raw, normalized):
    assert normalize_server(raw) == normalized
    assert parse_proxy(raw).server == normalized


@pytest.mark.parametrize("raw", ["http://gateway:1x321", "http://gateway:99999"])
def test_normalize_server_refuses_an_unparseable_port(raw):
    """Loud beats silent: this used to return the host on the scheme default port."""
    with pytest.raises(ValueError):
        normalize_server(raw)


def test_registered_server_is_what_playwright_will_send():
    """The end-to-end invariant: both commands carry the same normalized string."""
    browser = _Browser()
    new_proxy_context(browser, "http://user:pass@GATEWAY:80")
    registered = browser.log[1][2]["proxyServer"]
    created = browser.log[3][1]["proxy"]["server"]
    assert registered == created == "http://gateway"


@pytest.mark.parametrize("url,password", [
    ("http://user:p%40ss@host:1", "p@ss"),          # encoded '@'
    ("http://user:p%3Aword@host:1", "p:word"),      # encoded ':'
    ("http://user:p%2Fs%23h@host:1", "p/s#h"),      # encoded '/' and '#'
    ("http://user:p@ss:word@host:1", "p@ss:word"),  # literal, rfind still wins
])
def test_url_credentials_are_percent_decoded(url, password):
    """Credentials in a URL are encoded by definition; sending them still
    encoded authenticates with the wrong secret, and fails silently."""
    assert parse_proxy(url).password == password


def test_explicit_dict_credentials_are_taken_literally():
    """A dict is not a URL — a literal '%40' in a password must survive."""
    spec = parse_proxy({"server": "http://h:1", "username": "u", "password": "p%40ss"})
    assert spec.password == "p%40ss"


def test_launch_args_is_the_webrtc_policy():
    assert LAUNCH_ARGS == ("--webrtc-ip-handling-policy=disable_non_proxied_udp",)


# ── launch ───────────────────────────────────────────────────────────────────

class _Chromium:
    def __init__(self):
        self.kw = None

    def launch(self, **kwargs):
        self.kw = kwargs
        return "browser"


def test_launch_supplies_the_webrtc_policy_and_scrubs_proxy_env(monkeypatch):
    monkeypatch.setenv("HTTPS_PROXY", "http://controller:8080")
    monkeypatch.setenv("KEEP_ME", "yes")
    chromium = _Chromium()
    assert launch(chromium, "/opt/chromeleon/chrome", headless=True) == "browser"
    assert chromium.kw["args"] == list(LAUNCH_ARGS)
    assert chromium.kw["executable_path"] == "/opt/chromeleon/chrome"
    assert chromium.kw["headless"] is True          # passed straight through
    assert not any("PROXY" in k for k in chromium.kw["env"])
    assert chromium.kw["env"]["KEEP_ME"] == "yes"


def test_launch_does_not_override_an_explicit_policy():
    chromium = _Chromium()
    launch(chromium, "/x", args=["--webrtc-ip-handling-policy=default"])
    assert chromium.kw["args"] == ["--webrtc-ip-handling-policy=default"]


def test_launch_keeps_caller_args():
    chromium = _Chromium()
    launch(chromium, "/x", args=["--fingerprint-os=Windows"])
    assert chromium.kw["args"] == ["--fingerprint-os=Windows", *LAUNCH_ARGS]


def test_browser_process_env_strips_every_proxy_variable():
    assert browser_process_env({"http_proxy": "a", "MY_PROXY_URL": "b", "OK": "c"}) \
        == {"OK": "c"}


# ── core validation ──────────────────────────────────────────────────────────

def test_unauthenticated_proxy_is_rejected_with_guidance():
    with pytest.raises(ValueError, match="username and password"):
        with proxy_registration(object(), "http://host:1"):
            pass


def test_socks_proxy_is_rejected():
    with pytest.raises(ValueError, match="HTTP\\(S\\)"):
        with proxy_registration(object(), "socks5://u:p@host:1"):
            pass


def test_credentials_params_shape():
    spec = parse_proxy("http://u:p@h:1")
    assert credentials_params(spec) == {
        "proxyServer": "http://h:1", "username": "u", "password": "p"}


def test_check_registration_raises_on_error_envelope():
    check_registration({})          # Playwright's empty ack
    check_registration(None)        # a driver that returns nothing
    with pytest.raises(RuntimeError, match="refused"):
        check_registration({"error": {"code": -32000, "message": "already registered"}})


# ── Playwright adapter ───────────────────────────────────────────────────────

class _CDP:
    def __init__(self, log, result=None):
        self._log, self._result = log, {} if result is None else result
        self.detached = False

    def send(self, method, params):
        self._log.append(("send", method, params))
        return self._result

    def detach(self):
        self.detached = True
        self._log.append(("detach",))


class _Browser:
    def __init__(self, result=None):
        self.log, self.cdp, self._result = [], None, result

    def new_browser_cdp_session(self):
        self.cdp = _CDP(self.log, self._result)
        self.log.append(("cdp_session",))
        return self.cdp

    def new_context(self, **kwargs):
        self.log.append(("new_context", kwargs))
        return "context"


def test_handshake_order_and_payload():
    browser = _Browser()
    assert new_proxy_context(browser, "http://user:pass@host:12321",
                             viewport={"width": 800, "height": 600}) == "context"
    assert [e[0] for e in browser.log] == \
        ["cdp_session", "send", "detach", "new_context"]

    _, method, params = browser.log[1]
    assert method == CREDENTIALS_METHOD == "Target.setProxyCredentials"
    assert params == {"proxyServer": "http://host:12321",
                      "username": "user", "password": "pass"}

    _, ctx = browser.log[3]
    # Same server string in both calls; credentials ONLY in the registration.
    assert ctx["proxy"] == {"server": "http://host:12321"}
    assert ctx["viewport"] == {"width": 800, "height": 600}
    assert browser.cdp.detached


def test_refused_registration_still_detaches_and_creates_nothing():
    browser = _Browser(result={"error": {"message": "already registered"}})
    with pytest.raises(RuntimeError, match="refused"):
        new_proxy_context(browser, "http://u:p@host:1")
    assert browser.cdp.detached
    assert not any(e[0] == "new_context" for e in browser.log)


def test_register_then_create_is_serialized_per_connection_and_server():
    """Concurrent callers must not interleave: the registration is single-use
    and carries no correlation token."""
    browser = _Browser()
    original = browser.new_context

    def slow_new_context(**kwargs):
        import time
        time.sleep(0.05)
        return original(**kwargs)

    browser.new_context = slow_new_context
    threads = [threading.Thread(target=new_proxy_context,
                                args=(browser, "http://u:p@host:1"))
               for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    kinds = [e[0] for e in browser.log]
    sends = [i for i, k in enumerate(kinds) if k == "send"]
    creates = [i for i, k in enumerate(kinds) if k == "new_context"]
    assert len(sends) == len(creates) == 4
    # Every registration is consumed by its own create before the next one.
    for a, b in zip(sends, creates):
        assert a < b
    for earlier, later in zip(creates, sends[1:]):
        assert earlier < later


def test_different_servers_do_not_block_each_other():
    browser = _Browser()
    new_proxy_context(browser, "http://u:p@a:1")
    new_proxy_context(browser, "http://u:p@b:2")
    assert [e[2]["proxyServer"] for e in browser.log if e[0] == "send"] == \
        ["http://a:1", "http://b:2"]


# ── transport-agnostic core ──────────────────────────────────────────────────

def test_core_drives_a_non_playwright_transport():
    """The core must serve any driver — Selenium, a raw DevTools WebSocket, or a
    language with no Playwright binding. Nothing here is Playwright-shaped."""
    sent = []

    def send(method, params):                 # e.g. driver.execute_cdp_cmd
        sent.append((method, params))
        return {"id": len(sent), "result": {}}

    with proxy_registration("ws://127.0.0.1:9222/devtools/browser/abc",
                            "http://u:p@gw:12321") as spec:
        check_registration(send(CREDENTIALS_METHOD, credentials_params(spec)))
        check_registration(send("Target.createBrowserContext",
                                {"proxyServer": spec.server}))

    assert sent == [
        ("Target.setProxyCredentials",
         {"proxyServer": "http://gw:12321", "username": "u", "password": "p"}),
        ("Target.createBrowserContext", {"proxyServer": "http://gw:12321"}),
    ]
    # identical server string in both, credentials only in the first
    assert sent[0][1]["proxyServer"] == sent[1][1]["proxyServer"]
    assert "username" not in sent[1][1]


def test_core_lock_is_released_when_the_body_raises():
    conn = object()
    with pytest.raises(ZeroDivisionError):
        with proxy_registration(conn, "http://u:p@h:1"):
            1 / 0
    # A leaked lock would deadlock this second acquisition.
    with proxy_registration(conn, "http://u:p@h:1") as spec:
        assert spec.server == "http://h:1"
