'use strict';
/**
 * The per-context proxy handshake, and nothing else.
 *
 * Chromeleon attaches an authenticated proxy to a browser context with two
 * browser-level CDP commands, in order, on one connection:
 *
 *     Target.setProxyCredentials  {proxyServer, username, password}
 *     Target.createBrowserContext {proxyServer}
 *
 * That is a PROTOCOL fact, not a Playwright fact. This module knows only the
 * protocol: it takes a `send(method, params)` callable and holds the lock the
 * single-use registration requires. Every driver — Playwright, Puppeteer,
 * Selenium, a raw WebSocket — is a short adapter on top.
 *
 * Rules encoded here, none of them visible when you get them wrong:
 *   - the proxyServer string must be byte-identical in both commands;
 *   - credentials go only in the registration, never on the context;
 *   - the registration is single-use and carries no correlation token, so the
 *     register-then-create pair must be serialized per (connection, proxyServer);
 *     a second registration is rejected until the first is consumed.
 *
 * Not enforced here (the binary enforces it): passing credentials at LAUNCH
 * leaves the exit IP unresolved, which unbinds persona geo and disables WebRTC
 * masking. Chromeleon fails closed on that with a remediation banner.
 *
 * This module is transport-agnostic and fully async: `withProxyRegistration`
 * awaits your handshake body under an async mutex.
 */

/**
 * Launch flags a per-context proxy needs. A context-level proxy routes HTTP,
 * but WebRTC's UDP socket is browser-level and would otherwise gather ICE over
 * the real route. Chromeleon auto-appends this for LAUNCH-time proxies only,
 * which per-context proxies by definition are not.
 * @type {readonly string[]}
 */
const LAUNCH_ARGS = Object.freeze([
  '--webrtc-ip-handling-policy=disable_non_proxied_udp',
]);

/**
 * Playwright default switches that {@link launch} asks Playwright NOT to add.
 *
 * Playwright launches Chromium with ~46 flags of its own. Measured 2026-08-10
 * over 113 paired trials on 113 distinct fresh exits, a Playwright-launched
 * Chromeleon was blocked by Google materially more often than the same binary
 * launched bare on the same exit in the same minute: 10.6% vs 32.7% pass,
 * discordant 27-2, McNemar p = 1.6e-6, effect +22.1 pp (95% CI [+12.8, +31.5]).
 * Attaching to a bare launch with connectOverCDP instead recovers the whole
 * penalty, which locates the cost in how Playwright LAUNCHES, not in CDP itself.
 *
 * The individual flag responsible has NOT been isolated. An early reading
 * blamed --disable-field-trial-config; that did not replicate (30 pairs,
 * p=0.50) and is retracted. This list is the Google-services-adjacent subset of
 * Playwright's defaults, suppressed together; the supporting A/B for the list
 * itself (3/3) came from a measurement window that overstated the overall effect
 * ~5x, so treat it as a considered default rather than a verified remedy, and
 * re-run a paired A/B before quoting it as a fix.
 *
 * Callers who pass `ignoreDefaultArgs` themselves keep full control.
 */
const SUPPRESSED_DEFAULT_ARGS = Object.freeze([
  '--disable-field-trial-config',
  '--disable-component-update',
  '--disable-client-side-phishing-detection',
  '--metrics-recording-only',
  '--disable-breakpad',
  '--no-service-autorun',
  '--disable-extensions',
  '--disable-default-apps',
  '--disable-component-extensions-with-background-pages',
  '--disable-search-engine-choice-screen',
  '--no-default-browser-check',
  '--unsafely-disable-devtools-self-xss-warnings',
  '--use-mock-keychain',
]);

/** The registration command. Paired with {@link credentialsParams}. */
const CREDENTIALS_METHOD = 'Target.setProxyCredentials';

/** A proxy split into the parts each command is allowed to see. */
class ProxySpec {
  /**
   * @param {string} server  normalized `scheme://host[:port]`
   * @param {?string} username
   * @param {?string} password  `null` means "no password given" (unauthenticated);
   *                            `''` is an explicit empty password (authenticated).
   */
  constructor(server, username = null, password = null) {
    this.server = server;
    this.username = username == null ? null : String(username);
    this.password = password == null ? null : String(password);
    Object.freeze(this);
  }

  /** username present AND a password was given (even if empty). */
  get authenticated() {
    return Boolean(this.username) && this.password !== null;
  }
}

/**
 * Canonicalise a proxy server the way Playwright will. Byte-identity between
 * the two commands is required but only one of them is ours: Playwright rewrites
 * the server it sends to createBrowserContext as `url.protocol + '//' + url.host`
 * (browserContext.js normalizeProxySettings), which lowercases the host, drops a
 * default port, and prepends `http://` when the scheme is missing. We produce
 * exactly that so our setProxyCredentials.proxyServer matches byte-for-byte.
 * @param {string} server
 * @returns {string}
 */
function normalizeServer(server) {
  let s = String(server).trim();
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(s)) s = 'http://' + s;
  const url = new URL(s);
  // WHATWG URL.host already lowercases the hostname and omits the scheme's
  // default port — identical to Playwright's normalization.
  return url.protocol + '//' + url.host;
}

/**
 * Parse a proxy given as a URL string, a `{server, username, password}` object,
 * or a {@link ProxySpec}. Credentials embedded in a URL are percent-DECODED —
 * a password containing `@` or `:` must be encoded in the URL, so it has to be
 * decoded before it goes on the wire, or you authenticate with the wrong secret.
 * @param {string|object|ProxySpec} proxy
 * @returns {ProxySpec}
 */
function parseProxy(proxy) {
  if (proxy instanceof ProxySpec) return proxy;
  if (proxy && typeof proxy === 'object') {
    if (!('server' in proxy)) {
      throw new TypeError("proxy object requires a 'server' key");
    }
    return new ProxySpec(
      normalizeServer(proxy.server),
      proxy.username == null ? null : proxy.username,
      proxy.password == null ? null : proxy.password,
    );
  }
  let url = String(proxy).trim();
  const scheme = url.match(/^[a-z][a-z0-9+.-]*:\/\//i);
  const start = scheme ? scheme[0].length : 0;
  const rest = url.slice(start);
  const pathAt = rest.search(/[/?#]/);
  const authority = pathAt === -1 ? rest : rest.slice(0, pathAt);
  const at = authority.lastIndexOf('@');
  let username = null;
  let password = null;
  if (at !== -1) {
    const cred = authority.slice(0, at);
    const colon = cred.indexOf(':');
    if (colon === -1) {
      username = decodeURIComponent(cred);
    } else {
      username = decodeURIComponent(cred.slice(0, colon));
      password = decodeURIComponent(cred.slice(colon + 1));
    }
    url = url.slice(0, start) + url.slice(start + at + 1);
  }
  return new ProxySpec(normalizeServer(url), username, password);
}

/** Params for `Target.setProxyCredentials`. */
function credentialsParams(spec) {
  return {
    proxyServer: spec.server,
    username: spec.username || '',
    password: spec.password || '',
  };
}

/**
 * Throw if the browser refused the registration. Playwright resolves send() to
 * the result object (or throws itself); a raw WebSocket returns the CDP envelope
 * whose `error` we surface here.
 */
function checkRegistration(result) {
  if (result && typeof result === 'object' && result.error) {
    throw new Error(
      'proxy credential registration refused: ' + JSON.stringify(result),
    );
  }
}

// --- async mutex, keyed by (connection, proxyServer) ------------------------
// Single-threaded JS still interleaves at every await, so two concurrent
// newProxyContext() calls for the same (connection, server) could interleave
// their register/create pairs and have the browser reject the second
// registration. Serialize them. WeakMap keys by the connection object without
// leaking it; the chain map is cleaned up when a key drains.
const _connIds = new WeakMap();
let _connSeq = 0;
const _chain = new Map();

function _connId(connection) {
  if (connection === null || (typeof connection !== 'object' && typeof connection !== 'function')) {
    return 'primitive:' + String(connection);
  }
  let id = _connIds.get(connection);
  if (id === undefined) {
    id = ++_connSeq;
    _connIds.set(connection, id);
  }
  return id;
}

/**
 * Hold the registration slot for `proxy` on `connection` for the duration of
 * `body`. Send the credentials AND create the context inside `body` — the lock
 * must span both, because the registration is consumed by the matching
 * createBrowserContext. Returns whatever `body` resolves to.
 *
 *     await withProxyRegistration(browser, proxy, async (spec) => {
 *       checkRegistration(await send(CREDENTIALS_METHOD, credentialsParams(spec)));
 *       return await createContext(spec.server);
 *     });
 *
 * @template T
 * @param {*} connection  identifies the CDP connection for locking only
 * @param {string|object|ProxySpec} proxy
 * @param {(spec: ProxySpec) => Promise<T>} body
 * @returns {Promise<T>}
 */
async function withProxyRegistration(connection, proxy, body) {
  const spec = parseProxy(proxy);
  if (!spec.authenticated) {
    throw new Error(
      'per-context preregistration needs a username and password; an ' +
      'unauthenticated proxy can be passed straight to the driver',
    );
  }
  if (!/^https?:\/\//i.test(spec.server)) {
    throw new Error(
      'credential preregistration requires an HTTP(S) proxy, got ' + spec.server,
    );
  }

  const key = _connId(connection) + '\0' + spec.server;
  const prev = _chain.get(key) || Promise.resolve();
  let release;
  const held = new Promise((resolve) => { release = resolve; });
  const tail = prev.then(() => held);
  _chain.set(key, tail);

  await prev; // wait for the previous holder of this (connection, server)
  try {
    return await body(spec);
  } finally {
    release();
    if (_chain.get(key) === tail) _chain.delete(key); // drained: no one queued
  }
}

// --- Captcha solver -------------------------------------------------------
//
// Chromeleon ships a built-in reCAPTCHA/hCaptcha solver. Enabled at launch it
// works AUTOMATICALLY — detects the widget, solves it (audio first, image
// fallback), writes the token; you do not call it. The release binary embeds
// the models, so `--captcha-solver` alone is enough.
//
// The `Chromeleon` CDP domain only OBSERVES/STEERS the solver: enable it on a
// page CDP session for lifecycle events, and use `solverEval` to run JS in the
// solver's isolated world (world 10), which pierces CLOSED shadow roots.

/** Launch switch that turns the solver on. */
const CAPTCHA_SOLVER_SWITCH = '--captcha-solver';
/** Model-dir override. Dev/self-host only — the release binary embeds models. */
const CAPTCHA_MODEL_PATH_SWITCH = '--captcha-model-path';

/** `Chromeleon` domain commands, sent on a page CDP session. */
const ENABLE_METHOD = 'Chromeleon.enable';
const DISABLE_METHOD = 'Chromeleon.disable';
const SOLVER_EVAL_METHOD = 'Chromeleon.solverEval';

/** `Chromeleon` domain events. Subscribe with the driver's `cdp.on(name, cb)`. */
const CAPTCHA_DETECTED = 'Chromeleon.captchaDetected'; // {sitekey}
const CAPTCHA_SOLVING = 'Chromeleon.captchaSolving'; // {sitekey, method: audio|image}
const CAPTCHA_SOLVED = 'Chromeleon.captchaSolved'; // {sitekey, attempts, timeMs}
const CAPTCHA_FAILED = 'Chromeleon.captchaFailed'; // {sitekey, attempts, reason}
const SOLVER_EVAL_RESULT = 'Chromeleon.solverEvalResult'; // {result}

/** The four lifecycle events: detected -> solving -> solved | failed. */
const CAPTCHA_EVENTS = Object.freeze([
  CAPTCHA_DETECTED, CAPTCHA_SOLVING, CAPTCHA_SOLVED, CAPTCHA_FAILED,
]);

/**
 * Launch flags that turn on the built-in captcha solver. The release binary
 * embeds the models, so this is just `--captcha-solver`; `modelPath` is a
 * dev/self-host override that also appends `--captcha-model-path=<dir>`.
 * @param {?string=} modelPath
 * @returns {string[]}
 */
function captchaLaunchArgs(modelPath = null) {
  const args = [CAPTCHA_SOLVER_SWITCH];
  if (modelPath != null) args.push(`${CAPTCHA_MODEL_PATH_SWITCH}=${modelPath}`);
  return args;
}

/**
 * Params for `Chromeleon.solverEval`. `expression` runs in the solver's isolated
 * world (pierces CLOSED shadow roots); `frameUrlContains` selects a subframe by
 * URL substring (empty = primary main frame). The string result arrives as a
 * `solverEvalResult` event, not as the command return.
 * @param {string} expression
 * @param {string=} frameUrlContains
 * @returns {{expression: string, frameUrlContains: string}}
 */
function solverEvalParams(expression, frameUrlContains = '') {
  return { expression, frameUrlContains };
}

module.exports = {
  LAUNCH_ARGS,
  SUPPRESSED_DEFAULT_ARGS,
  CREDENTIALS_METHOD,
  ProxySpec,
  normalizeServer,
  parseProxy,
  credentialsParams,
  checkRegistration,
  withProxyRegistration,
  // Captcha solver (Chromeleon CDP domain).
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
};
