"""Contracts for chromeleon.perf — the launch-latency helpers.

sticky_geo_env: pre-supply a STICKY proxy's exit IP + timezone so the browser
skips its own through-proxy geo round trip. BrowserPool: reuse warm
connect_over_cdp connections. Both use an injected resolver/connector and a fake
clock, so no network or live browser is required (mirrors the transport-agnostic
stubs in test_chromeleon.py).
"""
import pytest

from chromeleon.perf import BrowserPool, clear_geo_cache, sticky_geo_env


@pytest.fixture(autouse=True)
def _clean_cache():
    clear_geo_cache()
    yield
    clear_geo_cache()


# --- sticky_geo_env ----------------------------------------------------------

def test_not_sticky_is_noop():
    # A rotating proxy must NOT get a pinned IP/timezone (stale-TZ tell).
    calls = []
    def resolver(proxy, timeout):
        calls.append(proxy)
        return "1.2.3.4", "Asia/Shanghai"
    assert sticky_geo_env("http://u:p@host:1", resolver=resolver) == {}
    assert calls == []  # never even resolved


def test_sticky_resolves_and_pins_env():
    env = sticky_geo_env(
        "http://u:p@host:1", sticky=True,
        resolver=lambda proxy, timeout: ("36.42.237.31", "Asia/Shanghai"),
    )
    assert env == {
        "CHROMELEON_EXIT_IP": "36.42.237.31",
        "CHROMELEON_TARGET_TZ": "Asia/Shanghai",
    }


def test_cache_resolves_once_within_ttl():
    n = {"c": 0}
    def resolver(proxy, timeout):
        n["c"] += 1
        return "1.2.3.4", "Europe/Paris"
    clock = {"t": 100.0}
    for _ in range(5):
        env = sticky_geo_env("http://host:1", sticky=True, ttl=600,
                             resolver=resolver, now=lambda: clock["t"])
        assert env["CHROMELEON_EXIT_IP"] == "1.2.3.4"
    assert n["c"] == 1  # one network round trip, then cache


def test_cache_expires_after_ttl():
    n = {"c": 0}
    def resolver(proxy, timeout):
        n["c"] += 1
        return f"10.0.0.{n['c']}", "Europe/Paris"
    clock = {"t": 0.0}
    e1 = sticky_geo_env("http://host:1", sticky=True, ttl=100,
                        resolver=resolver, now=lambda: clock["t"])
    clock["t"] = 150.0  # past ttl
    e2 = sticky_geo_env("http://host:1", sticky=True, ttl=100,
                        resolver=resolver, now=lambda: clock["t"])
    assert n["c"] == 2
    assert e1["CHROMELEON_EXIT_IP"] != e2["CHROMELEON_EXIT_IP"]


def test_distinct_proxies_cached_separately():
    def resolver(proxy, timeout):
        return ("1.1.1.1", "Europe/London") if "a" in str(proxy) else ("2.2.2.2", "America/New_York")
    ea = sticky_geo_env("http://a:1", sticky=True, resolver=resolver)
    eb = sticky_geo_env("http://b:1", sticky=True, resolver=resolver)
    assert ea["CHROMELEON_EXIT_IP"] == "1.1.1.1"
    assert eb["CHROMELEON_EXIT_IP"] == "2.2.2.2"


@pytest.mark.parametrize("bad_tz", ["", "UTC", "Not A Zone", "A" * 80 + "/x", "Asia/Sh anghai"])
def test_invalid_timezone_dropped_ip_kept(bad_tz):
    env = sticky_geo_env("http://host:1", sticky=True,
                         resolver=lambda proxy, timeout: ("5.6.7.8", bad_tz))
    assert env["CHROMELEON_EXIT_IP"] == "5.6.7.8"
    assert "CHROMELEON_TARGET_TZ" not in env  # never forward a malformed TZ


def test_resolver_exception_is_noop_not_launch_failure():
    def boom(proxy, timeout):
        raise RuntimeError("proxy down")
    assert sticky_geo_env("http://host:1", sticky=True, resolver=boom) == {}


def test_empty_ip_is_noop():
    env = sticky_geo_env("http://host:1", sticky=True,
                         resolver=lambda proxy, timeout: ("", "Europe/Paris"))
    assert env == {}


# --- BrowserPool -------------------------------------------------------------

class _FakeBrowser:
    def __init__(self, ep):
        self.ep = ep
        self.closed = False
    def close(self):
        self.closed = True


def test_pool_requires_endpoints():
    with pytest.raises(ValueError):
        BrowserPool([], connector=lambda ep: _FakeBrowser(ep))


def test_pool_round_robin_and_reuse():
    made = []
    def connector(ep):
        made.append(ep)
        return _FakeBrowser(ep)
    pool = BrowserPool(["e1", "e2"], connector=connector)
    b1 = pool.acquire_sync()   # e1
    b2 = pool.acquire_sync()   # e2
    b3 = pool.acquire_sync()   # e1 again — reused, not reconnected
    assert (b1.ep, b2.ep, b3.ep) == ("e1", "e2", "e1")
    assert b1 is b3                    # same warm connection
    assert made == ["e1", "e2"]        # connected once per endpoint
    assert pool.warm_count() == 2


def test_pool_close_sync_closes_all():
    pool = BrowserPool(["e1", "e2"], connector=lambda ep: _FakeBrowser(ep))
    a = pool.acquire_sync()
    b = pool.acquire_sync()
    pool.close_sync()
    assert a.closed and b.closed
    assert pool.warm_count() == 0


def test_pool_async_acquire_reuse():
    # asyncio.run keeps this portable without a pytest-asyncio plugin.
    async def scenario():
        made = []
        async def connector(ep):
            made.append(ep)
            return _FakeBrowser(ep)
        pool = BrowserPool(["e1"], connector=connector)
        b1 = await pool.acquire()
        b2 = await pool.acquire()
        assert b1 is b2
        assert made == ["e1"]
        await pool.aclose()
        assert b1.closed
    import asyncio
    asyncio.run(scenario())
