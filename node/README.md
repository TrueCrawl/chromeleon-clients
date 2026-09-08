# chromeleon (Node)

A thin, **fully-async** Node client for Chromeleon's per-context proxy
handshake. Playwright, Puppeteer, or any raw CDP connection. It does not wrap
`launch` or own your browser — objects go in and come back out unchanged.

See [`../HANDSHAKE.md`](../HANDSHAKE.md) for the protocol this implements.

## Install

```sh
npm i chromeleon
# plus your driver, e.g.  npm i playwright
```

## Playwright

```js
const { chromium } = require('playwright');
const { launch, newProxyContext } = require('chromeleon');

const browser = await launch(chromium, '/path/to/chromeleon');           // LAUNCH_ARGS + clean env
const ctx = await newProxyContext(browser, 'http://user:pass@gateway:12321');
const page = await ctx.newPage();
await page.goto('https://api.ipify.org?format=json');
```

`newProxyContext` runs `Target.setProxyCredentials` → `Target.createBrowserContext`
under a per-`(browser, server)` async lock, so firing several at once is safe:

```js
const [a, b] = await Promise.all([
  newProxyContext(browser, PROXY_A),
  newProxyContext(browser, PROXY_B),   // different server: runs concurrently
]);
```

## Puppeteer

```js
const { newProxyContextPuppeteer } = require('chromeleon');
const ctx = await newProxyContextPuppeteer(browser, 'http://user:pass@gw:12321');
```

## Any other driver

Supply a browser-level `send` and a `createContext`; the handshake runs under
the lock:

```js
const { newProxyContextWith } = require('chromeleon');
const ctxId = await newProxyContextWith(conn, proxy, {
  send: (m, p) => conn.send(m, p),
  createContext: (server) => conn.send('Target.createBrowserContext', { proxyServer: server }),
});
```

…or drive the core yourself:

```js
const { withProxyRegistration, CREDENTIALS_METHOD, credentialsParams, checkRegistration } = require('chromeleon');
await withProxyRegistration(conn, proxy, async (spec) => {
  checkRegistration(await send(CREDENTIALS_METHOD, credentialsParams(spec)));
  return send('Target.createBrowserContext', { proxyServer: spec.server });
});
```

## Captcha solving

Chromeleon ships a built-in reCAPTCHA/hCaptcha solver (audio reCAPTCHA v2/Enterprise
+ Turnstile / hCaptcha / DataDome / PerimeterX). Launch with `captcha: true` and it
solves challenges **automatically** — you don't call it. The models are embedded in
the release binary. The `Chromeleon` CDP domain only reports the lifecycle (its
events are visible to the DevTools session, not the page):

```js
const { chromium } = require('playwright');
const {
  launch, enableCaptcha, CAPTCHA_SOLVED, CAPTCHA_FAILED,
} = require('chromeleon');

const browser = await launch(chromium, '/path/to/chromeleon', { captcha: true });
const page = await browser.newPage();
const cdp = await page.context().newCDPSession(page);
await enableCaptcha(cdp);
cdp.on(CAPTCHA_SOLVED, (p) => console.log('solved in', Math.round(p.timeMs), 'ms'));
cdp.on(CAPTCHA_FAILED, (p) => console.log('failed:', p.reason));
await page.goto('https://example.com/with-a-recaptcha');
```

Events: `CAPTCHA_DETECTED` `{sitekey}`, `CAPTCHA_SOLVING` `{sitekey, method}`,
`CAPTCHA_SOLVED` `{sitekey, attempts, timeMs}`, `CAPTCHA_FAILED` `{sitekey, attempts,
reason}`. `solverEval(cdp, expression, frameUrlContains?)` runs JS in the solver's
isolated world (pierces **closed** shadow roots); its string result arrives on the
`SOLVER_EVAL_RESULT` event. `captchaModelPath` is a dev/self-host launch override.
Full example: `examples/captcha.mjs`.

## Page completion

Two ways to know a page is finished, in the order the measurements put them.

**`Chromeleon.waitForSettle`** is the primary: the browser process samples the
main frame's *rendered text* over Blink's inner-text channel and settles when it
has been unchanged for a quiet window. No JavaScript runs in the page, no
isolated world, nothing is registered on the document. It needs `--page-settle`
at launch (`pageSettle: true`), or the domain is not registered and the command
answers `-32601`.

**`networkAlmostIdle`** (at most 2 in-flight requests sustained 500ms) is the
fallback, taken automatically when the command is unavailable.

```js
const { chromium } = require('playwright');
const { launch, newProxyContext, settleWatch } = require('chromeleon');

const browser = await launch(chromium, '/path/to/chromeleon', { pageSettle: true });
const ctx = await newProxyContext(browser, 'http://user:pass@gateway:12321');
const page = await ctx.newPage();

const watch = await settleWatch(page);              // ATTACH BEFORE NAVIGATING
await page.goto(url, { waitUntil: 'commit' });
const state = await watch.wait({ timeoutMs: 30000 });
await watch.close();

if (state.blocked) throw new Error(`bot wall (${state.httpStatus})`);
console.log(state.outcome, state.via, state.elapsedMs, state.textLength);
```

The two phases are not a style choice. The challenge header is recorded at
**commit** time by a tracker attached when the handler is constructed, so a
session attached to an already-committed document cannot see it and silently
degrades to status-code-only detection — a one-shot helper called after `goto`
misses everything. Arming also swallows the lifecycle replay
`Page.setLifecycleEventsEnabled` emits for the incumbent `about:blank`; keep
those and every fallback signal reads as ~0ms.

`state` is a `SettleState`:

| field | |
|---|---|
| `outcome` | `"settled"` \| `"timeout"` \| `"challenge"` |
| `via` | `"waitForSettle"` \| `"networkAlmostIdle"` \| `"load"` \| `"timeout"` |
| `elapsedMs` | ms to settle — the browser's own clock on the primary path |
| `textLength` | rendered characters at settle (primary path only) |
| `httpStatus` | the committed document's status (primary path only) |
| `navigations` | documents committed in the main frame |
| `blocked` | `outcome === "challenge"` **or** `httpStatus` in 401/403/407/429/503 |

**Read `blocked`, not `outcome`.** `"challenge"` is a bot wall — 12.5% of proxied
navigations in production measurement — and never a successful load; but a wall
*shorter* than `minChars` reports `"timeout"` while still carrying its 403, so
the classification is outcome **or** status.

`wait()` never throws because a page did not settle (on proxied traffic that is
normal), never runs past `timeoutMs`, and is memoized — a second call returns the
first call's state. The session is detached on every exit path; `close()` is
idempotent, and a session you hand in (`settleWatch(page, { session })`, e.g. the
one already carrying the captcha events) is used as-is and never detached.

Why this order — 960 navigations, 80 sites, 3 rounds:

| | direct p50 | direct never | proxied p50 | proxied never |
|---|---|---|---|---|
| `networkAlmostIdle` | 3,989ms | 1% | 11,285ms | 36% |
| `Chromeleon.waitForSettle` | 7,306ms | 6% | 12,108ms | 7% |

Through a proxy — what this client is for — `networkAlmostIdle` fails to fire on
more than a third of loads and `waitForSettle` on 7%. On a **direct** connection
`networkAlmostIdle` is 1.8x faster for the same median content, which is why it
stays available and is the documented choice for un-proxied work: ask for it with
`settleWatch(page, { prefer: 'networkAlmostIdle' })`, or launch without
`pageSettle` and take the automatic fallback — either way `via` says which
signal answered. On completeness, direct:
`waitForSettle` reproduced the final text exactly on 79% of loads,
`networkAlmostIdle` 59%, the load event 40%, `domcontentloaded` 7%.

Fallback signals are counted only from the **main** frame and only for a
loaderId that is not the incumbent document's: bbc.com/news emits lifecycle from
23 frames, and first-across-all reports `networkIdle` at 699ms where the main
frame's real value is 6094ms.

`settleLaunchArgs({quietWindowMs, minChars, timeoutMs, sampleIntervalMs,
pierceShadow})` builds the flags if you launch the browser yourself; `pageSettle`
accepts the same object. `--page-settle` is deliberately **not** in `LAUNCH_ARGS`
— it changes browser behaviour, so it is opt-in. Full example:
`examples/settle.mjs`.

## API

- `launch(chromium, executablePath, options?) → Promise<Browser>` — merges
  `LAUNCH_ARGS`, strips `PROXY_*` env; `captcha` / `captchaModelPath` turn on the
  solver, `pageSettle` registers `Chromeleon.waitForSettle`; every other option
  passes through.
- `newProxyContext(browser, proxy, contextOptions?) → Promise<BrowserContext>`
- `newProxyContextPuppeteer(browser, proxy) → Promise<BrowserContext>`
- `newProxyContextWith(connection, proxy, { send, createContext }) → Promise<any>`
- `withProxyRegistration(connection, proxy, body) → Promise<T>`
- captcha: `enableCaptcha(cdp)`, `disableCaptcha(cdp)`,
  `solverEval(cdp, expression, frameUrlContains?)`, the `CAPTCHA_*` /
  `SOLVER_EVAL_RESULT` event constants, `captchaLaunchArgs`, `solverEvalParams`.
- page completion: `settleWatch(page, {session, prefer, replayWindowMs, ...}?) →
  Promise<SettleWatch>` (arm before `goto`),
  `watch.wait({timeoutMs, quietWindowMs, minChars}) → Promise<SettleState>`,
  `watch.close()`, `settleLaunchArgs(options?)`,
  `SettleState` / `SettleWatch`, `waitForSettleParams`, `BLOCKED_STATUSES`,
  `WAIT_FOR_SETTLE_METHOD`, the `PAGE_SETTLE_*` switches, the `OUTCOME_*` /
  `VIA_*` names, `REPLAY_WINDOW_MS`, `DEFAULT_TIMEOUT_MS`.
- core helpers: `parseProxy`, `normalizeServer`, `credentialsParams`,
  `checkRegistration`, `LAUNCH_ARGS`, `CREDENTIALS_METHOD`, `ProxySpec`,
  `browserProcessEnv`.

`proxy` is a URL string (`http://user:pass@host:port`) or
`{ server, username, password }`. URL credentials are percent-decoded.

## Test

```sh
npm test    # node --test: pure-core + async-mutex, no browser needed
```
