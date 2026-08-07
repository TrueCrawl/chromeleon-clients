"""Per-driver adapters over the transport-agnostic core.

Each is a few lines: get a browser-level CDP send function, send the
registration, create the context with the same server string — all inside the
core's lock. Adding a driver means adding a function here, not touching the core.

Playwright objects go in and come back out unchanged — ``launch`` returns a real
``Browser``, ``new_proxy_context`` a real ``BrowserContext``. Nothing here owns
your browser or introduces types you have to learn.

``launch`` exists only to supply the two things Chromeleon needs and Playwright
cannot know about: the WebRTC handling policy a per-context proxy requires, and
a browser environment free of the controller's own PROXY_* variables. It does
not police launch-time proxy credentials — the binary fails closed on those
itself, with a remediation banner.
"""
from __future__ import annotations

from typing import Any

from chromeleon.core import (
    CREDENTIALS_METHOD,
    DISABLE_METHOD,
    ENABLE_METHOD,
    LAUNCH_ARGS,
    SOLVER_EVAL_METHOD,
    captcha_launch_args,
    check_registration,
    credentials_params,
    proxy_registration,
    solver_eval_params,
)

__all__ = ["launch", "new_proxy_context", "new_proxy_context_async",
           "browser_process_env",
           "enable_captcha", "disable_captcha", "solver_eval"]


def launch(chromium: Any, executable_path: str, *,
           args: list[str] | None = None,
           env: dict[str, str] | None = None,
           captcha: bool = False,
           captcha_model_path: str | None = None,
           **launch_options: Any) -> Any:
    """Launch Chromeleon. Returns a normal Playwright ``Browser``.

        browser = launch(p.chromium, CHROMELEON)
        context = new_proxy_context(browser, "http://user:pass@gateway:12321")

    Equivalent to ``chromium.launch(...)`` plus the two things Playwright has no
    way to know: LAUNCH_ARGS merged in (unless you already set that policy), and
    the controller's PROXY_* variables stripped from the browser environment.
    Every other keyword is passed straight through.

    ``captcha=True`` turns on the built-in reCAPTCHA/hCaptcha solver, which then
    solves challenges automatically; observe it over CDP with
    :func:`enable_captcha` and the ``CAPTCHA_*`` events. ``captcha_model_path``
    is a dev/self-host override (the release binary embeds the models) and
    implies ``captcha``. A flag you set yourself in ``args`` always wins.
    """
    merged = list(args or [])
    extra = list(LAUNCH_ARGS)
    if captcha or captcha_model_path is not None:
        extra.extend(captcha_launch_args(captcha_model_path))
    for flag in extra:
        switch = flag.split("=", 1)[0]
        if not any(a.split("=", 1)[0] == switch for a in merged):
            merged.append(flag)
    return chromium.launch(executable_path=executable_path, args=merged,
                           env=browser_process_env(env), **launch_options)


def enable_captcha(cdp: Any) -> Any:
    """Start Chromeleon captcha lifecycle events on a page CDP session.

        cdp = page.context.new_cdp_session(page)   # ``await`` under async
        enable_captcha(cdp)
        cdp.on(CAPTCHA_SOLVED, on_solved)

    The solver runs on its own; this only turns on the notifications so you can
    watch ``captchaDetected -> captchaSolving -> captchaSolved | captchaFailed``.
    Returns the driver's send result (a coroutine to ``await`` under the async
    API), so the same call works sync and async.
    """
    return cdp.send(ENABLE_METHOD)


def disable_captcha(cdp: Any) -> Any:
    """Stop Chromeleon captcha event notifications on a page CDP session."""
    return cdp.send(DISABLE_METHOD)


def solver_eval(cdp: Any, expression: str, frame_url_contains: str = "") -> Any:
    """Evaluate JS in the solver's isolated world (pierces CLOSED shadow roots).

    The string result arrives asynchronously as a ``SOLVER_EVAL_RESULT`` event,
    not as this call's return. ``frame_url_contains`` targets a subframe by URL
    substring; the empty string is the primary main frame.
    """
    return cdp.send(SOLVER_EVAL_METHOD,
                    solver_eval_params(expression, frame_url_contains))


def browser_process_env(env: dict[str, str] | None = None) -> dict[str, str]:
    """Launch environment with controller-side proxy variables removed.

    HTTP_PROXY / PROXY_* in the controller's environment are for the CONTROLLER.
    Inheriting them routes browser traffic somewhere you did not choose.
    """
    import os

    source = os.environ if env is None else env
    return {k: v for k, v in source.items() if "PROXY" not in str(k).upper()}


def new_proxy_context(browser: Any, proxy: Any, **context_options: Any) -> Any:
    """Playwright (sync): a context behind an authenticated per-context proxy.

        browser = chromium.launch(executable_path=..., args=list(LAUNCH_ARGS))
        context = new_proxy_context(browser, "http://user:pass@gateway:12321")
    """
    with proxy_registration(browser, proxy) as spec:
        session = browser.new_browser_cdp_session()
        try:
            check_registration(
                session.send(CREDENTIALS_METHOD, credentials_params(spec)))
        finally:
            session.detach()
        # Still inside the lock: this call consumes the registration.
        return browser.new_context(proxy={"server": spec.server},
                                   **context_options)


async def new_proxy_context_async(browser: Any, proxy: Any,
                                  **context_options: Any) -> Any:
    """Playwright (async): the same handshake, awaited."""
    with proxy_registration(browser, proxy) as spec:
        session = await browser.new_browser_cdp_session()
        try:
            check_registration(
                await session.send(CREDENTIALS_METHOD, credentials_params(spec)))
        finally:
            await session.detach()
        return await browser.new_context(proxy={"server": spec.server},
                                         **context_options)
