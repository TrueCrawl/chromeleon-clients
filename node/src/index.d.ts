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
