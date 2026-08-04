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

Playwright objects go in and come back out unchanged; this does not wrap
``launch`` or own the browser.
"""
from chromeleon.adapters import (  # noqa: F401
    browser_process_env,
    launch,
    new_proxy_context,
    new_proxy_context_async,
)
from chromeleon.core import (  # noqa: F401
    CREDENTIALS_METHOD,
    LAUNCH_ARGS,
    ProxySpec,
    check_registration,
    credentials_params,
    normalize_server,
    parse_proxy,
    proxy_registration,
)

__all__ = [
    "CREDENTIALS_METHOD",
    "LAUNCH_ARGS",
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
]
