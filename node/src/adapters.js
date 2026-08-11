'use strict';
/**
 * Per-driver adapters over the transport-agnostic core. Each is a few lines:
 * get a browser-level CDP `send`, register the credentials, create the context
 * with the same server string — all inside the core's async lock. Adding a
 * driver means adding a function here, not touching the core.
 *
 * Everything is async, because every JS browser binding is. Playwright/Puppeteer
 * objects go in and come back out unchanged; nothing here owns your browser.
 *
 * `launch` exists only to supply the two things Chromeleon needs and the driver
 * cannot know: the WebRTC handling policy a per-context proxy requires, and a
 * browser environment free of the controller's own PROXY_* variables. It does
 * NOT police launch-time proxy credentials — the binary fails closed on those
 * itself, with a remediation banner.
 */

const {
  LAUNCH_ARGS,
  SUPPRESSED_DEFAULT_ARGS,
  CREDENTIALS_METHOD,
  ENABLE_METHOD,
  DISABLE_METHOD,
  SOLVER_EVAL_METHOD,
  captchaLaunchArgs,
  credentialsParams,
  checkRegistration,
  solverEvalParams,
  withProxyRegistration,
} = require('./core');

/**
 * Launch environment with controller-side proxy variables removed. HTTP_PROXY /
 * PROXY_* in the controller's environment are for the CONTROLLER; inheriting
 * them routes browser traffic somewhere you did not choose.
 * @param {Object<string,string>=} env  defaults to process.env
 * @returns {Object<string,string>}
 */
function browserProcessEnv(env) {
  const source = env || process.env;
  const out = {};
  for (const [k, v] of Object.entries(source)) {
    if (!String(k).toUpperCase().includes('PROXY')) out[k] = v;
  }
  return out;
}

function _mergeLaunchArgs(args, extra) {
  const merged = Array.isArray(args) ? args.slice() : [];
  for (const flag of extra || LAUNCH_ARGS) {
    const switchName = flag.split('=', 1)[0];
    if (!merged.some((a) => a.split('=', 1)[0] === switchName)) merged.push(flag);
  }
  return merged;
}

/**
 * Launch Chromeleon with Playwright. Returns a normal Playwright `Browser`.
 *
 *     const { chromium } = require('playwright');
 *     const browser = await launch(chromium, CHROMELEON);
 *     const context = await newProxyContext(browser, 'http://user:pass@gw:12321');
 *
 * Equivalent to `chromium.launch(...)` plus LAUNCH_ARGS merged in (unless you
 * already set that policy) and the controller's PROXY_* variables stripped from
 * the browser environment. Every other option is passed straight through.
 *
 * `captcha: true` turns on the built-in reCAPTCHA/hCaptcha solver, which then
 * solves challenges automatically; observe it over CDP with {@link enableCaptcha}
 * and the `CAPTCHA_*` events. `captchaModelPath` is a dev/self-host override (the
 * release binary embeds the models) and implies `captcha`. A flag you already
 * set in `args` always wins.
 * @param {*} chromium  playwright's `chromium` browser type
 * @param {string} executablePath  path to the Chromeleon binary
 * @param {object=} options  { args, env, captcha, captchaModelPath, ...launchOptions }
 * @returns {Promise<*>}  Playwright Browser
 */
async function launch(chromium, executablePath, options = {}) {
  const { args, env, captcha, captchaModelPath, ...launchOptions } = options;
  const extra = LAUNCH_ARGS.slice();
  if (captcha || captchaModelPath != null) {
    extra.push(...captchaLaunchArgs(captchaModelPath == null ? null : captchaModelPath));
  }
  return chromium.launch({
    executablePath,
    args: _mergeLaunchArgs(args, extra),
    env: browserProcessEnv(env),
    ignoreDefaultArgs: SUPPRESSED_DEFAULT_ARGS.slice(),
    ...launchOptions,
  });
}

/**
 * Start Chromeleon captcha lifecycle events on a page CDP session.
 *
 *     const cdp = await page.context().newCDPSession(page);
 *     await enableCaptcha(cdp);
 *     cdp.on(CAPTCHA_SOLVED, (p) => console.log('solved in', p.timeMs, 'ms'));
 *     cdp.on(CAPTCHA_FAILED, (p) => console.log('failed:', p.reason));
 *
 * The solver runs on its own; this only turns on the notifications so you can
 * watch `captchaDetected -> captchaSolving -> captchaSolved | captchaFailed`.
 * @param {*} cdp  a page CDP session (Playwright CDPSession / Puppeteer)
 * @returns {Promise<*>}
 */
function enableCaptcha(cdp) {
  return cdp.send(ENABLE_METHOD);
}

/**
 * Stop Chromeleon captcha event notifications on a page CDP session.
 * @param {*} cdp
 * @returns {Promise<*>}
 */
function disableCaptcha(cdp) {
  return cdp.send(DISABLE_METHOD);
}

/**
 * Evaluate JS in the solver's isolated world (pierces CLOSED shadow roots). The
 * string result arrives asynchronously as a `SOLVER_EVAL_RESULT` event, not this
 * call's return. `frameUrlContains` targets a subframe by URL substring; the
 * empty string is the primary main frame.
 * @param {*} cdp
 * @param {string} expression
 * @param {string=} frameUrlContains
 * @returns {Promise<*>}
 */
function solverEval(cdp, expression, frameUrlContains = '') {
  return cdp.send(SOLVER_EVAL_METHOD, solverEvalParams(expression, frameUrlContains));
}

/**
 * Playwright: a context behind an authenticated per-context proxy.
 *
 *     const context = await newProxyContext(browser, 'http://user:pass@gw:12321');
 *     const page = await context.newPage();
 *
 * @param {*} browser  a Playwright Browser
 * @param {string|object} proxy  URL string or {server, username, password}
 * @param {object=} contextOptions  passed to browser.newContext (minus proxy)
 * @returns {Promise<*>}  Playwright BrowserContext
 */
async function newProxyContext(browser, proxy, contextOptions = {}) {
  return withProxyRegistration(browser, proxy, async (spec) => {
    const session = await browser.newBrowserCDPSession();
    try {
      checkRegistration(
        await session.send(CREDENTIALS_METHOD, credentialsParams(spec)),
      );
    } finally {
      await session.detach();
    }
    // Still inside the lock: this call consumes the registration. `proxy` goes
    // LAST so a caller's contextOptions cannot override the normalized server —
    // spreading it after would silently substitute an unnormalized (or absent)
    // proxy and leave the registration unconsumed.
    return browser.newContext({ ...contextOptions, proxy: { server: spec.server } });
  });
}

/**
 * Puppeteer: the same handshake. Puppeteer creates the incognito context first,
 * so the browser-level registration and the context creation are both awaited
 * inside the lock, with the server string byte-identical in both.
 *
 *     const context = await newProxyContextPuppeteer(browser, proxy);
 *     const page = await context.newPage();
 *
 * @param {*} browser  a Puppeteer Browser
 * @param {string|object} proxy
 * @returns {Promise<*>}  Puppeteer BrowserContext
 */
async function newProxyContextPuppeteer(browser, proxy) {
  return withProxyRegistration(browser, proxy, async (spec) => {
    const session = await browser.target().createCDPSession();
    try {
      checkRegistration(
        await session.send(CREDENTIALS_METHOD, credentialsParams(spec)),
      );
    } finally {
      await session.detach();
    }
    const createContext =
      browser.createBrowserContext || browser.createIncognitoBrowserContext;
    return createContext.call(browser, { proxyServer: spec.server });
  });
}

/**
 * Any other driver — a raw DevTools WebSocket, a binding with no context helper.
 * You supply a browser-level `send(method, params) => Promise` and a
 * `createContext(server) => Promise`; the handshake is run under the lock.
 *
 *     const ctxId = await newProxyContextWith(conn, proxy, {
 *       send: (m, p) => conn.send(m, p),
 *       createContext: (server) =>
 *         conn.send('Target.createBrowserContext', { proxyServer: server }),
 *     });
 *
 * @param {*} connection  identifies the CDP connection (locking only)
 * @param {string|object} proxy
 * @param {{send: (m:string,p:object)=>Promise<*>, createContext: (server:string)=>Promise<*>}} driver
 * @returns {Promise<*>}
 */
async function newProxyContextWith(connection, proxy, driver) {
  return withProxyRegistration(connection, proxy, async (spec) => {
    checkRegistration(await driver.send(CREDENTIALS_METHOD, credentialsParams(spec)));
    return driver.createContext(spec.server);
  });
}

module.exports = {
  launch,
  browserProcessEnv,
  newProxyContext,
  newProxyContextPuppeteer,
  newProxyContextWith,
  enableCaptcha,
  disableCaptcha,
  solverEval,
};
