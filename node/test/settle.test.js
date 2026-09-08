'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const {
  BLOCKED_STATUSES,
  LIFECYCLE_EVENT,
  NETWORK_ALMOST_IDLE,
  OUTCOME_SETTLED,
  PAGE_SETTLE_SWITCH,
  SETTLE_OUTCOMES,
  VIA_NETWORK_ALMOST_IDLE,
  VIA_WAIT_FOR_SETTLE,
  WAIT_FOR_SETTLE_METHOD,
  SettleState,
  settleLaunchArgs,
  settleWatch,
  waitForSettleParams,
} = require('../src/settle');
const { LAUNCH_ARGS } = require('../src/core');

const MAIN = 'FRAME-MAIN';
const SUB = 'FRAME-SUB';
const STALE_LOADER = 'LOADER-ABOUT-BLANK';
const LOADER = 'LOADER-REAL';

/**
 * Minimal page CDP session double, in the shape the proxy handshake is tested
 * in: it records what was sent, lets a test answer a command, and lets a test
 * push `Page.lifecycleEvent` in like the browser would.
 *
 * `Page.setLifecycleEventsEnabled` replays the incumbent about:blank's whole
 * lifecycle, exactly as the browser does — that replay is the trap the arming
 * pause exists for, so the double has to reproduce it.
 */
function fakeSession(handlers = {}) {
  const seen = {
    sent: [],
    detached: 0,
    listeners: new Map(),
    lifecycleListenersWhenEnabled: 0,
  };
  const session = {
    seen,
    async send(method, params) {
      seen.sent.push({ method, params });
      if (method === 'Page.setLifecycleEventsEnabled') {
        seen.lifecycleListenersWhenEnabled = (seen.listeners.get(LIFECYCLE_EVENT) || []).length;
      }
      if (Object.prototype.hasOwnProperty.call(handlers, method)) {
        const handler = handlers[method];
        return typeof handler === 'function' ? handler(params, session) : handler;
      }
      if (method === 'Page.getFrameTree') {
        return { frameTree: { frame: { id: MAIN } } };
      }
      return {};
    },
    on(event, fn) {
      const list = seen.listeners.get(event) || [];
      list.push(fn);
      seen.listeners.set(event, list);
    },
    off(event, fn) {
      const list = (seen.listeners.get(event) || []).filter((f) => f !== fn);
      seen.listeners.set(event, list);
    },
    async detach() {
      seen.detached += 1;
    },
    /** Push one lifecycle event, as the browser would. */
    emit(params) {
      for (const fn of [...(seen.listeners.get(LIFECYCLE_EVENT) || [])]) fn(params);
    },
    /** The about:blank replay `setLifecycleEventsEnabled` emits. */
    replayAboutBlank() {
      for (const name of ['init', 'firstPaint', 'DOMContentLoaded', 'load',
        NETWORK_ALMOST_IDLE, 'networkIdle']) {
        session.emit({ frameId: MAIN, loaderId: STALE_LOADER, name, timestamp: 1.0 });
      }
    },
  };
  return session;
}

/** A Playwright-shaped Page whose context hands out `session`. */
function fakePage(session) {
  return { context: () => ({ newCDPSession: async () => session }) };
}

/** Every test arms with a short pause; the default is 350ms of real time. */
const ARM = { replayWindowMs: 2 };

function methodNotFound() {
  const err = new Error(
    `Protocol error (${WAIT_FOR_SETTLE_METHOD}): '${WAIT_FOR_SETTLE_METHOD}' wasn't found`,
  );
  err.code = -32601;
  return err;
}

// --- launch flags ---------------------------------------------------------

test('settleLaunchArgs is just --page-settle by default', () => {
  assert.deepEqual(settleLaunchArgs(), [PAGE_SETTLE_SWITCH]);
  assert.deepEqual(settleLaunchArgs(), ['--page-settle']);
});

test('settleLaunchArgs appends only the tuning switches that were given', () => {
  assert.deepEqual(
    settleLaunchArgs({ quietWindowMs: 2500, minChars: 120, timeoutMs: 20000,
      sampleIntervalMs: 250, pierceShadow: true }),
    ['--page-settle', '--page-settle-quiet-window-ms=2500', '--page-settle-min-chars=120',
      '--page-settle-timeout-ms=20000', '--page-settle-sample-interval-ms=250',
      '--page-settle-pierce-shadow'],
  );
  assert.deepEqual(settleLaunchArgs({ quietWindowMs: 0 }),
    ['--page-settle', '--page-settle-quiet-window-ms=0']);
  assert.deepEqual(settleLaunchArgs({ pierceShadow: false }), ['--page-settle']);
});

test('--page-settle is opt-in: never in the default LAUNCH_ARGS', () => {
  // It changes browser behaviour (a sampler runs per navigation), so it must be
  // asked for, exactly like the captcha solver.
  assert.ok(!LAUNCH_ARGS.some((a) => a.startsWith('--page-settle')));
});

test('launch({pageSettle}) adds the flags and keeps them off by default', async () => {
  const { launch } = require('../src/adapters');
  const calls = [];
  const chromium = { async launch(options) { calls.push(options); return {}; } };

  await launch(chromium, '/bin/chromeleon');
  assert.ok(!calls[0].args.some((a) => a.startsWith('--page-settle')));

  await launch(chromium, '/bin/chromeleon', { pageSettle: true });
  assert.ok(calls[1].args.includes('--page-settle'));
  assert.ok(calls[1].args.includes(LAUNCH_ARGS[0]), 'WebRTC policy still merged');
  assert.equal(calls[1].pageSettle, undefined, 'our option must not reach the driver');

  await launch(chromium, '/bin/chromeleon', { pageSettle: { quietWindowMs: 2000 } });
  assert.ok(calls[2].args.includes('--page-settle'));
  assert.ok(calls[2].args.includes('--page-settle-quiet-window-ms=2000'));

  await launch(chromium, '/bin/chromeleon', { pageSettle: true, args: ['--page-settle'] });
  assert.equal(calls[3].args.filter((a) => a === '--page-settle').length, 1);
});

// --- arming ---------------------------------------------------------------

test('arming enables Page, reads the MAIN frame, then turns lifecycle on', async () => {
  const session = fakeSession();
  const watch = await settleWatch(fakePage(session), ARM);
  assert.deepEqual(session.seen.sent.map((s) => s.method),
    ['Page.enable', 'Page.getFrameTree', 'Page.setLifecycleEventsEnabled']);
  assert.deepEqual(session.seen.sent[2].params, { enabled: true });
  assert.equal(watch.mainFrameId, MAIN);
  // Subscribed BEFORE the enable, or the about:blank replay is invisible and
  // cannot be discarded.
  assert.equal(session.seen.lifecycleListenersWhenEnabled, 1);
  await watch.close();
});

test('a failed arm still detaches the session it attached', async () => {
  const session = fakeSession({
    'Page.getFrameTree': () => { throw new Error('target closed'); },
  });
  await assert.rejects(settleWatch(fakePage(session), ARM), /target closed/);
  assert.equal(session.seen.detached, 1);
});

// --- the primary path: Chromeleon.waitForSettle ---------------------------

test('a settled reply is returned verbatim', async () => {
  const reply = {
    outcome: 'settled', elapsedMs: 7306, textLength: 48211,
    navigations: 2, httpStatus: 200, reason: '',
  };
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: reply });
  const watch = await settleWatch(fakePage(session), ARM);
  const state = await watch.wait({ timeoutMs: 5000, quietWindowMs: 4000, minChars: 200 });

  assert.equal(state.outcome, 'settled');
  assert.equal(state.via, 'waitForSettle');
  assert.equal(state.elapsedMs, 7306);
  assert.equal(state.textLength, 48211);
  assert.equal(state.navigations, 2);
  assert.equal(state.httpStatus, 200);
  assert.equal(state.blocked, false);
  assert.equal(state.raw, reply, 'the reply is kept as sent');

  const [call] = session.seen.sent.filter((s) => s.method === WAIT_FOR_SETTLE_METHOD);
  assert.equal(call.params.quietWindowMs, 4000);
  assert.equal(call.params.minChars, 200);
  // The browser gets a slightly shorter budget than our own deadline, so its
  // answer — the only one carrying httpStatus — wins the race.
  assert.ok(call.params.timeoutMs > 0 && call.params.timeoutMs <= 5000);
});

test('tuning is omitted from the command when the caller did not give it', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 10 } });
  const watch = await settleWatch(fakePage(session), ARM);
  await watch.wait({ timeoutMs: 1000 });
  const [call] = session.seen.sent.filter((s) => s.method === WAIT_FOR_SETTLE_METHOD);
  // Absent keys leave the browser on its launch-switch defaults.
  assert.deepEqual(Object.keys(call.params), ['timeoutMs']);
});

test('a "challenge" outcome is blocked, not a load', async () => {
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: {
      outcome: 'challenge', elapsedMs: 1200, textLength: 940,
      navigations: 1, httpStatus: 403, reason: 'datadome',
    },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  const state = await watch.wait({ timeoutMs: 1000 });
  assert.equal(state.outcome, 'challenge');
  assert.equal(state.blocked, true);
  assert.equal(state.settled, false);
  assert.equal(state.toJSON().blocked, true);
});

test('a 403 that reports "timeout" is STILL blocked', async () => {
  // A wall shorter than min_chars never reaches "challenge"; the status is the
  // other half of the classification, so callers read `blocked`, not `outcome`.
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: { outcome: 'timeout', elapsedMs: 30000, textLength: 120, httpStatus: 403 },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  const state = await watch.wait({ timeoutMs: 1000 });
  assert.equal(state.outcome, 'timeout');
  assert.equal(state.blocked, true);
});

test('every blocking status classifies, and an ordinary one does not', () => {
  for (const status of BLOCKED_STATUSES) {
    assert.equal(
      new SettleState({ outcome: 'timeout', via: 'waitForSettle', httpStatus: status }).blocked,
      true, `status ${status} must classify as blocked`);
  }
  assert.deepEqual([...BLOCKED_STATUSES], [401, 403, 407, 429, 503]);
  assert.equal(
    new SettleState({ outcome: 'timeout', via: 'waitForSettle', httpStatus: 200 }).blocked, false);
});

test('a challenge with no status is still blocked', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'challenge', elapsedMs: 800 } });
  const watch = await settleWatch(fakePage(session), ARM);
  const state = await watch.wait({ timeoutMs: 1000 });
  assert.equal(state.httpStatus, null);
  assert.equal(state.blocked, true);
});

// --- the fallback: Blink networkAlmostIdle --------------------------------

test('-32601 falls back to networkAlmostIdle instead of raising', async () => {
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); },
    'Page.setLifecycleEventsEnabled': (_p, s) => { s.replayAboutBlank(); return {}; },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  const waiting = watch.wait({ timeoutMs: 2000 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: 'init', timestamp: 10.0 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: 'load', timestamp: 12.0 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: NETWORK_ALMOST_IDLE, timestamp: 16.094 });

  const state = await waiting;
  assert.equal(state.via, 'networkAlmostIdle');
  assert.equal(state.outcome, 'settled');
  assert.equal(state.blocked, false);
  assert.equal(state.elapsedMs, 6094, "the main frame's own clock, from its init");
  assert.equal(state.navigations, 1);
  assert.equal(state.textLength, null, 'the lifecycle channel carries no text');
  assert.equal(state.httpStatus, null);
  assert.match(state.reason, /--page-settle/);
});

test('a -32601 CDP envelope (raw socket) falls back too', async () => {
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: { error: { code: -32601, message: "'Chromeleon.waitForSettle' wasn't found" } },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  const waiting = watch.wait({ timeoutMs: 2000 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: NETWORK_ALMOST_IDLE, timestamp: 3.0 });
  assert.equal((await waiting).via, 'networkAlmostIdle');
});

test('the about:blank replay is discarded, so no signal reads as ~0ms', async () => {
  // setLifecycleEventsEnabled replays a FULL lifecycle for the incumbent
  // about:blank, load and networkAlmostIdle included. Keep those and every
  // fallback answer is instant and wrong.
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); },
    'Page.setLifecycleEventsEnabled': (_p, s) => { s.replayAboutBlank(); return {}; },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  const state = await watch.wait({ timeoutMs: 60 });
  assert.equal(state.via, 'timeout');
  assert.equal(state.outcome, 'timeout');
  assert.equal(state.navigations, null, 'the incumbent document is not a navigation');
});

test('a stale loaderId arriving late is still discarded', async () => {
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); },
    'Page.setLifecycleEventsEnabled': (_p, s) => { s.replayAboutBlank(); return {}; },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  const waiting = watch.wait({ timeoutMs: 120 });
  session.emit({ frameId: MAIN, loaderId: STALE_LOADER, name: NETWORK_ALMOST_IDLE, timestamp: 1.5 });
  assert.equal((await waiting).via, 'timeout');
});

test('subframe lifecycle events are ignored', async () => {
  // bbc.com/news emits lifecycle from 23 frames; first-across-all reports
  // networkIdle at 699ms where the main frame's real value is 6094ms.
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); } });
  const watch = await settleWatch(fakePage(session), ARM);
  const waiting = watch.wait({ timeoutMs: 80 });
  session.emit({ frameId: SUB, loaderId: 'LOADER-SUB', name: 'load', timestamp: 0.5 });
  session.emit({ frameId: SUB, loaderId: 'LOADER-SUB', name: NETWORK_ALMOST_IDLE, timestamp: 0.699 });
  const state = await waiting;
  assert.equal(state.via, 'timeout', 'a subframe must not settle the page');
  assert.equal(state.navigations, null);
});

test('the load event answers only when networkAlmostIdle never fires', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); } });
  const watch = await settleWatch(fakePage(session), ARM);
  const waiting = watch.wait({ timeoutMs: 60 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: 'init', timestamp: 2.0 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: 'load', timestamp: 3.5 });
  const state = await waiting;
  assert.equal(state.via, 'load');
  assert.equal(state.outcome, 'settled');
  assert.equal(state.elapsedMs, 1500);
  assert.match(state.reason, /networkAlmostIdle before the deadline/);
});

test('a redirect chain settles on the document the caller ends up with', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); } });
  const watch = await settleWatch(fakePage(session), ARM);
  const waiting = watch.wait({ timeoutMs: 2000 });
  session.emit({ frameId: MAIN, loaderId: 'LOADER-1', name: 'init', timestamp: 1.0 });
  session.emit({ frameId: MAIN, loaderId: 'LOADER-2', name: 'init', timestamp: 2.0 });
  session.emit({ frameId: MAIN, loaderId: 'LOADER-2', name: NETWORK_ALMOST_IDLE, timestamp: 5.0 });
  const state = await waiting;
  assert.equal(state.via, 'networkAlmostIdle');
  assert.equal(state.elapsedMs, 3000, 'measured from the SECOND navigation');
  assert.equal(state.navigations, 2);
});

// --- timeout, idempotence, detaching --------------------------------------

test('wait() honours its timeout when the command never answers', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: () => new Promise(() => {}) });
  const watch = await settleWatch(fakePage(session), ARM);
  const started = Date.now();
  const state = await watch.wait({ timeoutMs: 60 });
  const elapsed = Date.now() - started;
  assert.equal(state.outcome, 'timeout');
  assert.equal(state.via, 'timeout');
  assert.equal(state.blocked, false);
  assert.match(state.reason, /did not answer within timeoutMs/);
  assert.ok(elapsed >= 40 && elapsed < 5000, `returned in ${elapsed}ms`);
});

test('the fallback honours the same deadline', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); } });
  const watch = await settleWatch(fakePage(session), ARM);
  const started = Date.now();
  const state = await watch.wait({ timeoutMs: 60 });
  assert.equal(state.via, 'timeout');
  assert.ok(Date.now() - started < 5000);
});

test('wait() twice returns the same state and detaches once', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 42 } });
  const watch = await settleWatch(fakePage(session), ARM);
  const first = await watch.wait({ timeoutMs: 1000 });
  const second = await watch.wait({ timeoutMs: 1000 });
  assert.equal(first, second);
  assert.equal(watch.state, first);
  assert.equal(session.seen.sent.filter((s) => s.method === WAIT_FOR_SETTLE_METHOD).length, 1);
  assert.equal(session.seen.detached, 1);
  await watch.close();
  await watch.close();
  assert.equal(session.seen.detached, 1, 'close() is idempotent');
});

test('the session is detached and unsubscribed on every exit path', async () => {
  for (const handler of [
    { [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 1 } },
    { [WAIT_FOR_SETTLE_METHOD]: () => { throw methodNotFound(); } },
    { [WAIT_FOR_SETTLE_METHOD]: () => new Promise(() => {}) },
  ]) {
    const session = fakeSession(handler);
    const watch = await settleWatch(fakePage(session), ARM);
    await watch.wait({ timeoutMs: 30 });
    assert.equal(session.seen.detached, 1);
    assert.equal((session.seen.listeners.get(LIFECYCLE_EVENT) || []).length, 0);
  }
});

test('a protocol error that is NOT -32601 surfaces, and still detaches', async () => {
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: () => { throw new Error('Protocol error: Target closed'); },
  });
  const watch = await settleWatch(fakePage(session), ARM);
  await assert.rejects(watch.wait({ timeoutMs: 500 }), /Target closed/);
  assert.equal(session.seen.detached, 1);
});

test('a session the caller owns is used as-is and never detached', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 5 } });
  const watch = await settleWatch(session, ARM); // a CDP session, not a Page
  assert.equal(watch.session, session);
  await watch.wait({ timeoutMs: 500 });
  assert.equal(session.seen.detached, 0);
  assert.equal((session.seen.listeners.get(LIFECYCLE_EVENT) || []).length, 0,
    'still unsubscribed, just not detached');
});

test('a Puppeteer-shaped page attaches through createCDPSession', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 3 } });
  const page = { createCDPSession: async () => session };
  const watch = await settleWatch(page, ARM);
  assert.equal(watch.mainFrameId, MAIN);
  await watch.wait({ timeoutMs: 500 });
  assert.equal(session.seen.detached, 1);
});

test('settleWatch refuses an object that is no page and no session', async () => {
  await assert.rejects(settleWatch({}, ARM), /Page or a CDP session/);
  await assert.rejects(settleWatch(null, ARM), /Page or a CDP session/);
});

test('watch-level tuning is the default for wait()', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 1 } });
  const watch = await settleWatch(fakePage(session), { ...ARM, quietWindowMs: 1500, minChars: 64 });
  await watch.wait({ timeoutMs: 900 });
  const [call] = session.seen.sent.filter((s) => s.method === WAIT_FOR_SETTLE_METHOD);
  assert.equal(call.params.quietWindowMs, 1500);
  assert.equal(call.params.minChars, 64);
});

// --- the caller asking for the fallback -----------------------------------

test("prefer:'networkAlmostIdle' skips the command entirely", async () => {
  // 1.8x faster for the same median content, and only sane un-proxied: the
  // lifecycle signal misses 1% of direct loads and 36% of proxied ones.
  const session = fakeSession({
    [WAIT_FOR_SETTLE_METHOD]: () => { throw new Error('must not be sent'); },
  });
  const watch = await settleWatch(fakePage(session), { ...ARM, prefer: 'networkAlmostIdle' });
  const waiting = watch.wait({ timeoutMs: 2000 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: 'init', timestamp: 1.0 });
  session.emit({ frameId: MAIN, loaderId: LOADER, name: NETWORK_ALMOST_IDLE, timestamp: 4.0 });

  const state = await waiting;
  assert.equal(state.via, 'networkAlmostIdle');
  assert.equal(state.elapsedMs, 3000);
  assert.ok(!session.seen.sent.some((s) => s.method === WAIT_FOR_SETTLE_METHOD));
  assert.match(state.reason, /prefer/);
});

test('prefer must name a signal we can actually wait on', async () => {
  const session = fakeSession();
  await assert.rejects(
    settleWatch(fakePage(session), { ...ARM, prefer: 'load' }),
    /prefer must be/);
  // Refused BEFORE anything is attached, so there is nothing to detach and
  // nothing left open either.
  assert.deepEqual(session.seen.sent, [], 'no session was ever attached');
  assert.equal(session.seen.detached, 0);
});

test('a session passed in explicitly is reused, not opened or detached', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 9 } });
  const page = { context: () => { throw new Error('must not open a second session'); } };
  const watch = await settleWatch(page, { ...ARM, session });
  const state = await watch.wait({ timeoutMs: 500 });
  assert.equal(state.elapsedMs, 9);
  assert.equal(session.seen.detached, 0);
});

// --- the params helper ----------------------------------------------------

test('waitForSettleParams omits what it was not given', () => {
  assert.deepEqual(waitForSettleParams(), {});
  assert.deepEqual(waitForSettleParams(4000, null, 30000), { quietWindowMs: 4000, timeoutMs: 30000 });
  assert.deepEqual(waitForSettleParams(null, 0, null), { minChars: 0 });
});

test('a settled page behind a wall is blocked, and not "settled"', () => {
  // The text can settle and the document still be a 403 wall; `settled` is the
  // conjunction, `blocked` the classification.
  const wall = new SettleState({
    outcome: OUTCOME_SETTLED, via: VIA_WAIT_FOR_SETTLE, httpStatus: 403, textLength: 900,
  });
  assert.equal(wall.blocked, true);
  assert.equal(wall.settled, false);
  const page = new SettleState({
    outcome: OUTCOME_SETTLED, via: VIA_WAIT_FOR_SETTLE, httpStatus: 200,
  });
  assert.equal(page.settled, true);
  assert.deepEqual([...SETTLE_OUTCOMES], ['settled', 'timeout', 'challenge']);
  assert.equal(VIA_NETWORK_ALMOST_IDLE, 'networkAlmostIdle');
});

test('wait() refuses on a watch that was never armed', async () => {
  // The whole point of the two phases: a session attached after the document
  // committed cannot see the challenge header, so a watch that skipped arming
  // must say so rather than answer with a plausible number.
  const { SettleWatch } = require('../src/settle');
  const watch = new SettleWatch(fakeSession(), {});
  await assert.rejects(watch.wait({ timeoutMs: 10 }), /not armed/);
});

test('the memoized state still comes back after close()', async () => {
  const session = fakeSession({ [WAIT_FOR_SETTLE_METHOD]: { outcome: 'settled', elapsedMs: 7 } });
  const watch = await settleWatch(fakePage(session), ARM);
  const state = await watch.wait({ timeoutMs: 500 });
  await watch.close();
  assert.equal(await watch.wait({ timeoutMs: 500 }), state);
});
