// Type definitions for chromeleon. Driver objects (Playwright/Puppeteer)
// are intentionally `any` so this package imposes no dependency on their types.

export type ProxyInput = string | { server: string; username?: string | null; password?: string | null } | ProxySpec;

export class ProxySpec {
  readonly server: string;
  readonly username: string | null;
  readonly password: string | null;
  constructor(server: string, username?: string | null, password?: string | null);
  /** username present AND a password given (even if empty). */
  readonly authenticated: boolean;
}

/** Launch flags a per-context proxy needs (browser-level WebRTC UDP policy). */
export const LAUNCH_ARGS: readonly string[];
/** The registration command: "Target.setProxyCredentials". */
export const CREDENTIALS_METHOD: string;

export function normalizeServer(server: string): string;
export function parseProxy(proxy: ProxyInput): ProxySpec;
export function credentialsParams(spec: ProxySpec): { proxyServer: string; username: string; password: string };
export function checkRegistration(result: unknown): void;

/** Run `body(spec)` under the per-(connection,server) registration lock. */
export function withProxyRegistration<T>(
  connection: unknown,
  proxy: ProxyInput,
  body: (spec: ProxySpec) => Promise<T>,
): Promise<T>;

/** Browser launch environment with the controller's PROXY_* variables removed. */
export function browserProcessEnv(env?: Record<string, string>): Record<string, string>;

/** Launch Chromeleon via Playwright's `chromium`; returns a Playwright Browser. */
export function launch(
  chromium: any,
  executablePath: string,
  options?: {
    args?: string[];
    env?: Record<string, string>;
    /** Turn on the built-in reCAPTCHA/hCaptcha solver (`--captcha-solver`). */
    captcha?: boolean;
    /** Model-dir override (dev/self-host); implies `captcha`. */
    captchaModelPath?: string;
    /** Register `Chromeleon.waitForSettle` (`--page-settle`); object form tunes it. */
    pageSettle?: boolean | SettleTuning;
    [k: string]: any;
  },
): Promise<any>;

/** Playwright: a BrowserContext behind an authenticated per-context proxy. */
export function newProxyContext(browser: any, proxy: ProxyInput, contextOptions?: Record<string, any>): Promise<any>;
/** Puppeteer: the same handshake; returns a Puppeteer BrowserContext. */
export function newProxyContextPuppeteer(browser: any, proxy: ProxyInput): Promise<any>;
/** Raw driver: supply `send` and `createContext`; the handshake runs under the lock. */
export function newProxyContextWith(
  connection: unknown,
  proxy: ProxyInput,
  driver: { send: (method: string, params: object) => Promise<any>; createContext: (server: string) => Promise<any> },
): Promise<any>;

// --- Captcha solver (Chromeleon CDP domain) -------------------------------

/** Launch switch that turns the solver on: "--captcha-solver". */
export const CAPTCHA_SOLVER_SWITCH: string;
/** Model-dir override switch (dev/self-host): "--captcha-model-path". */
export const CAPTCHA_MODEL_PATH_SWITCH: string;
/** `Chromeleon` domain commands. */
export const ENABLE_METHOD: string;
export const DISABLE_METHOD: string;
export const SOLVER_EVAL_METHOD: string;
/** `Chromeleon` domain events (subscribe with `cdp.on(name, cb)`). */
export const CAPTCHA_DETECTED: string;
export const CAPTCHA_SOLVING: string;
export const CAPTCHA_SOLVED: string;
export const CAPTCHA_FAILED: string;
export const SOLVER_EVAL_RESULT: string;
/** The four lifecycle events: detected -> solving -> solved | failed. */
export const CAPTCHA_EVENTS: readonly string[];

/** Flags that turn on the built-in captcha solver (`--captcha-solver` [+ model path]). */
export function captchaLaunchArgs(modelPath?: string | null): string[];
/** Params for `Chromeleon.solverEval`. */
export function solverEvalParams(
  expression: string,
  frameUrlContains?: string,
): { expression: string; frameUrlContains: string };

/** Enable Chromeleon captcha lifecycle events on a page CDP session. */
export function enableCaptcha(cdp: any): Promise<any>;
/** Disable Chromeleon captcha event notifications on a page CDP session. */
export function disableCaptcha(cdp: any): Promise<any>;
/** Evaluate JS in the solver's isolated world; result arrives as SOLVER_EVAL_RESULT. */
export function solverEval(cdp: any, expression: string, frameUrlContains?: string): Promise<any>;

// --- Page completion (Chromeleon.waitForSettle + networkAlmostIdle) -------

/** The browser's verdict. `"challenge"` is a bot wall, never a successful load. */
export type SettleOutcome = 'settled' | 'timeout' | 'challenge';
/** Which signal produced the answer, primary first. */
export type SettleVia = 'waitForSettle' | 'networkAlmostIdle' | 'load' | 'timeout';

/** Sampler tuning, shared by the launch switches and the command. */
export interface SettleTuning {
  /** Quiet window the rendered text must hold still for (browser default 4000). */
  quietWindowMs?: number;
  /** Minimum rendered characters before a settle counts as content. */
  minChars?: number;
  /** Browser-side ceiling for one wait. */
  timeoutMs?: number;
  /** How often the browser samples the rendered text (launch only). */
  sampleIntervalMs?: number;
  /** Include text inside shadow roots (launch only). */
  pierceShadow?: boolean;
}

/** What one `wait()` concluded. Immutable. */
export class SettleState {
  constructor(fields: {
    outcome: SettleOutcome | string;
    via: SettleVia | string;
    elapsedMs?: number | null;
    textLength?: number | null;
    httpStatus?: number | null;
    navigations?: number | null;
    reason?: string | null;
    raw?: Record<string, any> | null;
  });
  readonly outcome: SettleOutcome;
  readonly via: SettleVia;
  /** Milliseconds to settle; the browser's own clock on the primary path. */
  readonly elapsedMs: number | null;
  /** Rendered characters at settle; `null` on the fallback path. */
  readonly textLength: number | null;
  /** The committed document's status; `null` on the fallback path. */
  readonly httpStatus: number | null;
  readonly navigations: number | null;
  readonly reason: string | null;
  /** The browser's reply as sent, so a newer binary's fields survive. */
  readonly raw: Record<string, any> | null;
  /** `outcome === 'challenge'` OR `httpStatus` in {401,403,407,429,503}. */
  readonly blocked: boolean;
  /** `outcome === 'settled'` AND not blocked: a page, not a wall. */
  readonly settled: boolean;
  toJSON(): Record<string, any>;
}

/** An armed watch. Create with {@link settleWatch} BEFORE navigating. */
export class SettleWatch {
  /** Built by {@link settleWatch}, which also arms it; direct use is rare. */
  constructor(
    session: any,
    options?: { owned?: boolean; prefer?: SettleVia } & SettleTuning,
  );
  /** The frame every fallback signal must come from. */
  readonly mainFrameId: string | null;
  /** The CDP session in use (the caller's, or the one the watch attached). */
  readonly session: any;
  /** The concluded state, or `null` before `wait()` returns. */
  readonly state: SettleState | null;
  /**
   * Never rejects because a page did not settle; never runs past `timeoutMs`;
   * memoized, so a second call returns the first call's state.
   */
  wait(options?: { timeoutMs?: number; quietWindowMs?: number; minChars?: number }): Promise<SettleState>;
  /** Unsubscribe and detach. Idempotent. */
  close(): Promise<void>;
}

/**
 * Attach and arm a page-completion watch. Call BEFORE `page.goto`.
 *
 * `session` reuses a CDP session you already have (and is never detached);
 * `prefer: 'networkAlmostIdle'` skips the command outright — faster, and only
 * sane un-proxied.
 */
export function settleWatch(
  page: any,
  options?: SettleTuning & {
    session?: any;
    prefer?: SettleVia;
    replayWindowMs?: number;
  },
): Promise<SettleWatch>;

/** Launch flags that register `Chromeleon.waitForSettle`. */
export function settleLaunchArgs(options?: SettleTuning): string[];
/** Params for `Chromeleon.waitForSettle`; an omitted field is not sent at all. */
export function waitForSettleParams(
  quietWindowMs?: number | null,
  minChars?: number | null,
  timeoutMs?: number | null,
): { quietWindowMs?: number; minChars?: number; timeoutMs?: number };

/** Launch switch that registers the command: "--page-settle". */
export const PAGE_SETTLE_SWITCH: string;
/** Tuning switches, all optional. */
export const PAGE_SETTLE_QUIET_WINDOW_SWITCH: string;
export const PAGE_SETTLE_MIN_CHARS_SWITCH: string;
export const PAGE_SETTLE_TIMEOUT_SWITCH: string;
export const PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH: string;
export const PAGE_SETTLE_PIERCE_SHADOW_SWITCH: string;
/** "Chromeleon.waitForSettle". */
export const WAIT_FOR_SETTLE_METHOD: string;
/** "Page.lifecycleEvent" — the fallback's only channel. */
export const LIFECYCLE_EVENT: string;
/** Blink lifecycle names used by the fallback. */
export const NETWORK_ALMOST_IDLE: string;
export const NETWORK_IDLE: string;
/** `outcome` values the command can return. */
export const OUTCOME_SETTLED: string;
export const OUTCOME_TIMEOUT: string;
export const OUTCOME_CHALLENGE: string;
export const SETTLE_OUTCOMES: readonly string[];
/** `via` values, in preference order. */
export const VIA_WAIT_FOR_SETTLE: string;
export const VIA_NETWORK_ALMOST_IDLE: string;
export const VIA_LOAD: string;
export const VIA_TIMEOUT: string;
/** Statuses a bot wall answers with: 401, 403, 407, 429, 503. */
export const BLOCKED_STATUSES: readonly number[];
/** How long arming absorbs the incumbent about:blank's lifecycle replay: 350. */
export const REPLAY_WINDOW_MS: number;
/** Budget for one `wait()` when the caller names none: 30000. */
export const DEFAULT_TIMEOUT_MS: number;
