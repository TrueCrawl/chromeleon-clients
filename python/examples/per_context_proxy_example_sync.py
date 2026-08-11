#!/usr/bin/env python3
"""
Chromeleon Per-Context Proxy Example (Sync API)

Same as per_context_proxy_example.py but using Playwright's synchronous API.
Credentials are registered with Target.setProxyCredentials immediately before
each server-only context is created.

Usage:
    python per_context_proxy_example_sync.py --chromeleon-path /path/to/chrome \
        --proxy1 http://user:pass_country-us@residential.floxy.io:12321 \
        --proxy2 http://user:pass_country-jp@residential.floxy.io:12321
"""

import argparse
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
    new_proxy_context,
)
from chromeleon import parse_proxy as _parse_proxy  # noqa: E402

try:
    from playwright.sync_api import sync_playwright
except ImportError:
    # Don't sys.exit at import — parse_proxy() is pure and is exercised by
    # tests/unit/test_parse_proxy.py without playwright installed. main()
    # checks the symbol and exits cleanly if it's needed but missing.
    sync_playwright = None


def parse_proxy(proxy_url):
    """Parse proxy URL into (server, username, password).

    Thin shim over chromeleon.parse_proxy, kept so this example reads
    linearly. Prefer importing the library directly in real code.
    """
    return tuple(_parse_proxy(proxy_url))


def create_proxy_context(
        browser, server, username=None, password=None, **context_options):
    """Create a browser context with a per-context proxy.

    Delegates to chromeleon, which performs the register-then-create
    pair under a per-(browser, server) lock — the registration is single-use and
    carries no correlation token, so interleaving concurrent pairs gets the
    second one rejected.
    """
    return new_proxy_context(
        browser, ProxySpec(server, username, password), **context_options)


def check_context_identity(page, label):
    """Check timezone, locale, and IP for a browser context."""
    print(f"\n--- {label} ---")

    tz = page.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone")
    print(f"  Timezone: {tz}")

    locale = page.evaluate("Intl.DateTimeFormat().resolvedOptions().locale")
    print(f"  Locale: {locale}")

    lang = page.evaluate("navigator.language")
    print(f"  Language: {lang}")

    langs = page.evaluate("JSON.stringify(navigator.languages)")
    print(f"  Languages: {langs}")

    try:
        page.goto("https://api.ipify.org?format=json", timeout=15000)
        ip_text = page.evaluate("document.body.innerText")
        print(f"  IP: {ip_text}")
    except Exception as e:
        print(f"  IP check failed: {e}")


def main():
    if sync_playwright is None:
        print("Error: playwright not installed. Run: pip install playwright")
        sys.exit(1)
    parser = argparse.ArgumentParser(description="Chromeleon Per-Context Proxy Example (Sync)")
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

    with sync_playwright() as p:
        browser = p.chromium.launch(
            executable_path=chromeleon_path,
            args=launch_args,
            headless=args.headless,
            env=browser_process_env(),
        )

        server1, user1, pass1 = parse_proxy(args.proxy1)
        server2, user2, pass2 = parse_proxy(args.proxy2)

        context1 = create_proxy_context(browser, server1, user1, pass1)
        page1 = context1.new_page()
        check_context_identity(page1, "Context 1 (Proxy 1)")

        context2 = create_proxy_context(browser, server2, user2, pass2)
        page2 = context2.new_page()
        check_context_identity(page2, "Context 2 (Proxy 2)")

        tz1 = page1.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone")
        tz2 = page2.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone")
        print(f"\n=== Summary ===")
        print(f"Context 1 timezone: {tz1}")
        print(f"Context 2 timezone: {tz2}")
        print(f"Different timezones: {tz1 != tz2}")

        context1.close()
        context2.close()
        browser.close()

    print("\nDone!")


if __name__ == "__main__":
    main()
