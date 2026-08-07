#!/usr/bin/env python3
"""
Chromeleon reCAPTCHA Auto-Solver Example (Sync API)

Minimal example using Playwright's synchronous API.
The captcha solver runs transparently — just enable it and navigate.

Usage:
    python captcha_solver_example_sync.py --chromeleon-path /path/to/chrome

    # With proxy:
    python captcha_solver_example_sync.py --chromeleon-path /path/to/chrome \
        --proxy http://user:pass@host:port
"""

import argparse
import sys
import time

try:
    from playwright.sync_api import sync_playwright
except ImportError:
    print("Error: playwright not installed. Run: pip install playwright")
    sys.exit(1)

try:
    from examples.per_context_proxy_example_sync import (
        browser_process_env,
        create_proxy_context,
        parse_proxy,
    )
except ImportError:
    from per_context_proxy_example_sync import (
        browser_process_env,
        create_proxy_context,
        parse_proxy,
    )


def main():
    parser = argparse.ArgumentParser(
        description="Chromeleon Captcha Solver (Sync)")
    parser.add_argument("--chromeleon-path", required=True,
                        help="Path to Chromeleon chrome binary")
    parser.add_argument("--model-path", default=None,
                        help=argparse.SUPPRESS)  # Dev only.
    parser.add_argument("--proxy", default=None,
                        help="Proxy URL (http://user:pass@host:port)")
    args = parser.parse_args()

    chrome_args = ["--captcha-solver", "--no-sandbox"]
    if args.model_path:
        chrome_args.append(f"--captcha-model-path={args.model_path}")
    if args.proxy:
        chrome_args.append(
            "--webrtc-ip-handling-policy=disable_non_proxied_udp")

    with sync_playwright() as p:
        browser = p.chromium.launch(
            executable_path=args.chromeleon_path,
            headless=True,
            args=chrome_args,
            env=browser_process_env(),
        )
        if args.proxy:
            server, username, password = parse_proxy(args.proxy)
            context = create_proxy_context(
                browser, server, username, password)
        else:
            context = browser.new_context()
        page = context.new_page()

        print("Navigating to reCAPTCHA demo...")
        start = time.time()
        page.goto(
            "https://recaptcha-demo.appspot.com/recaptcha-v2-checkbox.php",
            timeout=30000)

        # Just wait for the solver to fill the response token.
        # No CDP session needed — the solver handles everything.
        print("Waiting for auto-solver...")
        try:
            page.wait_for_function(
                "() => {"
                "  const ta = document.querySelector("
                "    'textarea[name=g-recaptcha-response]');"
                "  return ta && ta.value && ta.value.length > 10;"
                "}",
                timeout=60000,
            )
            elapsed = time.time() - start
            print(f"Solved in {elapsed:.1f}s!")
        except Exception:
            elapsed = time.time() - start
            print(f"Not solved after {elapsed:.1f}s")

        browser.close()


if __name__ == "__main__":
    main()
