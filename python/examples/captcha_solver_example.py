#!/usr/bin/env python3
"""
Chromeleon reCAPTCHA Auto-Solver Example

Demonstrates how to use the embedded captcha solver with Playwright.
The solver automatically detects and solves reCAPTCHA v2/Enterprise
challenges using audio-based speech-to-text (whisper.cpp).

Setup:
    1. Build Chromeleon with --captcha-solver support
    2. Place whisper-cli + ggml-small.en.bin in a model directory
    3. Run this script

Usage:
    # Basic — solver runs transparently:
    python captcha_solver_example.py --chromeleon-path /path/to/chrome

    # With proxy (recommended for clean IP):
    python captcha_solver_example.py --chromeleon-path /path/to/chrome \
        --proxy http://user:pass@host:port

    # With CDP event tracking:
    python captcha_solver_example.py --chromeleon-path /path/to/chrome --cdp-events

    # Enterprise v2:
    python captcha_solver_example.py --chromeleon-path /path/to/chrome --site enterprise

CDP Events (Chromeleon domain):
    Chromeleon.captchaDetected  — reCAPTCHA found on page
    Chromeleon.captchaSolving   — solving started (audio mode)
    Chromeleon.captchaSolved    — successfully solved
    Chromeleon.captchaFailed    — all attempts exhausted
"""

import argparse
import asyncio
import sys
import time

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


# Demo sites with reCAPTCHA v2
DEMO_SITES = {
    "v2": "https://recaptcha-demo.appspot.com/recaptcha-v2-checkbox.php",
    "enterprise": "https://2captcha.com/demo/recaptcha-v2-enterprise",
}


async def solve_with_cdp_events(page, timeout=60):
    """Wait for captcha solve using CDP events (recommended approach).

    Returns the captchaSolved event params, or raises on failure.
    """
    cdp = await page.context.new_cdp_session(page)
    await cdp.send("Chromeleon.enable")

    result_future = asyncio.get_event_loop().create_future()

    def on_solved(params):
        if not result_future.done():
            result_future.set_result(params)

    def on_failed(params):
        if not result_future.done():
            result_future.set_exception(
                Exception(f"Captcha failed: {params.get('reason', 'unknown')}")
            )

    cdp.on("Chromeleon.captchaDetected",
           lambda p: print(f"  [CDP] Detected: sitekey={p['sitekey'][:12]}..."))
    cdp.on("Chromeleon.captchaSolving",
           lambda p: print(f"  [CDP] Solving via {p['method']}..."))
    cdp.on("Chromeleon.captchaSolved", on_solved)
    cdp.on("Chromeleon.captchaFailed", on_failed)

    # Also log solved/failed
    cdp.on("Chromeleon.captchaSolved",
           lambda p: print(f"  [CDP] Solved in {p['timeMs']:.0f}ms, "
                          f"{p['attempts']} attempt(s)"))
    cdp.on("Chromeleon.captchaFailed",
           lambda p: print(f"  [CDP] Failed: {p['reason']}"))

    try:
        return await asyncio.wait_for(result_future, timeout=timeout)
    finally:
        await cdp.send("Chromeleon.disable")


async def solve_with_polling(page, timeout=60):
    """Wait for captcha solve by polling the response textarea.

    Simpler approach — no CDP session needed. Works with sync API too.
    """
    try:
        await page.wait_for_function(
            "() => {"
            "  const ta = document.querySelector("
            "    'textarea[name=g-recaptcha-response]');"
            "  return ta && ta.value && ta.value.length > 10;"
            "}",
            timeout=timeout * 1000,
        )
        return True
    except Exception:
        return False


async def main():
    parser = argparse.ArgumentParser(
        description="Chromeleon reCAPTCHA Auto-Solver Example")
    parser.add_argument("--chromeleon-path", required=True,
                        help="Path to Chromeleon chrome binary")
    parser.add_argument("--model-path", default=None,
                        help=argparse.SUPPRESS)  # Dev only — models embedded in binary.
    parser.add_argument("--proxy", default=None,
                        help="Proxy URL (http://user:pass@host:port)")
    parser.add_argument("--headless", action="store_true", default=True,
                        help="Run headless (default)")
    parser.add_argument("--no-headless", action="store_true",
                        help="Run with visible browser")
    parser.add_argument("--cdp-events", action="store_true",
                        help="Use CDP events to track solver progress")
    parser.add_argument("--site", choices=list(DEMO_SITES.keys()),
                        default="v2",
                        help="Which demo site to test (default: v2)")
    args = parser.parse_args()

    headless = not args.no_headless

    # Build Chrome launch args
    chrome_args = ["--captcha-solver", "--no-sandbox"]
    if args.model_path:
        chrome_args.append(f"--captcha-model-path={args.model_path}")
    if args.proxy:
        chrome_args.append(
            "--webrtc-ip-handling-policy=disable_non_proxied_udp")

    url = DEMO_SITES[args.site]
    print(f"Chromeleon reCAPTCHA Auto-Solver Example")
    print(f"  Binary:  {args.chromeleon_path}")
    print(f"  Site:    {url}")
    print(f"  Proxy:   {'configured' if args.proxy else 'none'}")
    print(f"  Method:  {'CDP events' if args.cdp_events else 'polling'}")
    print()

    async with async_playwright() as p:
        browser = await p.chromium.launch(
            executable_path=args.chromeleon_path,
            headless=headless,
            args=chrome_args,
            env=browser_process_env(),
        )
        if args.proxy:
            server, username, password = parse_proxy(args.proxy)
            context = await create_proxy_context(
                browser, server, username, password)
        else:
            context = await browser.new_context()
        page = await context.new_page()

        print(f"Navigating to {url}...")
        start = time.time()
        await page.goto(url, timeout=30000)

        # Wait for reCAPTCHA iframe to appear
        try:
            await page.locator('iframe[title*="reCAPTCHA"]').first.wait_for(
                state="attached", timeout=10000)
        except Exception:
            print("No reCAPTCHA iframe found on page.")
            await browser.close()
            return

        print("reCAPTCHA detected. Solver is working...")
        print()

        if args.cdp_events:
            # Method 1: CDP events (recommended for async scripts)
            try:
                result = await solve_with_cdp_events(page, timeout=60)
                elapsed = time.time() - start
                print()
                print(f"SUCCESS! Captcha solved in {elapsed:.1f}s")
                print(f"  Attempts: {result['attempts']}")
                print(f"  Time:     {result['timeMs']:.0f}ms")
            except Exception as e:
                print(f"\nFAILED: {e}")
        else:
            # Method 2: Polling (simpler, works with sync API too)
            solved = await solve_with_polling(page, timeout=60)
            elapsed = time.time() - start
            if solved:
                print(f"SUCCESS! Captcha solved in {elapsed:.1f}s")
            else:
                print(f"FAILED after {elapsed:.1f}s")

        await browser.close()


if __name__ == "__main__":
    asyncio.run(main())
