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

## API

- `launch(chromium, executablePath, options?) → Promise<Browser>` — merges
  `LAUNCH_ARGS`, strips `PROXY_*` env; every other option passes through.
- `newProxyContext(browser, proxy, contextOptions?) → Promise<BrowserContext>`
- `newProxyContextPuppeteer(browser, proxy) → Promise<BrowserContext>`
- `newProxyContextWith(connection, proxy, { send, createContext }) → Promise<any>`
- `withProxyRegistration(connection, proxy, body) → Promise<T>`
- core helpers: `parseProxy`, `normalizeServer`, `credentialsParams`,
  `checkRegistration`, `LAUNCH_ARGS`, `CREDENTIALS_METHOD`, `ProxySpec`,
  `browserProcessEnv`.

`proxy` is a URL string (`http://user:pass@host:port`) or
`{ server, username, password }`. URL credentials are percent-decoded.

## Test

```sh
npm test    # node --test: pure-core + async-mutex, no browser needed
```
