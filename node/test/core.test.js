'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const {
  LAUNCH_ARGS,
  CREDENTIALS_METHOD,
  ProxySpec,
  normalizeServer,
  parseProxy,
  credentialsParams,
  checkRegistration,
  withProxyRegistration,
} = require('../src/core');

test('LAUNCH_ARGS is the WebRTC per-context policy', () => {
  assert.deepEqual([...LAUNCH_ARGS], [
    '--webrtc-ip-handling-policy=disable_non_proxied_udp',
  ]);
  assert.equal(CREDENTIALS_METHOD, 'Target.setProxyCredentials');
});

test('normalizeServer matches Playwright: scheme prepend, lowercase, default-port drop', () => {
  assert.equal(normalizeServer('GATEWAY.example.com:12321'), 'http://gateway.example.com:12321');
  assert.equal(normalizeServer('http://Gateway.EXAMPLE.com:80'), 'http://gateway.example.com');
  assert.equal(normalizeServer('https://gw.example.com:443'), 'https://gw.example.com');
  assert.equal(normalizeServer('http://gw:8080'), 'http://gw:8080');
  // byte-identity: the server we send == the one Playwright would send
  assert.equal(normalizeServer('gw.example.com:12321'), normalizeServer('http://gw.example.com:12321'));
});

test('parseProxy: URL string splits + percent-decodes credentials', () => {
  const s = parseProxy('http://user:p%40ss%3Aword@gw.example.com:12321');
  assert.equal(s.server, 'http://gw.example.com:12321');
  assert.equal(s.username, 'user');
  assert.equal(s.password, 'p@ss:word'); // %40 -> @, %3A -> :
  assert.equal(s.authenticated, true);
});

test('parseProxy: object form', () => {
  const s = parseProxy({ server: 'GW.example.com:12321', username: 'u', password: 'p' });
  assert.equal(s.server, 'http://gw.example.com:12321');
  assert.equal(s.username, 'u');
  assert.equal(s.password, 'p');
  assert.equal(s.authenticated, true);
});

test('parseProxy: authenticated semantics — no colon means no password (unauthenticated)', () => {
  assert.equal(parseProxy('http://user@gw:12321').authenticated, false); // username only
  assert.equal(parseProxy('http://user:@gw:12321').authenticated, true); // explicit empty pw
  assert.equal(parseProxy('http://gw:12321').authenticated, false); // no creds
  const bare = parseProxy('http://user@gw:12321');
  assert.equal(bare.password, null);
});

test('parseProxy: ProxySpec passes through', () => {
  const spec = new ProxySpec('http://gw:1', 'u', 'p');
  assert.equal(parseProxy(spec), spec);
});

test('credentialsParams shape', () => {
  const spec = parseProxy('http://u:p@gw:12321');
  assert.deepEqual(credentialsParams(spec), {
    proxyServer: 'http://gw:12321', username: 'u', password: 'p',
  });
  // null creds render as empty strings on the wire
  const bare = new ProxySpec('http://gw:1');
  assert.deepEqual(credentialsParams(bare), { proxyServer: 'http://gw:1', username: '', password: '' });
});

test('checkRegistration throws only on a CDP error envelope', () => {
  assert.doesNotThrow(() => checkRegistration({}));
  assert.doesNotThrow(() => checkRegistration(undefined));
  assert.throws(() => checkRegistration({ error: { code: -32000, message: 'nope' } }),
    /registration refused/);
});

test('withProxyRegistration rejects an unauthenticated or non-HTTP proxy', async () => {
  await assert.rejects(
    withProxyRegistration({}, 'http://gw:12321', async () => 'x'),
    /needs a username and password/);
  await assert.rejects(
    withProxyRegistration({}, 'socks5://u:p@gw:1080', async () => 'x'),
    /requires an HTTP\(S\) proxy/);
});

test('withProxyRegistration passes the parsed spec and returns the body value', async () => {
  const out = await withProxyRegistration({}, 'http://u:p@gw:12321', async (spec) => {
    assert.equal(spec.server, 'http://gw:12321');
    assert.equal(spec.username, 'u');
    return 'context-id';
  });
  assert.equal(out, 'context-id');
});

test('withProxyRegistration serializes register/create for the same (connection, server)', async () => {
  const browser = { id: 'b1' };
  const proxy = 'http://u:p@gw:12321';
  const events = [];
  const one = withProxyRegistration(browser, proxy, async () => {
    events.push('A:register');
    await new Promise((r) => setTimeout(r, 30)); // A holds across the "create"
    events.push('A:create');
  });
  // B starts while A is mid-handshake; it must wait for A to finish.
  const two = withProxyRegistration(browser, proxy, async () => {
    events.push('B:register');
    events.push('B:create');
  });
  await Promise.all([one, two]);
  // No interleaving: A's pair completes before B's begins.
  assert.deepEqual(events, ['A:register', 'A:create', 'B:register', 'B:create']);
});

test('withProxyRegistration does NOT serialize different servers on the same connection', async () => {
  const browser = { id: 'b1' };
  const order = [];
  const slow = withProxyRegistration(browser, 'http://u:p@gw-a:1', async () => {
    await new Promise((r) => setTimeout(r, 30));
    order.push('slow');
  });
  const fast = withProxyRegistration(browser, 'http://u:p@gw-b:2', async () => {
    order.push('fast');
  });
  await Promise.all([slow, fast]);
  assert.deepEqual(order, ['fast', 'slow']); // different servers run concurrently
});

test('withProxyRegistration releases the lock even when the body throws', async () => {
  const browser = { id: 'b1' };
  const proxy = 'http://u:p@gw:12321';
  await assert.rejects(
    withProxyRegistration(browser, proxy, async () => { throw new Error('boom'); }),
    /boom/);
  // the next acquirer must still proceed (lock was released)
  const ok = await withProxyRegistration(browser, proxy, async () => 'recovered');
  assert.equal(ok, 'recovered');
});
