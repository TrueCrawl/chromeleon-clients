'use strict';
/**
 * A thin, fully-async client for driving Chromeleon from Node.
 *
 * Chromeleon attaches authenticated proxies per browser context with two
 * browser-level CDP commands. That is a protocol fact; the core here knows only
 * the protocol and the async lock it needs, and each driver is a short adapter.
 *
 * Playwright:
 *
 *     const { chromium } = require('playwright');
 *     const { launch, newProxyContext } = require('chromeleon');
 *
 *     const browser = await launch(chromium, CHROMELEON);
 *     const context = await newProxyContext(browser, 'http://user:pass@gw:12321');
 *     const page = await context.newPage();
 *
 * Any other driver uses the core directly:
 *
 *     const {
 *       CREDENTIALS_METHOD, credentialsParams, checkRegistration,
 *       withProxyRegistration,
 *     } = require('chromeleon');
 *
 *     await withProxyRegistration(conn, proxy, async (spec) => {
 *       checkRegistration(await send(CREDENTIALS_METHOD, credentialsParams(spec)));
 *       return send('Target.createBrowserContext', { proxyServer: spec.server });
 *     });
 *
 * The built-in captcha solver runs on its own once enabled at launch; the
 * `Chromeleon` CDP domain re-exported here only observes and steers it.
 */
const {
  LAUNCH_ARGS,
  CREDENTIALS_METHOD,
  ProxySpec,
  normalizeServer,
  parseProxy,
  credentialsParams,
  checkRegistration,
  withProxyRegistration,
  CAPTCHA_SOLVER_SWITCH,
  CAPTCHA_MODEL_PATH_SWITCH,
  ENABLE_METHOD,
  DISABLE_METHOD,
  SOLVER_EVAL_METHOD,
  CAPTCHA_DETECTED,
  CAPTCHA_SOLVING,
  CAPTCHA_SOLVED,
  CAPTCHA_FAILED,
  SOLVER_EVAL_RESULT,
  CAPTCHA_EVENTS,
  captchaLaunchArgs,
  solverEvalParams,
} = require('./core');
const {
  launch,
  browserProcessEnv,
  newProxyContext,
  newProxyContextPuppeteer,
  newProxyContextWith,
  enableCaptcha,
  disableCaptcha,
  solverEval,
} = require('./adapters');

// Destructured into locals and re-exported as shorthand on purpose. Node lets
// ESM name-import from a CommonJS module only when cjs-module-lexer can detect
// the export names statically, and it cannot see through member expressions:
// `module.exports = { launch: adapters.launch }` yields a module whose ONLY
// usable ESM shape is the default import. Shorthand identifiers are detected.
module.exports = {
  // core protocol
  LAUNCH_ARGS,
  CREDENTIALS_METHOD,
  ProxySpec,
  normalizeServer,
  parseProxy,
  credentialsParams,
  checkRegistration,
  withProxyRegistration,
  // driver adapters
  launch,
  browserProcessEnv,
  newProxyContext,
  newProxyContextPuppeteer,
  newProxyContextWith,
  // captcha solver (Chromeleon CDP domain)
  CAPTCHA_SOLVER_SWITCH,
  CAPTCHA_MODEL_PATH_SWITCH,
  ENABLE_METHOD,
  DISABLE_METHOD,
  SOLVER_EVAL_METHOD,
  CAPTCHA_DETECTED,
  CAPTCHA_SOLVING,
  CAPTCHA_SOLVED,
  CAPTCHA_FAILED,
  SOLVER_EVAL_RESULT,
  CAPTCHA_EVENTS,
  captchaLaunchArgs,
  solverEvalParams,
  enableCaptcha,
  disableCaptcha,
  solverEval,
};
