# chromeleon

Client for driving the [Chromeleon](https://chromeleon.dev) browser. Pure Python,
no runtime dependencies, and it never imports your driver — so it neither pins a
Playwright version nor conflicts with the one you already have.

Chromeleon itself is a licensed product; this package drives it, it does not
contain it.

```bash
pip install chromeleon
```

## Quickstart

```python
from playwright.sync_api import sync_playwright
from chromeleon import launch

with sync_playwright() as p:
    browser = launch(p.chromium, "/path/to/chromeleon/chrome")
    page = browser.new_page()
    page.goto("https://example.com")
    browser.close()
```

`launch` returns a normal Playwright `Browser`. It differs from
`chromium.launch` in exactly two ways, both things Playwright has no way to
know: it supplies the WebRTC handling policy a per-context proxy needs, and it
strips the controller's own `PROXY_*` variables from the browser environment.

## Authenticated proxies

Chromeleon attaches an authenticated proxy **per browser context**, through two
browser-level CDP commands that must be issued in order, on one connection, with
a byte-identical server string, and serialized against each other:

```
Target.setProxyCredentials  {proxyServer, username, password}
Target.createBrowserContext {proxyServer}
```

Playwright has no API shaped like that, so this does it for you:

```python
from chromeleon import launch, new_proxy_context

browser = launch(p.chromium, CHROMELEON)
context = new_proxy_context(browser, "http://user:pass@gateway:12321")
page = context.new_page()
```

`new_proxy_context_async` is the asyncio twin. Each returns a real Playwright
`BrowserContext`.

Chromeleon derives the persona's timezone, locale and language from the proxy's
exit-IP geolocation, so each context is internally consistent with where it
appears to be.

> Do **not** pass proxy credentials at launch. Chromeleon cannot authenticate
> its exit-IP probe from them, and rejects it — the exit IP would go unresolved,
> which unbinds the persona's geo and disables WebRTC masking.

## Other drivers

The handshake is a protocol fact, not a Playwright one. Selenium, Puppeteer, a
raw DevTools WebSocket, or a language with no Playwright binding all use the
core directly — give it any browser-level `send(method, params)`:

```python
from chromeleon import (CREDENTIALS_METHOD, check_registration,
                               credentials_params, proxy_registration)

with proxy_registration(connection, "http://user:pass@gateway:12321") as spec:
    check_registration(send(CREDENTIALS_METHOD, credentials_params(spec)))
    ctx_id = send("Target.createBrowserContext", {"proxyServer": spec.server})
```

`proxy_registration` holds the lock the single-use registration requires, and
normalizes the server string the same way Playwright will, so both commands
agree.

## Captcha solving

Chromeleon has a built-in reCAPTCHA/hCaptcha solver (models embedded in the
binary). Launch with `captcha=True` and it solves challenges **automatically**;
watch it over a page CDP session:

```python
from chromeleon import launch, enable_captcha, CAPTCHA_SOLVED, CAPTCHA_FAILED

browser = launch(p.chromium, CHROMELEON, captcha=True)
page = browser.new_page()
cdp = page.context.new_cdp_session(page)
enable_captcha(cdp)
cdp.on(CAPTCHA_SOLVED, lambda p: print("solved in", p["timeMs"], "ms"))
cdp.on(CAPTCHA_FAILED, lambda p: print("failed:", p["reason"]))
page.goto("https://example.com/with-a-recaptcha")
```

`solver_eval(cdp, expression, frame_url_contains="")` runs JS in the solver's
isolated world (pierces **closed** shadow roots); its result arrives on the
`SOLVER_EVAL_RESULT` event.

## Knowing when the page is done

Chromeleon settles a page in the **browser process**, on the main frame's
*rendered text*, sampled over Blink's inner-text channel — no injected script, no
isolated world, nothing registered on the document. Launch with it on, arm the
watch **around** the navigation, and ask what happened:

```python
from chromeleon import launch, settle_watch

browser = launch(p.chromium, CHROMELEON, page_settle=True)
page = browser.new_page()

with settle_watch(page) as watch:                 # attaches CDP BEFORE the nav
    page.goto(url, wait_until="commit")
    state = watch.wait(timeout_ms=30_000)

if state.blocked:                                 # a bot wall, not a page
    rotate_exit_and_retry()
else:
    html = page.content()
```

`async with settle_watch(page)` is the asyncio form of the same object (or spell
it `settle_watch_async(page)`); inside it, `await watch.wait(...)`.

The two phases are the API on purpose. The challenge header is recorded at
**commit** time by a tracker attached when the handler is constructed, so a
session attached after `goto` returns cannot see it and degrades silently to
status-code-only detection — a one-shot `wait_for_settle(page)` would be the
shape that cannot be used correctly, so there is not one.

`state` is a `SettleState`:

| field | |
|---|---|
| `outcome` | `"settled"` \| `"timeout"` \| `"challenge"` |
| `blocked` | `outcome == "challenge"` **or** `http_status` in 401/403/407/429/503 |
| `elapsed_ms` | from the browser's own clock when it settled |
| `text_length` | rendered characters at settle |
| `navigations` | documents committed during the wait |
| `http_status` | the committed document's status |
| `via` | `"waitForSettle"` \| `"networkAlmostIdle"` \| `"load"` \| `"timeout"` |

`outcome == "challenge"` is a **bot wall** — 12.5% of proxied navigations in
production measurement — and is never reported as a successful load. A wall
shorter than `minChars` comes back as `"timeout"` instead, but the status is
still 403/429/…, which is why `blocked` checks both.

### The fallback, and why this order

Launched without `--page-settle`, the command is `-32601 method not found`; the
watch falls back to Blink's `networkAlmostIdle` lifecycle signal (≤2 in-flight
requests for 500 ms) without raising, then to the `load` event, then reports
`via="timeout"`. `state.via` always says which path answered.

Measured over 960 navigations, 80 sites, 3 rounds:

| | direct p50 | direct never | proxied p50 | proxied never |
|---|---|---|---|---|
| `networkAlmostIdle` | 3,989 ms | 1% | 11,285 ms | **36%** |
| `Chromeleon.waitForSettle` | 7,306 ms | 6% | 12,108 ms | **7%** |

**Through a proxy — what this client is for — `networkAlmostIdle` never fires on
more than a third of loads**, so the command is the primary. Direct,
`networkAlmostIdle` is 1.8x faster for the same median content, so it stays
available: `settle_watch(page, prefer="networkAlmostIdle")` skips the command
outright, and is the documented choice for un-proxied work. On completeness
(direct), `waitForSettle` reproduced the final text exactly on 79% of loads,
`networkAlmostIdle` 59%, `load` 40%, `domcontentloaded` 7%.

Fallback events are counted **only** from the main frame, and only for a loader
that is not the incumbent's: `Page.setLifecycleEventsEnabled` replays a full
lifecycle for the about:blank you were sitting on (arming discards it), and
bbc.com/news emits lifecycle from 23 frames — first-across-all-frames reports
`networkIdle` at 699 ms where the main frame's real value is 6094 ms.

`page_settle=True` is opt-in because it changes how the browser behaves; it is
not in `LAUNCH_ARGS`. Tune it with a dict — `page_settle={"quiet_window_ms":
2500, "min_chars": 400, "timeout_ms": 20000, "sample_interval_ms": 100,
"pierce_shadow": True}` — or build the flags yourself with
`settle_launch_args(...)`. Per call, `watch.wait(timeout_ms=…,
quiet_window_ms=…, min_chars=…)` overrides them for that navigation.

## API

| | |
|---|---|
| `launch(chromium, executable_path, **kw)` | Playwright `Browser`, correctly flagged |
| `new_proxy_context(browser, proxy, **kw)` | `BrowserContext` behind an authenticated proxy |
| `new_proxy_context_async(...)` | asyncio twin |
| `proxy_registration(connection, proxy)` | the transport-agnostic core |
| `credentials_params(spec)` / `check_registration(result)` | build and check the registration |
| `parse_proxy(proxy)` / `normalize_server(server)` | URL, dict or `ProxySpec` → canonical form |
| `LAUNCH_ARGS` | flags a per-context proxy needs |
| `launch(..., captcha=True)` | turn on the built-in captcha solver |
| `enable_captcha(cdp)` / `disable_captcha(cdp)` | `Chromeleon` captcha lifecycle events |
| `solver_eval(cdp, expr, frame="")` | eval in the solver's isolated world (pierces closed shadow roots) |
| `settle_watch(page)` / `settle_watch_async(page)` | arm a page-completion watch around a navigation |
| `watch.wait(timeout_ms=30000)` | `SettleState`: `outcome`, `blocked`, `elapsed_ms`, `text_length`, `via`, … |
| `launch(..., page_settle=True)` | register `Chromeleon.waitForSettle` (opt-in) |
| `settle_launch_args(**tuning)` | the `--page-settle*` flags, if you launch the binary yourself |

`proxy` may be a URL (`http://user:pass@host:port`, scheme optional), a
Playwright-style dict, or a `ProxySpec`. Credentials inside a URL are
percent-decoded; credentials given explicitly in a dict are taken literally.
