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

## API

- `launch(chromium, executablePath, options?) → Promise<Browser>` — merges
  `LAUNCH_ARGS`, strips `PROXY_*` env; `captcha` / `captchaModelPath` turn on the
  solver; every other option passes through.
- `newProxyContext(browser, proxy, contextOptions?) → Promise<BrowserContext>`
- `newProxyContextPuppeteer(browser, proxy) → Promise<BrowserContext>`
- `newProxyContextWith(connection, proxy, { send, createContext }) → Promise<any>`
- `withProxyRegistration(connection, proxy, body) → Promise<T>`
- captcha: `enableCaptcha(cdp)`, `disableCaptcha(cdp)`,
  `solverEval(cdp, expression, frameUrlContains?)`, the `CAPTCHA_*` /
  `SOLVER_EVAL_RESULT` event constants, `captchaLaunchArgs`, `solverEvalParams`.
- core helpers: `parseProxy`, `normalizeServer`, `credentialsParams`,
  `checkRegistration`, `LAUNCH_ARGS`, `CREDENTIALS_METHOD`, `ProxySpec`,
  `browserProcessEnv`.

`proxy` is a URL string (`http://user:pass@host:port`) or
`{ server, username, password }`. URL credentials are percent-decoded.

## Test

```sh
npm test    # node --test: pure-core + async-mutex, no browser needed
```
