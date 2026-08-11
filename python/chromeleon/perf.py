"""Launch-latency helpers: pre-supply proxy geo, and pool warm browsers.

Two independent, opt-in optimizations for the Chromeleon launch tax:

* :func:`sticky_geo_env` — resolve a STICKY proxy's exit IP + IANA timezone
  ONCE and hand them to the browser via ``CHROMELEON_EXIT_IP`` /
  ``CHROMELEON_TARGET_TZ`` (both honored by the binary's launcher), so the
  browser skips its own through-proxy geo round trip on every launch. This is
  ONLY valid for a proxy whose exit IP is stable for the session
  (sticky/pinned). A ROTATING proxy would pin a stale IP/timezone that no
  longer matches the real exit on the next launch — a fingerprint tell — so the
  helper refuses unless the caller asserts ``sticky=True``.

* :class:`BrowserPool` — keep N warm ``connect_over_cdp`` connections to
  already-running Chromeleon endpoints and hand them out, so throughput
  workloads pay the launch tax zero times per task. This is the recommended
  launch-once-connect-many pattern (see examples/connect_over_cdp_example.py),
  made reusable with round-robin dispatch and lazy, cached connections.

Both are transport-only: they change WHEN/whether work happens, never the
persona the browser presents. ``sticky_geo_env`` only forwards the exit IP the
proxy already routes through; the binary derives locale/country from it exactly
as it would have.
"""

from __future__ import annotations

import json
import threading
import time
import urllib.parse
import urllib.request
from typing import Any, Callable, NamedTuple

__all__ = ["sticky_geo_env", "GeoResolution", "BrowserPool"]


class GeoResolution(NamedTuple):
    """A resolved proxy exit with a monotonic timestamp for TTL bookkeeping."""

    exit_ip: str
    timezone: str
    resolved_at: float


# --- #1: pre-supply the sticky proxy's exit IP + timezone --------------------

_geo_cache: dict[str, GeoResolution] = {}
_geo_cache_guard = threading.Lock()


def _proxy_parts(proxy: Any) -> tuple[str, str | None, str | None]:
    """Normalize a proxy (ProxySpec | dict | url str | (server,user,pass)) to
    (server, username, password) WITHOUT importing playwright."""
    # ProxySpec / any object exposing .server/.username/.password
    server = getattr(proxy, "server", None)
    if server is not None:
        return server, getattr(proxy, "username", None), getattr(proxy, "password", None)
    if isinstance(proxy, dict):
        return proxy.get("server", ""), proxy.get("username"), proxy.get("password")
    if isinstance(proxy, (tuple, list)):
        server = proxy[0] if len(proxy) > 0 else ""
        username = proxy[1] if len(proxy) > 1 else None
        password = proxy[2] if len(proxy) > 2 else None
        return server, username, password
    # bare URL string, possibly with embedded user:pass@
    from chromeleon.core import parse_proxy as _pp  # local import, no cycle at module load

    spec = _pp(proxy)
    return spec.server, spec.username, spec.password


def _is_valid_iana_tz(tz: str) -> bool:
    """The same shape gate the binary applies: 'Area/Location', no whitespace or
    control chars, bounded length. Prevents a malformed field becoming the TZ."""
    if not tz or "/" not in tz or len(tz) >= 64:
        return False
    return not any(c.isspace() or ord(c) < 0x20 for c in tz)


def _default_geo_resolver(proxy: Any, timeout: float) -> tuple[str, str]:
    """Query ipwho.is THROUGH the proxy — one round trip returns {ip,
    timezone.id, country}. Returns (exit_ip, timezone)."""
    server, username, password = _proxy_parts(proxy)
    # Strip any scheme; default to http CONNECT proxy for the urllib opener.
    host_port = server.split("://", 1)[-1]
    if username:
        creds = f"{urllib.parse.quote(username, safe='')}:{urllib.parse.quote(password or '', safe='')}@"
    else:
        creds = ""
    proxy_url = f"http://{creds}{host_port}"
    handler = urllib.request.ProxyHandler({"http": proxy_url, "https": proxy_url})
    opener = urllib.request.build_opener(handler)
    req = urllib.request.Request("https://ipwho.is/", headers={"User-Agent": "curl/8"})
    with opener.open(req, timeout=timeout) as resp:
        data = json.load(resp)
    exit_ip = str(data.get("ip") or "")
    tz = str(((data.get("timezone") or {}) or {}).get("id") or "")
    return exit_ip, tz


def sticky_geo_env(
    proxy: Any,
    *,
    sticky: bool = False,
    ttl: float = 600.0,
    timeout: float = 15.0,
    resolver: Callable[[Any, float], tuple[str, str]] | None = None,
    now: Callable[[], float] = time.monotonic,
) -> dict[str, str]:
    """Resolve a STICKY proxy's exit IP + timezone once and return the env dict
    (``CHROMELEON_EXIT_IP`` / ``CHROMELEON_TARGET_TZ``) to merge into the browser
    launch environment, so the binary skips its own through-proxy geo round trip.

    Merge into a launch, e.g.::

        from chromeleon import launch, browser_process_env
        from chromeleon.perf import sticky_geo_env

        env = {**browser_process_env(), **sticky_geo_env(proxy, sticky=True)}
        browser = launch(p.chromium, CHROMELEON, env=env,
                         args=[f"--proxy-server={proxy.server}"])

    :param sticky: MUST be True. A rotating proxy's exit changes between
        launches, so pinning a resolved IP/timezone would present a timezone
        that no longer matches the egress IP — a detection tell. When False the
        helper is a no-op and returns ``{}`` (the binary resolves geo itself).
    :param ttl: seconds a cached resolution stays valid (default 10 min). A
        sticky session that rotates on a fixed interval should set this below
        the rotation period.
    :param resolver: injectable ``(proxy, timeout) -> (exit_ip, timezone)`` for
        testing; defaults to a through-proxy ipwho.is query.
    """
    if not sticky:
        return {}
    server, _u, _p = _proxy_parts(proxy)
    key = server
    ts = now()
    with _geo_cache_guard:
        cached = _geo_cache.get(key)
        if cached is not None and (ts - cached.resolved_at) < ttl:
            return _env_from(cached)

    try:
        exit_ip, tz = (resolver or _default_geo_resolver)(proxy, timeout)
    except Exception:
        # Never fail the launch on a resolution miss — the binary falls back to
        # resolving geo itself, exactly as if this helper were not used.
        return {}
    if not exit_ip:
        return {}
    if not _is_valid_iana_tz(tz):
        tz = ""  # keep the IP pin; let the binary derive TZ if we could not.
    res = GeoResolution(exit_ip, tz, now())
    with _geo_cache_guard:
        _geo_cache[key] = res
    return _env_from(res)


def _env_from(res: GeoResolution) -> dict[str, str]:
    env = {"CHROMELEON_EXIT_IP": res.exit_ip}
    if res.timezone:
        env["CHROMELEON_TARGET_TZ"] = res.timezone
    return env


def clear_geo_cache() -> None:
    """Drop all cached proxy geo resolutions (e.g. on a known rotation)."""
    with _geo_cache_guard:
        _geo_cache.clear()


# --- #5: warm connect_over_cdp pool ------------------------------------------


class BrowserPool:
    """Round-robin pool of warm ``connect_over_cdp`` connections.

    Launch Chromeleon once per endpoint (``docker run ... --remote-debugging``)
    and connect many times. The pool caches one Playwright ``Browser`` per CDP
    endpoint and hands them out round-robin, so a throughput workload never pays
    the per-launch tax. Each task should create its OWN context
    (``browser.new_context(...)``) for isolation and close it when done; the
    shared ``Browser`` stays warm.

    Async usage::

        pool = BrowserPool(["http://c1:9222", "http://c2:9222"],
                           connector=p.chromium.connect_over_cdp)
        browser = await pool.acquire()               # warm, round-robin
        ctx = await browser.new_context()
        ...
        await ctx.close()                            # browser stays in the pool
        await pool.aclose()                          # closes all connections

    ``connector`` is ``playwright.chromium.connect_over_cdp`` (or a sync/async
    stand-in for testing). The pool never launches processes itself — endpoint
    lifecycle is the operator's.
    """

    def __init__(
        self,
        endpoints: list[str],
        *,
        connector: Callable[[str], Any],
    ) -> None:
        if not endpoints:
            raise ValueError("BrowserPool requires at least one CDP endpoint")
        self._endpoints = list(endpoints)
        self._connector = connector
        self._connections: dict[str, Any] = {}
        self._next = 0
        self._guard = threading.Lock()

    def _pick_endpoint(self) -> str:
        with self._guard:
            ep = self._endpoints[self._next % len(self._endpoints)]
            self._next += 1
            return ep

    async def acquire(self) -> Any:
        """Return a warm Browser for the next endpoint (async connector)."""
        ep = self._pick_endpoint()
        conn = self._connections.get(ep)
        if conn is None:
            conn = await self._connector(ep)
            self._connections[ep] = conn
        return conn

    def acquire_sync(self) -> Any:
        """Return a warm Browser for the next endpoint (sync connector)."""
        ep = self._pick_endpoint()
        conn = self._connections.get(ep)
        if conn is None:
            conn = self._connector(ep)
            self._connections[ep] = conn
        return conn

    def endpoints(self) -> tuple[str, ...]:
        return tuple(self._endpoints)

    def warm_count(self) -> int:
        """Number of endpoints with a live cached connection."""
        return len(self._connections)

    async def aclose(self) -> None:
        conns, self._connections = self._connections, {}
        for conn in conns.values():
            close = getattr(conn, "close", None)
            if close is None:
                continue
            result = close()
            if hasattr(result, "__await__"):
                await result

    def close_sync(self) -> None:
        conns, self._connections = self._connections, {}
        for conn in conns.values():
            close = getattr(conn, "close", None)
            if close is not None:
                close()
