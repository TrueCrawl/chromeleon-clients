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
 */
const core = require('./core');
const adapters = require('./adapters');

module.exports = {
  // core protocol
  LAUNCH_ARGS: core.LAUNCH_ARGS,
  CREDENTIALS_METHOD: core.CREDENTIALS_METHOD,
  ProxySpec: core.ProxySpec,
  normalizeServer: core.normalizeServer,
  parseProxy: core.parseProxy,
  credentialsParams: core.credentialsParams,
  checkRegistration: core.checkRegistration,
  withProxyRegistration: core.withProxyRegistration,
  // driver adapters
  launch: adapters.launch,
  browserProcessEnv: adapters.browserProcessEnv,
  newProxyContext: adapters.newProxyContext,
  newProxyContextPuppeteer: adapters.newProxyContextPuppeteer,
  newProxyContextWith: adapters.newProxyContextWith,
};
