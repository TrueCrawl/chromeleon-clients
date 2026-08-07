#!/usr/bin/env python3
"""
Chromeleon Playwright Example

Usage:
    python playwright_example.py --chromeleon-path /path/to/chrome
    python playwright_example.py --chromeleon-path /path/to/chrome --headless
    python playwright_example.py --chromeleon-path /path/to/chrome --proxy http://user:pass@host:port
    python playwright_example.py --chromeleon-path /path/to/chrome --fingerprint-os windows
    python playwright_example.py --chromeleon-path /path/to/chrome --test  # Run fingerprint tests
"""

import argparse
import asyncio
import os
import sys
from pathlib import Path

try:
    from playwright.async_api import async_playwright
except ImportError:
    print("Error: playwright not installed. Run: pip install playwright")
    sys.exit(1)

try:
    from examples.per_context_proxy_example import (
        browser_process_env,
        create_proxy_context,
        parse_proxy,
    )
except ImportError:
    from per_context_proxy_example import (
        browser_process_env,
        create_proxy_context,
        parse_proxy,
    )


# Fingerprint detection test sites
TEST_SITES = [
    ("https://demo.fingerprint.com/playground", "FingerprintJS"),
    ("https://overpoweredjs.com/", "OverpoweredJS"),
]


async def extract_fingerprintjs_scores(page):
    """Extract scores from FingerprintJS demo page."""
    body_text = await page.evaluate('document.body.innerText')
    results = {}
    lines = body_text.split('\n')

    for i, line in enumerate(lines):
        line = line.strip()
        if 'CONFIDENCE SCORE' in line:
            for j in range(i+1, min(i+3, len(lines))):
                if lines[j].strip():
                    results['Confidence Score'] = lines[j].strip()
                    break
        if 'SUSPECT SCORE' in line:
            for j in range(i+1, min(i+3, len(lines))):
                if lines[j].strip() and lines[j].strip().isdigit():
                    results['Suspect Score'] = lines[j].strip()
                    break
        if 'Chrome' in line and ' on ' in line:
            results['Detected Browser'] = line.strip()
        if line == 'OPERATING SYSTEM':
            for j in range(i+1, min(i+3, len(lines))):
                val = lines[j].strip()
                if val and val in ['Windows', 'macOS', 'Linux', 'Android', 'iOS']:
                    results['Detected OS'] = val
                    break

    return results


async def run_tests(page, output_dir="."):
    """Run fingerprint detection tests on multiple sites."""
    print("\n=== Running Fingerprint Detection Tests ===\n")

    for url, name in TEST_SITES:
        print(f"Testing: {name}")
        print(f"  URL: {url}")

        try:
            await page.goto(url, timeout=60000, wait_until="domcontentloaded")
            await asyncio.sleep(8)  # Wait for JS fingerprinting to complete

            # Extract FingerprintJS scores
            if name == "FingerprintJS":
                scores = await extract_fingerprintjs_scores(page)
                if scores:
                    for key, val in scores.items():
                        print(f"  {key}: {val}")

            filename = f"{output_dir}/test_{name.lower().replace(' ', '_')}.png"
            await page.screenshot(path=filename, full_page=True)
            print(f"  Screenshot: {filename}")
        except Exception as e:
            print(f"  Error: {e}")

        print()


async def main():
    parser = argparse.ArgumentParser(description="Chromeleon Playwright Example")
    parser.add_argument("--chromeleon-path", required=True, help="Path to Chromeleon chrome binary")
    parser.add_argument("--headless", action="store_true", help="Run in headless mode")
    parser.add_argument("--proxy", help="Proxy server (e.g., http://user:pass@host:port)")
    parser.add_argument("--fingerprint-os", choices=["windows", "linux", "macos"], help="OS to spoof")
    parser.add_argument("--test", action="store_true", help="Run fingerprint detection tests")
    parser.add_argument("--url", help="Custom URL to visit")
    args = parser.parse_args()

    # Resolve to absolute path
    chromeleon_path = str(Path(args.chromeleon_path).resolve())

    if not os.path.exists(chromeleon_path):
        print(f"Error: Chromeleon binary not found: {chromeleon_path}")
        sys.exit(1)

    # Build launch args
    # --webrtc-ip-handling-policy=disable_non_proxied_udp:
    # Required whenever a proxy is in play. Context-level proxy attachment
    # (new_context(proxy=...)) routes HTTP through the proxy but WebRTC's
    # UDP lives at browser level and uses the default route, leaking the
    # real public IP via ICE srflx candidates. This flag suppresses
    # unproxied UDP gathering for the server-only context created below.
    launch_args = [
        "--disable-blink-features=AutomationControlled",
        "--disable-infobars",
        "--no-first-run",
        "--disable-background-networking",
        "--webrtc-ip-handling-policy=disable_non_proxied_udp",
    ]

    if args.fingerprint_os:
        launch_args.append(f"--fingerprint-os={args.fingerprint_os}")

    if args.headless:
        launch_args.append("--headless=new")

    print(f"Chromeleon: {chromeleon_path}")
    print(f"Headless: {args.headless}")
    print(f"Proxy: {'configured' if args.proxy else 'none'}")
    print(f"Fingerprint OS: {args.fingerprint_os or 'default'}")

    async with async_playwright() as p:
        browser = await p.chromium.launch(
            executable_path=chromeleon_path,
            args=launch_args,
            headless=args.headless,
            env=browser_process_env(),
        )

        context_options = {"viewport": {"width": 1920, "height": 1080}}
        if args.proxy:
            server, username, password = parse_proxy(args.proxy)
            context = await create_proxy_context(
                browser, server, username, password, **context_options)
        else:
            context = await browser.new_context(**context_options)

        page = await context.new_page()

        if args.test:
            # Run fingerprint detection tests
            await run_tests(page)
        else:
            # Single page visit
            url = args.url or "https://browserleaks.com/javascript"
            await page.goto(url, timeout=60000, wait_until="domcontentloaded")
            await asyncio.sleep(8)

            # Extract FingerprintJS scores if visiting their demo
            if "demo.fingerprint.com" in url:
                scores = await extract_fingerprintjs_scores(page)
                if scores:
                    print("\n=== FingerprintJS Results ===")
                    for key, val in scores.items():
                        print(f"  {key}: {val}")

            await page.screenshot(path="screenshot.png")
            print(f"\nScreenshot saved: screenshot.png")

            title = await page.title()
            print(f"Page title: {title}")

        if not args.headless:
            print("\nBrowser open. Press Enter to close...")
            input()

        await browser.close()


if __name__ == "__main__":
    asyncio.run(main())
