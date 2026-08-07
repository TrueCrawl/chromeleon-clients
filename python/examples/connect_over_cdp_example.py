#!/usr/bin/env python3
"""Connect to a running Chromeleon instance via CDP remote debugging.

This example shows how to connect to a Chromeleon browser running in Docker
(or on another machine) via Playwright's connectOverCDP. This is the
recommended approach for production — launch the browser once, connect many
times.

Prerequisites:
    1. Chromeleon running with --remote-debugging-port=9222:
       docker run -d -p 9222:9222 chromeleon

       IMPORTANT: if you plan to use per-context proxies (--proxy below),
       the Chromeleon process MUST be launched with
           --webrtc-ip-handling-policy=disable_non_proxied_udp
       Context-level proxies (new_context(proxy=...)) route HTTP through the
       proxy but WebRTC's UDP socket uses the browser's default route,
       leaking the real public IP via ICE srflx candidates. The flag above
       suppresses unproxied UDP gathering so this can't happen.

       Note: --fingerprint-os=Windows|macOS|Linux is also a launch-time
       flag and must be passed when the Chromeleon process is started
       (e.g., to the docker run command, or in docker-entrypoint.sh).
       It cannot be changed via CDP after connectOverCDP attaches.

    2. pip install playwright && playwright install

Usage:
    # Connect to local Docker container
    python connect_over_cdp_example.py

    # Connect to remote host
    python connect_over_cdp_example.py --cdp-url http://10.0.0.5:9222

    # With per-context proxy
    python connect_over_cdp_example.py \
        --proxy http://user:pass_country-jp@residential.floxy.io:12321
"""

import argparse
import asyncio
import ipaddress
from urllib.parse import unquote, urlsplit

from playwright.async_api import async_playwright

#: Ports a scheme omits from its canonical authority, matching WHATWG URL.
_DEFAULT_PORTS = {"http": 80, "https": 443}


def normalize_proxy_server(server: str) -> str:
    """Canonicalise a proxy server the way Playwright will.

    The browser matches the two proxyServer strings byte-for-byte, and only the
    registration is ours to spell: Playwright rewrites the one it sends to
    createBrowserContext as `url.protocol + "//" + url.host`
    (browserContext.js normalizeProxySettings), which drops the scheme's default
    port, lowercases the host, and strips any path. Register `http://gw:80` and
    the context asks for `http://gw` — the registration is never consumed and
    creation fails with "Target.setProxyCredentials must be called first".
    """
    raw = str(server).strip()
    parts = urlsplit(raw)
    if not parts.scheme or not parts.netloc:      # scheme-less "host:port"
        parts = urlsplit("http://" + raw)
    scheme = (parts.scheme or "http").lower()
    port = parts.port                             # ValueError on a bad port
    host = unquote(parts.hostname or "")
    if ":" in host:                               # IPv6 — hostname drops the []
        host = f"[{ipaddress.IPv6Address(host).compressed}]"
    elif any(ord(c) > 127 for c in host):
        host = host.encode("idna").decode("ascii")
    host = host.lower()
    if port is not None and port != _DEFAULT_PORTS.get(scheme):
        host = f"{host}:{port}"
    return f"{scheme}://{host}"


def parse_proxy(proxy_url: str) -> tuple[str, str, str]:
    """Return the server spelling PLAYWRIGHT will send, plus its credentials."""
    scheme_pos = proxy_url.find("://")
    scheme_end = scheme_pos + 3 if scheme_pos != -1 else 0
    at_pos = proxy_url.rfind("@")
    if at_pos < scheme_end:
        raise ValueError("--proxy must include username:password@")
    credentials = proxy_url[scheme_end:at_pos]
    if ":" not in credentials:
        raise ValueError("--proxy must include username:password@")
    username, password = credentials.split(":", 1)
    if not username:
        raise ValueError("--proxy username cannot be empty")
    server = normalize_proxy_server(proxy_url[:scheme_end] + proxy_url[at_pos + 1:])
    if not server.startswith(("http://", "https://")):
        raise ValueError("--proxy must be an HTTP(S) URL")
    return server, username, password


async def main(cdp_url: str, proxy: str | None = None):
    async with async_playwright() as p:
        # Connect to the already-running Chromeleon browser
        print(f"Connecting to {cdp_url} ...")
        browser = await p.chromium.connect_over_cdp(cdp_url)
        print(f"Connected! Browser version: {browser.version}")

        # --- Per-context proxy (optional) ---
        context_opts = {}
        if proxy:
            proxy_server, username, password = parse_proxy(proxy)

            # Registration is root-scoped and single-use. Keep this send and
            # the matching context creation serialized, and reuse this exact
            # proxy_server value in both calls — parse_proxy already normalized
            # it to the spelling Playwright will send, so the two match.
            cdp = await browser.new_browser_cdp_session()
            try:
                result = await cdp.send(
                    "Target.setProxyCredentials",
                    {
                        "proxyServer": proxy_server,
                        "username": username,
                        "password": password,
                    },
                )
                if result != {}:
                    raise RuntimeError(
                        f"unexpected credential registration result: {result!r}"
                    )
            finally:
                await cdp.detach()

            context_opts["proxy"] = {"server": proxy_server}
            print(f"Proxy: {proxy_server}")

        # Create a new context (gets its own fingerprint)
        context = await browser.new_context(**context_opts)
        page = await context.new_page()

        # Verify fingerprint properties
        await page.goto("about:blank")
        info = await page.evaluate("""() => ({
            userAgent: navigator.userAgent,
            platform: navigator.platform,
            language: navigator.language,
            timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
            locale: Intl.DateTimeFormat().resolvedOptions().locale,
            hardwareConcurrency: navigator.hardwareConcurrency,
            screenWidth: screen.width,
            screenHeight: screen.height,
        })""")

        print(f"\nFingerprint properties:")
        for k, v in info.items():
            print(f"  {k}: {v}")

        # Visit a test page
        print(f"\nVisiting fingerprint.com playground...")
        await page.goto("https://demo.fingerprint.com/playground", wait_until="networkidle")
        title = await page.title()
        print(f"  Page title: {title}")

        # Cleanup
        await context.close()
        await browser.close()
        print("\nDone!")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Connect to Chromeleon via CDP")
    parser.add_argument("--cdp-url", default="http://localhost:9222",
                        help="CDP endpoint URL (default: http://localhost:9222)")
    parser.add_argument("--proxy", default=None,
                        help="Proxy URL (e.g., http://user:pass@host:port)")
    args = parser.parse_args()
    asyncio.run(main(args.cdp_url, args.proxy))
