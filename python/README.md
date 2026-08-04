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

`proxy` may be a URL (`http://user:pass@host:port`, scheme optional), a
Playwright-style dict, or a `ProxySpec`. Credentials inside a URL are
percent-decoded; credentials given explicitly in a dict are taken literally.
