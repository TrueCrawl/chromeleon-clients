'use strict';
/**
 * Page completion: is the page finished, and did a bot wall answer instead?
 *
 * The primary signal is Chromeleon's OWN CDP command. `Chromeleon.waitForSettle`
 * settles when the main frame's RENDERED TEXT has been unchanged for a quiet
 * window, sampled in the browser process over Blink's inner-text channel: no
 * JavaScript runs in the page, no isolated world, nothing registered on the
 * document. That is the point of it over a MutationObserver — the page cannot
 * see the measurement.
 *
 * The fallback is Blink's own lifecycle: `networkAlmostIdle` is at most 2
 * in-flight requests sustained for 500ms (`networkIdle` is the same rule at 0).
 * It is exposed only as a `Page.lifecycleEvent`, never as a command.
 *
 * Why THAT order — measured over 960 navigations, 80 sites, 3 rounds:
 *
 *                              direct p50 | never | proxied p50 | never
 *     networkAlmostIdle           3,989ms |    1% |    11,285ms |   36%
 *     Chromeleon.waitForSettle    7,306ms |    6% |    12,108ms |    7%
 *
 * THROUGH A PROXY — which is what this client is for — networkAlmostIdle fails
 * to fire on more than a third of loads, while waitForSettle misses 7%. That is
 * the whole argument for the ordering. On a DIRECT connection networkAlmostIdle
 * is 1.8x faster for the same median content, so it stays available and is the
 * documented choice for un-proxied work. On completeness (direct), waitForSettle
 * reproduced the final text exactly on 79% of loads, networkAlmostIdle 59%, the
 * load event 40%, domcontentloaded 7%.
 *
 * The API is two-phase on purpose, so the fatal ordering mistake is structurally
 * impossible:
 *
 *     const watch = await settleWatch(page);         // attaches CDP BEFORE nav
 *     await page.goto(url, { waitUntil: 'commit' });
 *     const state = await watch.wait({ timeoutMs: 30000 });
 *     await watch.close();
 *
 * A one-shot helper called AFTER goto silently misses everything: the challenge
 * header is recorded at COMMIT time by a tracker attached when the handler is
 * constructed, so a session attached to an already-committed document cannot see
 * it and degrades to status-code-only detection without saying so.
 *
 * `outcome: "challenge"` means a BOT WALL — 12.5% of proxied navigations in
 * production measurement — and is never a successful load. A wall shorter than
 * `minChars` reports "timeout" instead, but `httpStatus` still carries the
 * 401/403/407/429/503, so {@link SettleState#blocked} classifies on outcome OR
 * status and callers should read that rather than the outcome alone.
 *
 * The command exists only when the browser was launched with `--page-settle`
 * ({@link settleLaunchArgs}); without it the domain is not registered and the
 * command answers -32601, which this module treats as "use the fallback", not as
 * an error.
 */

// --- launch switches ------------------------------------------------------
//
// Opt-in, exactly like the captcha solver: `--page-settle` changes browser
// behaviour (a sampler runs per navigation), so it is NOT in LAUNCH_ARGS.

/** Registers the `Chromeleon.waitForSettle` command. Required; opt-in. */
const PAGE_SETTLE_SWITCH = '--page-settle';
/** Quiet window the rendered text must hold still for. Binary default: 4000. */
const PAGE_SETTLE_QUIET_WINDOW_SWITCH = '--page-settle-quiet-window-ms';
/** Minimum rendered characters before a settle counts as content. */
const PAGE_SETTLE_MIN_CHARS_SWITCH = '--page-settle-min-chars';
/** Browser-side ceiling for one wait. */
const PAGE_SETTLE_TIMEOUT_SWITCH = '--page-settle-timeout-ms';
/** How often the browser samples the rendered text. */
const PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH = '--page-settle-sample-interval-ms';
/** Include text inside shadow roots in the sample. */
const PAGE_SETTLE_PIERCE_SHADOW_SWITCH = '--page-settle-pierce-shadow';

// --- protocol -------------------------------------------------------------

/** The command, sent on a PAGE CDP session (as the captcha domain is). */
const WAIT_FOR_SETTLE_METHOD = 'Chromeleon.waitForSettle';
/** Blink's fallback channel. Neither idle name is a command; both only arrive here. */
const LIFECYCLE_EVENT = 'Page.lifecycleEvent';
/** At most 2 in-flight requests sustained 500ms. */
const NETWORK_ALMOST_IDLE = 'networkAlmostIdle';
/** The same rule at 0 in-flight requests. */
const NETWORK_IDLE = 'networkIdle';
/** The load event, the weakest completion signal we will report. */
const LOAD_EVENT = 'load';

/** `outcome` values the command can return. */
const OUTCOME_SETTLED = 'settled';
const OUTCOME_TIMEOUT = 'timeout';
const OUTCOME_CHALLENGE = 'challenge';
const SETTLE_OUTCOMES = Object.freeze([OUTCOME_SETTLED, OUTCOME_TIMEOUT, OUTCOME_CHALLENGE]);

/** `via` values — WHICH signal produced the answer, in preference order. */
const VIA_WAIT_FOR_SETTLE = 'waitForSettle';
const VIA_NETWORK_ALMOST_IDLE = 'networkAlmostIdle';
const VIA_LOAD = 'load';
const VIA_TIMEOUT = 'timeout';

/**
 * Statuses a bot wall answers with. A wall shorter than `minChars` never
 * reaches "challenge", so the status is the second half of the classification.
 * @type {readonly number[]}
 */
const BLOCKED_STATUSES = Object.freeze([401, 403, 407, 429, 503]);

/**
 * How long to let `Page.setLifecycleEventsEnabled` REPLAY the incumbent
 * about:blank's lifecycle before believing anything. Keep the replay and every
 * fallback signal reads as ~0ms — an instant, wrong "settled".
 */
const REPLAY_WINDOW_MS = 350;

/** Overall budget for one {@link SettleWatch#wait} when the caller names none. */
const DEFAULT_TIMEOUT_MS = 30000;

// Shaved off the command's own timeoutMs so the browser's answer — which carries
// elapsedMs, httpStatus and the challenge classification — arrives BEFORE our
// client-side cutoff instead of racing it to the same instant.
const _REPLY_GRACE_MS = 250;

// Playwright raises with the protocol message and no code; a raw DevTools socket
// hands back {"error": {"code": -32601}}. Both mean "launched without
// --page-settle", and neither is a reason to fail a scrape.
const _NOT_FOUND_MARKERS = [
  '-32601',
  "wasn't found",
  'was not found',
  'method not found',
  'not implemented',
  'unknown method',
];

const TIMED_OUT = Symbol('chromeleon.settle.deadline');

/**
 * The two signals a watch can be told to wait on. Checked BEFORE anything is
 * attached, so a typo cannot leak a CDP session.
 */
function _checkPrefer(prefer) {
  const value = prefer == null ? VIA_WAIT_FOR_SETTLE : String(prefer);
  if (value !== VIA_WAIT_FOR_SETTLE && value !== VIA_NETWORK_ALMOST_IDLE) {
    throw new TypeError(
      `prefer must be '${VIA_WAIT_FOR_SETTLE}' or '${VIA_NETWORK_ALMOST_IDLE}', ` +
      `got ${JSON.stringify(prefer)}`,
    );
  }
  return value;
}

function _num(value) {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (value == null || value === '') return null;
  const n = Number(value);
  return Number.isFinite(n) ? n : null;
}

function _firstNumber(...values) {
  for (const value of values) {
    const n = _num(value);
    if (n != null) return n;
  }
  return null;
}

/**
 * Launch flags that register `Chromeleon.waitForSettle`. `--page-settle` alone
 * is enough; the rest tune the sampler and are only worth setting when the
 * defaults do not fit the corpus.
 *
 *     const browser = await launch(chromium, CHROMELEON, { pageSettle: true });
 *     // or, hand-rolled:
 *     chromium.launch({ args: settleLaunchArgs({ quietWindowMs: 2500 }) });
 *
 * @param {{quietWindowMs?: number, minChars?: number, timeoutMs?: number,
 *          sampleIntervalMs?: number, pierceShadow?: boolean}=} options
 * @returns {string[]}
 */
function settleLaunchArgs(options = {}) {
  const opts = options || {};
  const args = [PAGE_SETTLE_SWITCH];
  const pairs = [
    [PAGE_SETTLE_QUIET_WINDOW_SWITCH, opts.quietWindowMs],
    [PAGE_SETTLE_MIN_CHARS_SWITCH, opts.minChars],
    [PAGE_SETTLE_TIMEOUT_SWITCH, opts.timeoutMs],
    [PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH, opts.sampleIntervalMs],
  ];
  for (const [flag, value] of pairs) {
    if (value != null) args.push(`${flag}=${value}`);
  }
  if (opts.pierceShadow) args.push(PAGE_SETTLE_PIERCE_SHADOW_SWITCH);
  return args;
}

/**
 * Params for `Chromeleon.waitForSettle`. Every field is optional and an omitted
 * one keeps the launch-time default, so a missing value is not sent as `null` —
 * it is not sent at all.
 * @param {?number=} quietWindowMs
 * @param {?number=} minChars
 * @param {?number=} timeoutMs
 * @returns {{quietWindowMs?: number, minChars?: number, timeoutMs?: number}}
 */
function waitForSettleParams(quietWindowMs = null, minChars = null, timeoutMs = null) {
  const params = {};
  if (_num(quietWindowMs) != null) params.quietWindowMs = _num(quietWindowMs);
  if (_num(minChars) != null) params.minChars = _num(minChars);
  if (_num(timeoutMs) != null) params.timeoutMs = _num(timeoutMs);
  return params;
}

/**
 * What one wait() concluded. Immutable; `raw` keeps the browser's reply as sent
 * so a field added by a newer binary is not lost on the way through.
 *
 * `outcome` is the browser's verdict — `"settled" | "timeout" | "challenge"`.
 * `via` is the mechanism that produced it, which is a different question:
 * `"waitForSettle"` is the primary path, `"networkAlmostIdle"` and `"load"` are
 * the two fallback signals (in descending order of completeness: 59% vs 40% of
 * loads reproduce the final text exactly), and `"timeout"` means nothing fired.
 * A fallback answer reports `outcome: "settled"` when a signal did fire — the
 * page completed, we just could not measure the text — and carries no
 * `httpStatus`, because only the browser-side command reads the committed
 * document's status.
 */
class SettleState {
  /**
   * @param {{outcome: string, via: string, elapsedMs?: ?number,
   *          textLength?: ?number, httpStatus?: ?number, navigations?: ?number,
   *          reason?: ?string, raw?: ?object}} fields
   */
  constructor(fields) {
    const f = fields || {};
    this.outcome = String(f.outcome);
    this.via = String(f.via);
    this.elapsedMs = _num(f.elapsedMs);
    this.textLength = _num(f.textLength);
    this.httpStatus = _num(f.httpStatus);
    this.navigations = _num(f.navigations);
    this.reason = f.reason == null ? null : String(f.reason);
    this.raw = f.raw == null ? null : f.raw;
    Object.freeze(this);
  }

  /**
   * A bot wall answered, on either half of the evidence: the browser called it a
   * challenge, or the committed document carries a blocking status. Callers act
   * on THIS, not on `outcome === 'challenge'` alone — a wall shorter than
   * `minChars` reports "timeout" while still carrying its 403.
   * @returns {boolean}
   */
  get blocked() {
    return (
      this.outcome === OUTCOME_CHALLENGE ||
      (this.httpStatus != null && BLOCKED_STATUSES.includes(this.httpStatus))
    );
  }

  /** Some signal reported completion — and it was a page, not a wall. */
  get settled() {
    return this.outcome === OUTCOME_SETTLED && !this.blocked;
  }

  toJSON() {
    return {
      outcome: this.outcome,
      via: this.via,
      elapsedMs: this.elapsedMs,
      textLength: this.textLength,
      httpStatus: this.httpStatus,
      navigations: this.navigations,
      reason: this.reason,
      blocked: this.blocked,
    };
  }

  /** Build a state from a `Chromeleon.waitForSettle` reply, verbatim. */
  static fromReply(reply) {
    const r = reply && typeof reply === 'object' ? reply : {};
    return new SettleState({
      outcome: typeof r.outcome === 'string' ? r.outcome : OUTCOME_TIMEOUT,
      via: VIA_WAIT_FOR_SETTLE,
      elapsedMs: r.elapsedMs,
      textLength: r.textLength,
      httpStatus: r.httpStatus,
      navigations: r.navigations,
      reason: r.reason,
      raw: r,
    });
  }
}

/** True when the browser said "I have no such method", in any driver's dialect. */
function _isMethodNotFound(err) {
  if (!err) return false;
  const code = _num(
    (err && err.code) ||
    (err && err.response && err.response.code) ||
    (err && err.error && err.error.code),
  );
  if (code === -32601) return true;
  const message = String(
    (err && (err.message || err.originalMessage)) ||
    (err && err.error && err.error.message) || err || '',
  ).toLowerCase();
  // Matched against the markers only — never a loose /not found/, which would
  // swallow a real failure like a closed target.
  return _NOT_FOUND_MARKERS.some((marker) => message.includes(marker));
}

/**
 * Resolve to `TIMED_OUT` if `promise` has not answered by `deadline`. Both
 * outcomes are handled here, so a late rejection never escapes as an unhandled
 * one.
 */
function _byDeadline(promise, deadline) {
  return new Promise((resolve, reject) => {
    let done = false;
    const timer = setTimeout(() => {
      if (done) return;
      done = true;
      resolve(TIMED_OUT);
    }, Math.max(0, deadline - Date.now()));
    promise.then(
      (value) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        resolve(value);
      },
      (err) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        reject(err);
      },
    );
  });
}

/** Get a page CDP session out of whatever driver object the caller has. */
async function _cdpSessionFor(page) {
  if (!page || (typeof page !== 'object' && typeof page !== 'function')) {
    throw new TypeError(
      'settleWatch needs a Playwright/Puppeteer Page or a CDP session',
    );
  }
  // Playwright: the session is created on the context, for the page.
  if (typeof page.context === 'function') {
    const context = page.context();
    if (context && typeof context.newCDPSession === 'function') {
      return { session: await context.newCDPSession(page), owned: true };
    }
  }
  // Puppeteer: on the page itself, or on its target in older versions.
  if (typeof page.createCDPSession === 'function') {
    return { session: await page.createCDPSession(), owned: true };
  }
  if (typeof page.target === 'function') {
    const target = page.target();
    if (target && typeof target.createCDPSession === 'function') {
      return { session: await target.createCDPSession(), owned: true };
    }
  }
  // Already a CDP session. Used as-is and NOT detached on close: we did not
  // attach it, and the caller is still using it.
  if (typeof page.send === 'function' && typeof page.on === 'function') {
    return { session: page, owned: false };
  }
  throw new TypeError(
    'settleWatch needs a Playwright/Puppeteer Page or a CDP session, got ' +
    Object.prototype.toString.call(page),
  );
}

/**
 * An armed page-completion watch. Build one with {@link settleWatch} BEFORE
 * navigating; `wait()` after.
 */
class SettleWatch {
  /**
   * @param {*} session  a page CDP session
   * @param {{owned?: boolean, prefer?: string, quietWindowMs?: number,
   *          minChars?: number, timeoutMs?: number}=} options
   */
  constructor(session, options = {}) {
    const opts = options || {};
    this._session = session;
    this._owned = Boolean(opts.owned);
    this._prefer = _checkPrefer(opts.prefer);
    this._defaults = {
      quietWindowMs: _num(opts.quietWindowMs),
      minChars: _num(opts.minChars),
      timeoutMs: _num(opts.timeoutMs),
    };
    this._mainFrameId = null;
    this._stale = new Set(); // loaderIds from the incumbent document
    this._seen = new Map(); // lifecycle name -> {name, timestamp, at}
    this._waiters = new Set();
    this._loaderId = null;
    this._navigations = 0;
    this._navStart = null;
    this._armedAt = Date.now();
    this._arming = true;
    this._armed = false;
    this._subscribed = false;
    this._closed = false;
    this._state = null;
    this._pending = null;
    this._onLifecycle = this._onLifecycle.bind(this);
  }

  /** The frame every fallback signal must come from. */
  get mainFrameId() {
    return this._mainFrameId;
  }

  /** The CDP session in use (the caller's, or the one we attached). */
  get session() {
    return this._session;
  }

  /** The state `wait()` concluded with, or `null` before it has. */
  get state() {
    return this._state;
  }

  /**
   * Wait for the page to finish. Never raises because a page did not settle —
   * on proxied traffic that is a normal outcome — and never runs past
   * `timeoutMs`. Memoized: a second call returns the first call's state, even
   * after `close()`. Raises only for a watch that was never armed, or a
   * protocol error that is not "no such method".
   *
   * @param {{timeoutMs?: number, quietWindowMs?: number, minChars?: number}=} options
   * @returns {Promise<SettleState>}
   */
  async wait(options = {}) {
    if (this._state) return this._state;
    if (!this._armed) {
      throw new Error(
        'settle watch is not armed — build it with settleWatch(page) and call ' +
        'wait() while it is open, so the CDP session is attached before the ' +
        'navigation commits',
      );
    }
    if (this._pending) return this._pending;
    this._pending = this._wait(options || {}).finally(() => {
      this._pending = null;
    });
    return this._pending;
  }

  /**
   * Unsubscribe and detach. Idempotent, safe after a failed arm, and only ever
   * detaches a session this watch attached itself.
   * @returns {Promise<void>}
   */
  async close() {
    if (this._closed) return;
    this._closed = true;
    this._armed = false;
    if (this._subscribed) {
      this._subscribed = false;
      const off = this._session.off || this._session.removeListener;
      if (typeof off === 'function') {
        try {
          off.call(this._session, LIFECYCLE_EVENT, this._onLifecycle);
        } catch (_err) {
          /* an already-dead session has nothing to unsubscribe */
        }
      }
    }
    for (const wake of [...this._waiters]) {
      this._waiters.delete(wake);
      wake();
    }
    if (this._owned && this._session && typeof this._session.detach === 'function') {
      try {
        await this._session.detach();
      } catch (_err) {
        /* the page (or the browser) may already be gone */
      }
    }
  }

  // --- arming -------------------------------------------------------------

  async _arm(replayWindowMs) {
    const session = this._session;
    await session.send('Page.enable');
    const tree = await session.send('Page.getFrameTree');
    const frame = tree && tree.frameTree ? tree.frameTree.frame : null;
    this._mainFrameId = frame && frame.id != null ? frame.id : null;
    // Subscribe BEFORE enabling: the enable call replays the incumbent
    // document's lifecycle synchronously, and those are the events we have to
    // see in order to discard them.
    session.on(LIFECYCLE_EVENT, this._onLifecycle);
    this._subscribed = true;
    await session.send('Page.setLifecycleEventsEnabled', { enabled: true });
    if (replayWindowMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, replayWindowMs));
    }
    this._arming = false;
    this._armed = true;
    this._armedAt = Date.now();
  }

  _onLifecycle(params) {
    const p = params || {};
    if (this._arming) {
      // Everything up to the end of the arming pause belongs to the incumbent
      // about:blank. Its loaderIds are the stale set.
      if (p.loaderId != null) this._stale.add(p.loaderId);
      return;
    }
    // Rule: the main frame only. bbc.com/news emits lifecycle from 23 frames,
    // and first-across-all reports networkIdle at 699ms where the main frame's
    // real value is 6094ms.
    if (this._mainFrameId != null && p.frameId !== this._mainFrameId) return;
    if (p.loaderId != null && this._stale.has(p.loaderId)) return;
    if (p.loaderId != null && p.loaderId !== this._loaderId) {
      // A document we have not seen before: count it and start over, so a
      // redirect chain settles on the document the caller ends up with.
      this._loaderId = p.loaderId;
      this._navigations += 1;
      this._seen.clear();
      this._navStart = _num(p.timestamp);
    }
    const entry = { name: p.name, timestamp: _num(p.timestamp), at: Date.now() };
    if (this._navStart == null) this._navStart = entry.timestamp;
    if (!this._seen.has(entry.name)) this._seen.set(entry.name, entry);
    for (const wake of [...this._waiters]) {
      this._waiters.delete(wake);
      wake();
    }
  }

  // --- waiting ------------------------------------------------------------

  async _wait(options) {
    const timeoutMs = Math.max(
      0,
      _firstNumber(options.timeoutMs, this._defaults.timeoutMs, DEFAULT_TIMEOUT_MS),
    );
    const deadline = Date.now() + timeoutMs;
    try {
      const state = await this._run(options, timeoutMs, deadline);
      this._state = state;
      return state;
    } finally {
      await this.close();
    }
  }

  async _run(options, timeoutMs, deadline) {
    if (this._prefer === VIA_NETWORK_ALMOST_IDLE) {
      // The caller asked for Blink's own signal: 1.8x faster for the same median
      // content on a DIRECT connection, and only sane un-proxied (it fails to
      // fire on 36% of proxied loads).
      return this._fallback(deadline, _ASKED_FOR_LIFECYCLE);
    }
    const params = waitForSettleParams(
      _firstNumber(options.quietWindowMs, this._defaults.quietWindowMs),
      _firstNumber(options.minChars, this._defaults.minChars),
      // The browser's budget is a shade under ours, so its answer wins the race.
      Math.max(1, Math.round(timeoutMs - _REPLY_GRACE_MS)),
    );

    let reply;
    try {
      reply = await _byDeadline(
        this._session.send(WAIT_FOR_SETTLE_METHOD, params), deadline,
      );
    } catch (err) {
      if (!_isMethodNotFound(err)) throw err;
      return this._fallback(deadline, _NO_COMMAND);
    }
    if (reply === TIMED_OUT) {
      // The command outlived the whole budget. Report what the lifecycle did
      // see rather than hanging on it — the deadline has passed, so this
      // resolves immediately.
      return this._fallback(deadline, _NO_ANSWER);
    }
    // A raw DevTools socket resolves the envelope instead of throwing; the
    // driver bindings throw. Both mean the same thing here.
    if (reply && typeof reply === 'object' && reply.error) {
      if (_isMethodNotFound(reply.error)) return this._fallback(deadline, _NO_COMMAND);
      throw new Error(
        WAIT_FOR_SETTLE_METHOD + ' refused: ' + JSON.stringify(reply.error),
      );
    }
    return SettleState.fromReply(reply);
  }

  /** networkAlmostIdle, else the load event, else nothing — main frame only. */
  async _fallback(deadline, reason) {
    for (;;) {
      const idle = this._seen.get(NETWORK_ALMOST_IDLE);
      if (idle) return this._fromEvent(idle, VIA_NETWORK_ALMOST_IDLE, reason);
      const remaining = deadline - Date.now();
      if (remaining <= 0 || this._closed) break;
      await this._nextEvent(remaining);
    }
    const load = this._seen.get(LOAD_EVENT);
    if (load) {
      return this._fromEvent(
        load,
        VIA_LOAD,
        reason + ' No main-frame networkAlmostIdle before the deadline; ' +
        'reporting the load event, which reproduces the final text on 40% of loads.',
      );
    }
    return new SettleState({
      outcome: OUTCOME_TIMEOUT,
      via: VIA_TIMEOUT,
      elapsedMs: Math.max(0, Date.now() - this._armedAt),
      navigations: this._navigations || null,
      reason: reason + ' No main-frame lifecycle signal before the deadline.',
    });
  }

  _fromEvent(entry, via, reason) {
    return new SettleState({
      outcome: OUTCOME_SETTLED,
      via,
      elapsedMs: this._elapsedFor(entry),
      // Only the browser-side command reads the rendered text and the committed
      // document's status; the lifecycle channel carries neither.
      textLength: null,
      httpStatus: null,
      navigations: this._navigations || null,
      reason,
    });
  }

  /**
   * Milliseconds from navigation start. Blink's lifecycle timestamps are the
   * browser's own monotonic clock in seconds, so where we have the navigation's
   * first event we subtract; otherwise we fall back to our own clock since
   * arming, which is one goto away from the same thing.
   */
  _elapsedFor(entry) {
    if (this._navStart != null && entry.timestamp != null && entry.timestamp > this._navStart) {
      return Math.round((entry.timestamp - this._navStart) * 1000);
    }
    return Math.max(0, entry.at - this._armedAt);
  }

  /** Resolve on the next recorded lifecycle event, or after `ms`. */
  _nextEvent(ms) {
    return new Promise((resolve) => {
      const wake = () => {
        clearTimeout(timer);
        resolve();
      };
      const timer = setTimeout(() => {
        this._waiters.delete(wake);
        resolve();
      }, ms);
      this._waiters.add(wake);
    });
  }
}

const _NO_COMMAND =
  WAIT_FOR_SETTLE_METHOD + ' is not registered (-32601): the browser was ' +
  'launched without --page-settle. Fell back to the Blink lifecycle.';
const _NO_ANSWER =
  WAIT_FOR_SETTLE_METHOD + ' did not answer within timeoutMs. Fell back to the ' +
  'Blink lifecycle.';
const _ASKED_FOR_LIFECYCLE =
  'Caller asked for the Blink lifecycle (prefer: networkAlmostIdle).';

/**
 * Attach and ARM a page-completion watch. Call it BEFORE `page.goto`.
 *
 *     const { settleWatch } = require('chromeleon');
 *
 *     const watch = await settleWatch(page);
 *     await page.goto(url, { waitUntil: 'commit' });
 *     const state = await watch.wait({ timeoutMs: 30000 });
 *     if (state.blocked) throw new Error('bot wall: ' + state.httpStatus);
 *     await watch.close();
 *
 * Arming attaches a CDP session, enables `Page`, records the MAIN frame id,
 * subscribes to `Page.lifecycleEvent`, turns lifecycle events on, and then pauses
 * {@link REPLAY_WINDOW_MS} to collect the replay `setLifecycleEventsEnabled`
 * emits for the incumbent about:blank — those loaderIds become the stale set.
 * Keep them and every fallback signal reads as ~0ms.
 *
 * `page` may be a Playwright Page, a Puppeteer Page, or an already-open CDP
 * session; pass `session` to reuse one you already have (the captcha domain's,
 * say). A session we did not open is never detached.
 *
 * `prefer: 'networkAlmostIdle'` skips the command outright — 1.8x faster, and
 * only sane un-proxied, where the lifecycle signal misses 1% of loads instead
 * of 36%.
 *
 * @param {*} page
 * @param {{session?: *, prefer?: string, replayWindowMs?: number,
 *          quietWindowMs?: number, minChars?: number, timeoutMs?: number}=} options
 * @returns {Promise<SettleWatch>}
 */
async function settleWatch(page, options = {}) {
  const opts = options || {};
  const replayWindowMs =
    opts.replayWindowMs == null ? REPLAY_WINDOW_MS : opts.replayWindowMs;
  _checkPrefer(opts.prefer); // before we attach anything we would have to detach
  const { session, owned } = opts.session
    ? { session: opts.session, owned: false }
    : await _cdpSessionFor(page);
  const watch = new SettleWatch(session, {
    owned,
    prefer: opts.prefer,
    quietWindowMs: opts.quietWindowMs,
    minChars: opts.minChars,
    timeoutMs: opts.timeoutMs,
  });
  try {
    await watch._arm(replayWindowMs);
  } catch (err) {
    await watch.close();
    throw err;
  }
  return watch;
}

// `await using watch = await settleWatch(page)` where the runtime has it.
if (typeof Symbol.asyncDispose !== 'undefined') {
  SettleWatch.prototype[Symbol.asyncDispose] = function asyncDispose() {
    return this.close();
  };
}

module.exports = {
  PAGE_SETTLE_SWITCH,
  PAGE_SETTLE_QUIET_WINDOW_SWITCH,
  PAGE_SETTLE_MIN_CHARS_SWITCH,
  PAGE_SETTLE_TIMEOUT_SWITCH,
  PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH,
  PAGE_SETTLE_PIERCE_SHADOW_SWITCH,
  WAIT_FOR_SETTLE_METHOD,
  LIFECYCLE_EVENT,
  NETWORK_ALMOST_IDLE,
  NETWORK_IDLE,
  OUTCOME_SETTLED,
  OUTCOME_TIMEOUT,
  OUTCOME_CHALLENGE,
  SETTLE_OUTCOMES,
  VIA_WAIT_FOR_SETTLE,
  VIA_NETWORK_ALMOST_IDLE,
  VIA_LOAD,
  VIA_TIMEOUT,
  BLOCKED_STATUSES,
  REPLAY_WINDOW_MS,
  DEFAULT_TIMEOUT_MS,
  SettleState,
  SettleWatch,
  settleLaunchArgs,
  settleWatch,
  waitForSettleParams,
};
