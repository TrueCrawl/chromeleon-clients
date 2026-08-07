#!/usr/bin/env python3
"""Launch Chromeleon with embedded VNC server and control via Playwright.

This example starts the browser with the VNC Ozone platform, then connects
via CDP to control it with Playwright. A VNC viewer can connect at the same
time to watch the browser in real-time — useful for debugging, demos, and
monitoring automated sessions.

Prerequisites:
    pip install playwright && playwright install

Usage:
    # Launch with VNC on default port 5900, control via Playwright
    python vnc_example.py --chromeleon-path ./chrome

    # Custom VNC port
    python vnc_example.py --chromeleon-path ./chrome --vnc-port 5901

    # With fingerprint spoofing
    python vnc_example.py --chromeleon-path ./chrome --fingerprint-os Windows

    # Visit a specific URL
    python vnc_example.py --chromeleon-path ./chrome --url https://example.com

    # Then connect any VNC viewer to see it live:
    #   vncviewer localhost:5900

Docker:
    docker run -d -p 9222:9222 -p 5900:5900 chromeleon \
        --ozone-platform=vnc --vnc-port=5900 --remote-debugging-port=9222

    python vnc_example.py --cdp-url http://localhost:9222
"""

import argparse
import asyncio
import subprocess
import sys
import time

from playwright.async_api import async_playwright


async def main(
    chromeleon_path: str | None = None,
    cdp_url: str | None = None,
    vnc_port: int = 5900,
    fingerprint_os: str | None = None,
    url: str = "https://example.com",
    headless: bool = False,
):
    browser_process = None

    try:
        if cdp_url is None:
            # Launch the browser with VNC platform
            if not chromeleon_path:
                print("Error: --chromeleon-path or --cdp-url required")
                sys.exit(1)

            cdp_port = 9222
            args = [
                chromeleon_path,
                f"--ozone-platform=vnc",
                f"--vnc-port={vnc_port}",
                f"--remote-debugging-port={cdp_port}",
                "--no-sandbox",
                "--disable-gpu",
                "about:blank",
            ]
            if fingerprint_os:
                args.append(f"--fingerprint-os={fingerprint_os}")

            print(f"Launching Chromeleon with VNC on port {vnc_port}...")
            browser_process = subprocess.Popen(
                args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            )
            cdp_url = f"http://127.0.0.1:{cdp_port}"

            # Wait for CDP to be ready
            for i in range(30):
                try:
                    import urllib.request
                    urllib.request.urlopen(f"{cdp_url}/json/version", timeout=1)
                    break
                except Exception:
                    time.sleep(0.5)
            else:
                print("Error: browser did not start in time")
                sys.exit(1)

            print(f"Browser ready. VNC viewer: vncviewer localhost:{vnc_port}")

        # Connect via CDP
        async with async_playwright() as p:
            print(f"Connecting to {cdp_url} ...")
            browser = await p.chromium.connect_over_cdp(cdp_url)
            print(f"Connected! Browser version: {browser.version}")

            # Create a new context and page
            context = await browser.new_context()
            page = await context.new_page()

            # Navigate
            print(f"Navigating to {url} ...")
            await page.goto(url, wait_until="domcontentloaded")
            title = await page.title()
            print(f"Page title: {title}")

            # Show viewport info
            dims = await page.evaluate(
                "({w: window.innerWidth, h: window.innerHeight})"
            )
            print(f"Viewport: {dims['w']}x{dims['h']}")

            # Demonstrate interaction
            print("\nThe browser is now visible via VNC.")
            print(f"Connect a VNC viewer to localhost:{vnc_port} to see it live.")
            print("Press Ctrl+C to exit.\n")

            # Keep running so the user can interact via VNC viewer
            try:
                while True:
                    await asyncio.sleep(1)
            except KeyboardInterrupt:
                print("\nShutting down...")

            await context.close()
            browser.close()

    finally:
        if browser_process:
            browser_process.terminate()
            browser_process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Launch Chromeleon with embedded VNC server"
    )
    parser.add_argument(
        "--chromeleon-path", help="Path to Chromeleon chrome binary"
    )
    parser.add_argument(
        "--cdp-url",
        help="Connect to existing browser via CDP (skip launching)",
    )
    parser.add_argument(
        "--vnc-port", type=int, default=5900, help="VNC server port (default: 5900)"
    )
    parser.add_argument(
        "--fingerprint-os",
        choices=["Windows", "macOS", "Linux"],
        help="Spoof target OS fingerprint",
    )
    parser.add_argument(
        "--url", default="https://example.com", help="URL to navigate to"
    )
    args = parser.parse_args()

    asyncio.run(
        main(
            chromeleon_path=args.chromeleon_path,
            cdp_url=args.cdp_url,
            vnc_port=args.vnc_port,
            fingerprint_os=args.fingerprint_os,
            url=args.url,
        )
    )
