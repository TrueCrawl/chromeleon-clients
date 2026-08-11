#!/usr/bin/env python3
"""
Chromeleon Per-Context Proxy Example

Demonstrates launching a single browser instance with multiple contexts,
each using a different proxy. Each context automatically gets a fingerprint
matching its proxy's geographic location (timezone, locale, etc.).

For country-targeted proxies (e.g., Floxy with _country-XX in the password),
pre-register the credentials with the browser-level
Target.setProxyCredentials command, then create a context that contains only
the exact same proxy server string. This is the sole supported per-context
credential path.

Usage:
    # Credentials are parsed out for CDP registration, not passed to context:
    python per_context_proxy_example.py --chromeleon-path /path/to/chrome \
        --proxy1 http://user:pass_country-us@residential.floxy.io:12321 \
        --proxy2 http://user:pass_country-jp@residential.floxy.io:12321

    # With fingerprint OS spoofing:
    python per_context_proxy_example.py --chromeleon-path /path/to/chrome \
        --proxy1 http://user:pass@us-proxy:8080 \
        --proxy2 http://user:pass@jp-proxy:8080 \
        --fingerprint-os Windows
"""

import argparse
import asyncio
import os
import sys
from pathlib import Path

# Examples run from the examples/ directory, so put the repo root on sys.path
# before importing the client library.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from chromeleon import (  # noqa: E402
    browser_process_env,
    LAUNCH_ARGS,
    ProxySpec,
    new_proxy_context_async,
)
from chromeleon import parse_proxy as _parse_proxy  # noqa: E402

try:
    from playwright.async_api import async_playwright
except ImportError:
    # Don't sys.exit at import — parse_proxy() is pure and is exercised by
    # tests/unit/test_parse_proxy.py without playwright installed. main()
    # checks the symbol and exits cleanly if it's needed but missing.
    async_playwright = None


def parse_proxy(proxy_url):
    """Parse proxy URL into (server, username, password).

    Thin shim over chromeleon.parse_proxy, kept so this example reads
    linearly. Prefer importing the library directly in real code.
    """
    return tuple(_parse_proxy(proxy_url))


async def create_proxy_context(
        browser, server, username=None, password=None, **context_options):
    """Create a browser context with a per-context proxy.

    Delegates to chromeleon, which performs the register-then-create
    pair under a per-(browser, server) lock — the registration is single-use and
    carries no correlation token, so interleaving concurrent pairs gets the
    second one rejected.
    """
    return await new_proxy_context_async(
        browser, ProxySpec(server, username, password), **context_options)


async def check_context_identity(page, label):
    """Check timezone, locale, and IP for a browser context."""
    print(f"\n--- {label} ---")

    tz = await page.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone")
    print(f"  Timezone: {tz}")

    locale = await page.evaluate("Intl.DateTimeFormat().resolvedOptions().locale")
    print(f"  Locale: {locale}")

    lang = await page.evaluate("navigator.language")
    print(f"  Language: {lang}")

    langs = await page.evaluate("JSON.stringify(navigator.languages)")
    print(f"  Languages: {langs}")

    try:
        await page.goto("https://api.ipify.org?format=json", timeout=15000)
        ip_text = await page.evaluate("document.body.innerText")
        print(f"  IP: {ip_text}")
    except Exception as e:
        print(f"  IP check failed: {e}")


async def main():
    if async_playwright is None:
        print("Error: playwright not installed. Run: pip install playwright")
        sys.exit(1)
    parser = argparse.ArgumentParser(description="Chromeleon Per-Context Proxy Example")
    parser.add_argument("--chromeleon-path", required=True, help="Path to Chromeleon binary")
    parser.add_argument("--proxy1", required=True, help="First proxy (e.g., http://user:pass@host:port)")
    parser.add_argument("--proxy2", required=True, help="Second proxy (e.g., http://user:pass@host:port)")
    parser.add_argument("--fingerprint-os", help="OS to spoof (e.g., Windows, macOS)")
    parser.add_argument("--headless", action="store_true", help="Run in headless mode")
    args = parser.parse_args()

    chromeleon_path = str(Path(args.chromeleon_path).resolve())
    if not os.path.exists(chromeleon_path):
        print(f"Error: Chromeleon binary not found: {chromeleon_path}")
        sys.exit(1)

    # Context-level proxy (new_context(proxy=...)) routes HTTP through the
    # proxy but WebRTC's UDP socket lives at the browser level and uses the
    # default route, which leaks the real public IP via ICE srflx candidates.
    # Force disable_non_proxied_udp so WebRTC can't gather over unproxied UDP.
    launch_args = list(LAUNCH_ARGS)
    if args.fingerprint_os:
        launch_args.append(f"--fingerprint-os={args.fingerprint_os}")

    print(f"Chromeleon: {chromeleon_path}")
    print(f"Proxy 1: {args.proxy1}")
    print(f"Proxy 2: {args.proxy2}")
    print(f"Fingerprint OS: {args.fingerprint_os or 'default'}")

    async with async_playwright() as p:
        browser = await p.chromium.launch(
            executable_path=chromeleon_path,
            args=launch_args,
            headless=args.headless,
            env=browser_process_env(),
        )

        # Parse proxy URLs
        server1, user1, pass1 = parse_proxy(args.proxy1)
        server2, user2, pass2 = parse_proxy(args.proxy2)

        # Each helper call serializes credential preregistration and context
        # creation; credentials are never passed to new_context().
        context1 = await create_proxy_context(browser, server1, user1, pass1)
        page1 = await context1.new_page()
        await check_context_identity(page1, "Context 1 (Proxy 1)")

        context2 = await create_proxy_context(browser, server2, user2, pass2)
        page2 = await context2.new_page()
        await check_context_identity(page2, "Context 2 (Proxy 2)")

        # Verify they're different
        tz1 = await page1.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone")
        tz2 = await page2.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone")
        print(f"\n=== Summary ===")
        print(f"Context 1 timezone: {tz1}")
        print(f"Context 2 timezone: {tz2}")
        print(f"Different timezones: {tz1 != tz2}")

        await context1.close()
        await context2.close()
        await browser.close()

    print("\nDone!")


if __name__ == "__main__":
    asyncio.run(main())
