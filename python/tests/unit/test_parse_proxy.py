"""
Unit tests for parse_proxy() — the proxy URL parser used in examples.

These verify the fix for schemeless proxy URLs (no http:// prefix),
which previously caused incorrect parsing due to find("://") returning -1
and the +3 offset producing position 2 instead of 0.

Both examples now delegate to chromeleon, so the returned SERVER is
canonicalised the way Playwright will canonicalise it before sending
Target.createBrowserContext (`url.protocol + "//" + url.host`): the scheme is
added when missing, the host is lowercased, and a default port is dropped. The
two CDP commands must carry a byte-identical string, and only one of them is
ours — registering the raw spelling would simply never be consumed.
"""
import pytest
import sys
from pathlib import Path

# Import parse_proxy from both example files
REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "examples"))

from per_context_proxy_example import parse_proxy as parse_proxy_async
from per_context_proxy_example_sync import parse_proxy as parse_proxy_sync


# Run every test case against both async and sync versions
@pytest.fixture(params=[parse_proxy_async, parse_proxy_sync],
                ids=["async", "sync"])
def parse_proxy(request):
    return request.param


# ── Schemed URLs (the happy path that always worked) ─────────────────────

def test_http_scheme_with_creds(parse_proxy):
    server, user, pwd = parse_proxy("http://myuser:mypass@proxy.example.com:8080")
    assert server == "http://proxy.example.com:8080"
    assert user == "myuser"
    assert pwd == "mypass"


def test_https_scheme_with_creds(parse_proxy):
    server, user, pwd = parse_proxy("https://user:pass@host:443")
    assert server == "https://host"   # 443 is https's default and is dropped
    assert user == "user"
    assert pwd == "pass"


def test_socks5_scheme_with_creds(parse_proxy):
    server, user, pwd = parse_proxy("socks5://admin:secret@socks.example.com:1080")
    assert server == "socks5://socks.example.com:1080"
    assert user == "admin"
    assert pwd == "secret"


def test_http_scheme_no_creds(parse_proxy):
    server, user, pwd = parse_proxy("http://proxy.example.com:8080")
    assert server == "http://proxy.example.com:8080"
    assert user is None
    assert pwd is None


# ── Schemeless URLs (the bug that was fixed) ─────────────────────────────

def test_schemeless_with_creds(parse_proxy):
    """Core regression test: schemeless URL with user:pass@host:port."""
    server, user, pwd = parse_proxy("myuser:mypass@proxy.example.com:8080")
    assert server == "http://proxy.example.com:8080"
    assert user == "myuser"
    assert pwd == "mypass"


def test_schemeless_no_creds(parse_proxy):
    """Schemeless URL without credentials (just host:port)."""
    server, user, pwd = parse_proxy("proxy.example.com:8080")
    assert server == "http://proxy.example.com:8080"
    assert user is None
    assert pwd is None


def test_schemeless_with_country_suffix(parse_proxy):
    """IPRoyal-style password with _country- suffix, no scheme."""
    server, user, pwd = parse_proxy("myuser:mypass_country-us@geo.iproyal.com:12321")
    assert server == "http://geo.iproyal.com:12321"
    assert user == "myuser"
    assert pwd == "mypass_country-us"


# ── Passwords containing special characters ──────────────────────────────

def test_password_with_colon(parse_proxy):
    """Password containing a colon (split on first : only)."""
    server, user, pwd = parse_proxy("http://user:pass:word@host:9090")
    assert server == "http://host:9090"
    assert user == "user"
    assert pwd == "pass:word"


def test_password_with_at_sign(parse_proxy):
    """Password containing @ — rfind('@') picks the last one."""
    server, user, pwd = parse_proxy("http://user:p@ss@host:9090")
    assert server == "http://host:9090"
    assert user == "user"
    assert pwd == "p@ss"


def test_schemeless_password_with_colon(parse_proxy):
    """Schemeless URL with colon in password."""
    server, user, pwd = parse_proxy("user:complex:pass@host:3128")
    assert server == "http://host:3128"
    assert user == "user"
    assert pwd == "complex:pass"


# ── Edge cases ───────────────────────────────────────────────────────────

def test_bare_hostname(parse_proxy):
    """Just a hostname, no port, no creds."""
    server, user, pwd = parse_proxy("proxy.example.com")
    assert server == "http://proxy.example.com"
    assert user is None
    assert pwd is None


def test_ipv4_with_port(parse_proxy):
    server, user, pwd = parse_proxy("http://user:pass@192.168.1.1:8080")
    assert server == "http://192.168.1.1:8080"
    assert user == "user"
    assert pwd == "pass"


def test_schemeless_ipv4(parse_proxy):
    server, user, pwd = parse_proxy("user:pass@192.168.1.1:8080")
    assert server == "http://192.168.1.1:8080"
    assert user == "user"
    assert pwd == "pass"
