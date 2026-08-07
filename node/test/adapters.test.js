'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { newProxyContext } = require('../src/adapters');
const { CREDENTIALS_METHOD } = require('../src/core');

/** Minimal Playwright Browser double that records the handshake. */
function fakeBrowser() {
  const seen = { sent: [], contextOptions: null, detached: 0 };
  return {
    seen,
    async newBrowserCDPSession() {
      return {
        async send(method, params) {
          seen.sent.push({ method, params });
          return {};
        },
        async detach() {
          seen.detached += 1;
        },
      };
    },
    async newContext(options) {
      seen.contextOptions = options;
      return { __context: true };
    },
  };
}

test('newProxyContext registers and creates with the SAME normalized server', async () => {
  const browser = fakeBrowser();
  // Port 80 is the trap: Playwright rewrites the context's server to drop it,
  // so the registration has to be the dropped form or the browser refuses.
  await newProxyContext(browser, 'http://user:pass@GW.Example.com:80');

  const [reg] = browser.seen.sent;
  assert.equal(reg.method, CREDENTIALS_METHOD);
  assert.equal(reg.params.proxyServer, 'http://gw.example.com');
  assert.equal(reg.params.username, 'user');
  assert.equal(reg.params.password, 'pass');
  assert.equal(browser.seen.contextOptions.proxy.server, reg.params.proxyServer);
  // Credentials never travel on the context.
  assert.equal(browser.seen.contextOptions.proxy.username, undefined);
  assert.equal(browser.seen.contextOptions.proxy.password, undefined);
  assert.equal(browser.seen.detached, 1);
});

test('contextOptions cannot override the normalized proxy', async () => {
  const browser = fakeBrowser();
  // A caller reusing a contextOptions object that still carries a stale proxy
  // key must not silently unbind the registration it just consumed.
  await newProxyContext(browser, 'http://user:pass@gw.example.com:12321', {
    viewport: { width: 1920, height: 1080 },
    proxy: { server: 'http://stale.example.com:9999' },
  });

  const [reg] = browser.seen.sent;
  assert.equal(browser.seen.contextOptions.proxy.server, 'http://gw.example.com:12321');
  assert.equal(browser.seen.contextOptions.proxy.server, reg.params.proxyServer);
  assert.deepEqual(browser.seen.contextOptions.viewport, { width: 1920, height: 1080 });
});
