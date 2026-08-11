"""A thin client for driving Chromeleon.

Chromeleon attaches authenticated proxies per browser context with two
browser-level CDP commands. That is a protocol fact, so the core here knows only
the protocol and the lock it needs; each driver is a short adapter.

Playwright:

    from playwright.sync_api import sync_playwright
    from chromeleon import LAUNCH_ARGS, new_proxy_context

    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=CHROMELEON,
                                    args=list(LAUNCH_ARGS))
        context = new_proxy_context(browser, "http://user:pass@gateway:12321")
        page = context.new_page()

Any other driver — Selenium, a raw DevTools WebSocket, a language with no
Playwright binding — uses the core directly:

    from chromeleon import (CREDENTIALS_METHOD, check_registration,
                                   credentials_params, proxy_registration)

    with proxy_registration(connection, proxy) as spec:
        check_registration(send(CREDENTIALS_METHOD, credentials_params(spec)))
        context_id = send("Target.createBrowserContext",
                          {"proxyServer": spec.server})

Chromeleon also ships a built-in captcha solver. Launch with it on and it solves
reCAPTCHA/hCaptcha automatically; watch it over a page CDP session:

    from chromeleon import launch, enable_captcha, CAPTCHA_SOLVED, CAPTCHA_FAILED

    browser = launch(p.chromium, CHROMELEON, captcha=True)
    page = browser.new_page()
    cdp = page.context.new_cdp_session(page)
    enable_captcha(cdp)
    cdp.on(CAPTCHA_SOLVED, lambda p: print("solved", p["timeMs"], "ms"))
    cdp.on(CAPTCHA_FAILED, lambda p: print("failed", p["reason"]))
    page.goto("https://example.com/with-a-recaptcha")

Playwright objects go in and come back out unchanged; this does not wrap
``launch`` or own the browser.
"""
from chromeleon.adapters import (  # noqa: F401
    browser_process_env,
    disable_captcha,
    enable_captcha,
    launch,
    new_proxy_context,
    new_proxy_context_async,
    solver_eval,
)
from chromeleon.core import (  # noqa: F401
    CAPTCHA_DETECTED,
    CAPTCHA_EVENTS,
    CAPTCHA_FAILED,
    CAPTCHA_MODEL_PATH_SWITCH,
    CAPTCHA_SOLVED,
    CAPTCHA_SOLVER_SWITCH,
    CAPTCHA_SOLVING,
    CREDENTIALS_METHOD,
    DISABLE_METHOD,
    ENABLE_METHOD,
    LAUNCH_ARGS,
    ProxySpec,
    SOLVER_EVAL_METHOD,
    SOLVER_EVAL_RESULT,
    captcha_launch_args,
    check_registration,
    credentials_params,
    normalize_server,
    parse_proxy,
    proxy_registration,
    solver_eval_params,
)
from chromeleon.perf import (  # noqa: F401
    BrowserPool,
    sticky_geo_env,
)

__all__ = [
    "CREDENTIALS_METHOD",
    "LAUNCH_ARGS",
    "BrowserPool",
    "ProxySpec",
    "browser_process_env",
    "check_registration",
    "credentials_params",
    "launch",
    "new_proxy_context",
    "new_proxy_context_async",
    "normalize_server",
    "parse_proxy",
    "proxy_registration",
    "sticky_geo_env",
    # Captcha solver (Chromeleon CDP domain).
    "CAPTCHA_SOLVER_SWITCH",
    "CAPTCHA_MODEL_PATH_SWITCH",
    "ENABLE_METHOD",
    "DISABLE_METHOD",
    "SOLVER_EVAL_METHOD",
    "CAPTCHA_DETECTED",
    "CAPTCHA_SOLVING",
    "CAPTCHA_SOLVED",
    "CAPTCHA_FAILED",
    "SOLVER_EVAL_RESULT",
    "CAPTCHA_EVENTS",
    "captcha_launch_args",
    "solver_eval_params",
    "enable_captcha",
    "disable_captcha",
    "solver_eval",
]
