//! A thin client for driving the [Chromeleon](https://chromeleon.dev) browser.
//!
//! Chromeleon attaches an **authenticated proxy per browser context** with two
//! browser-level CDP commands that must be issued in order, on one connection,
//! with a byte-identical server string, and serialized against each other:
//!
//! ```text
//! Target.setProxyCredentials  {proxyServer, username, password}
//! Target.createBrowserContext {proxyServer}
//! ```
//!
//! No driver has an API shaped like that, and the ways of getting it wrong are
//! quiet: credentials passed at launch leave the exit IP unresolved, which
//! unbinds the persona's geo and disables WebRTC masking; credentials left
//! percent-encoded authenticate with the wrong secret; a server string the
//! driver re-normalizes no longer matches the one you registered, so the
//! registration is never consumed and the context browses direct. This crate
//! exists so none of that is the caller's problem.
//!
//! It is **driver-agnostic**: no browser library is a dependency, and the
//! handshake is expressed as "send this, then create the context with this
//! string, while holding this guard". That works with
//! [chromiumoxide](https://docs.rs/chromiumoxide), with `headless_chrome`, and
//! with a raw DevTools WebSocket.
//!
//! # Launch, then attach
//!
//! ```no_run
//! use chromeleon::launch::Launcher;
//!
//! let mut browser = Launcher::new("/opt/chromeleon/chrome")
//!     .remote_debugging_port(9222)
//!     .spawn()?;   // WebRTC policy merged in, controller PROXY_* stripped
//! # let _ = browser.kill();
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # The handshake
//!
//! With any async CDP connection, in one call:
//!
//! ```no_run
//! use chromeleon::adapters::new_proxy_context_raw;
//! # use chromeleon::Error;
//! # struct Cdp;
//! # impl Cdp { async fn call(&self, _m: &str, _p: serde_json::Value) -> Result<serde_json::Value, Error> { Ok(serde_json::json!({})) } }
//! # async fn demo(cdp: &Cdp, ws_url: &str) -> Result<(), Error> {
//! let created = new_proxy_context_raw(
//!     ws_url,                                     // identifies the connection
//!     "http://user:pass@gateway:12321",           // credentials are decoded for you
//!     |method, params| cdp.call(method, params),
//! ).await?;
//! let context_id = created["browserContextId"].as_str().unwrap_or_default();
//! # let _ = context_id;
//! # Ok(())
//! # }
//! ```
//!
//! …or step by step, when your driver creates contexts its own way:
//!
//! ```no_run
//! use chromeleon::{check_registration, registration::proxy_registration, CREDENTIALS_METHOD};
//! # use chromeleon::Error;
//! # struct Browser;
//! # impl Browser {
//! #   async fn send(&self, _m: &str, _p: serde_json::Value) -> Result<serde_json::Value, Error> { Ok(serde_json::json!({})) }
//! #   async fn new_context(&self, _server: &str) -> Result<String, Error> { Ok(String::new()) }
//! # }
//! # async fn demo(browser: &Browser, ws_url: &str) -> Result<(), Error> {
//! let reg = proxy_registration(ws_url, "http://user:pass@gateway:12321").await?;
//! check_registration(&browser.send(CREDENTIALS_METHOD, reg.credentials_params().to_value()).await?)?;
//! let context = browser.new_context(reg.server()).await?;   // same bytes, still under the guard
//! drop(reg);
//! # let _ = context;
//! # Ok(())
//! # }
//! ```
//!
//! # Captcha solving
//!
//! Launch with [`Launcher::captcha(true)`](launch::Launcher::captcha) and the
//! built-in solver handles reCAPTCHA/hCaptcha by itself. The [`captcha`] module
//! is only for watching it: enable the `Chromeleon` domain on a page session and
//! parse the lifecycle events with [`CaptchaEvent::parse`].
//!
//! # Page completion
//!
//! "The page has finished" is [`settle`]: `Chromeleon.waitForSettle` settles on
//! the main frame's rendered TEXT going quiet, with no JavaScript in the page,
//! and Blink's `networkAlmostIdle` is the fallback for a browser launched
//! without `--page-settle`. Arm the watch BEFORE the navigation — a session
//! attached to an already-committed document cannot see the challenge header,
//! and degrades to status-code-only detection without saying so.
//!
//! ```no_run
//! # use std::time::Duration;
//! # use serde_json::Value;
//! # use chromeleon::settle::{PageSession, SettleWatch, WaitOptions};
//! # struct Session;
//! # impl PageSession for Session {
//! #     type Error = chromeleon::Error;
//! #     async fn send(&self, _m: &'static str, _p: Value) -> Result<Value, Self::Error> { unimplemented!() }
//! #     async fn next_event(&self, _t: Duration) -> Option<(String, Value)> { None }
//! # }
//! # async fn demo(session: Session) -> Result<(), chromeleon::Error> {
//! let mut watch = SettleWatch::arm(session).await?;    // before the goto
//! // …navigate…
//! let state = watch.wait(WaitOptions::new().timeout_ms(30_000)).await?;
//! if state.blocked() { /* a bot wall, not a page */ }
//! watch.close().await;
//! # Ok(()) }
//! ```
//!
//! # What this crate is held to
//!
//! `HANDSHAKE.md` in the [clients
//! repository](https://github.com/TrueCrawl/chromeleon-clients) is the spec all
//! three clients (Python, Node, Rust) implement, and a shared conformance
//! corpus pins them to the same parsing and normalization behaviour.

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]
// Feature badges on docs.rs, which builds with nightly and passes --cfg docsrs.
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod adapters;
pub mod captcha;
pub mod core;
pub mod launch;
pub mod perf;
pub mod registration;
pub mod settle;

#[cfg(feature = "chromiumoxide")]
#[cfg_attr(docsrs, doc(cfg(feature = "chromiumoxide")))]
pub mod oxide;

#[doc(inline)]
pub use crate::core::{
    check_registration, normalize_server, parse_proxy, CreateContextParams, CredentialsParams,
    Error, IntoProxySpec, ProxySpec, Result, CREATE_CONTEXT_METHOD, CREDENTIALS_METHOD,
};

#[doc(inline)]
pub use crate::launch::{
    browser_process_env, launch_args, Launcher, LAUNCH_ARGS, SUPPRESSED_DEFAULT_ARGS,
};

#[doc(inline)]
pub use crate::registration::{
    proxy_registration, proxy_registration_blocking, with_proxy_registration,
    with_proxy_registration_blocking, ConnKey, Registration, Registry,
};

#[doc(inline)]
pub use crate::adapters::{
    new_proxy_context_raw, new_proxy_context_raw_blocking, new_proxy_context_with,
    new_proxy_context_with_blocking,
};

#[doc(inline)]
pub use crate::perf::{sticky_geo_env, BrowserPool, GeoResolution, ProxyRotation};

#[doc(inline)]
pub use crate::captcha::{
    captcha_launch_args, solver_eval_params, CaptchaEvent, SolveMethod, CAPTCHA_DETECTED,
    CAPTCHA_EVENTS, CAPTCHA_FAILED, CAPTCHA_SOLVED, CAPTCHA_SOLVER_SWITCH, CAPTCHA_SOLVING,
    DISABLE_METHOD, ENABLE_METHOD, SOLVER_EVAL_METHOD, SOLVER_EVAL_RESULT,
};

#[doc(inline)]
pub use crate::settle::{
    settle_launch_args, BlockingPageSession, BlockingSettleWatch, LifecycleTracker, PageSession,
    SettleOutcome, SettleState, SettleTuning, SettleVia, SettleWatch, WaitOptions,
    NETWORK_ALMOST_IDLE, PAGE_SETTLE_SWITCH, WAIT_FOR_SETTLE_METHOD,
};

/// The `HANDSHAKE.md` spec version this client implements.
///
/// The handshake is versioned separately from the crate: when it changes, every
/// client's **major** moves together. A client whose `SPEC_VERSION` is behind
/// the browser's is a client that will fail quietly, not loudly.
pub const SPEC_VERSION: u32 = 1;
