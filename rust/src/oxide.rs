//! [chromiumoxide](https://docs.rs/chromiumoxide) adapter.
//!
//! Enable the `chromiumoxide` feature. It is off by default and it raises the
//! MSRV to whatever chromiumoxide requires (1.85 at 0.9) — the rest of this
//! crate builds on 1.75 and depends on no browser library at all.
//!
//! ```no_run
//! # #[cfg(feature = "chromiumoxide")]
//! # async fn demo() -> Result<(), chromeleon::oxide::OxideError> {
//! use chromeleon::oxide::new_proxy_context;
//!
//! # let browser: chromiumoxide::Browser = unimplemented!();
//! let context = new_proxy_context(&browser, "http://user:pass@gateway:12321").await?;
//! # let _ = context;
//! # Ok(()) }
//! ```
//!
//! Two chromiumoxide facts this module exists to absorb:
//!
//! * **`Target.setProxyCredentials` is not in its generated protocol**, and
//!   there is no untyped `send`. [`RawCommand`] is the escape hatch — its
//!   `Method::identifier` is computed from the value, so the method name can be
//!   chosen at runtime, and its `Serialize` forwards to the params object.
//! * **`Browser::execute` is browser-level.** A page session cannot carry the
//!   registration; the `Chromeleon` captcha domain is the opposite way round and
//!   must be enabled on a page.

use std::borrow::Cow;
use std::fmt;

use chromiumoxide::browser::BrowserConfigBuilder;
use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::CustomEvent;
use chromiumoxide::error::CdpError;
use chromiumoxide::types::{MethodId, MethodType};
use chromiumoxide::{Browser, Command, Method, Page};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::captcha::{
    CaptchaEvent, SolveMethod, CAPTCHA_DETECTED, CAPTCHA_FAILED, CAPTCHA_SOLVED, CAPTCHA_SOLVING,
    DISABLE_METHOD, ENABLE_METHOD, SOLVER_EVAL_METHOD, SOLVER_EVAL_RESULT,
};
use crate::core::{Error, IntoProxySpec, CREATE_CONTEXT_METHOD, CREDENTIALS_METHOD};
use crate::launch::{is_suppressed_default_arg, LAUNCH_ARGS};
use crate::registration::{proxy_registration, ConnKey};

/// This crate's errors and chromiumoxide's, in one type.
#[derive(Debug)]
pub enum OxideError {
    /// The proxy was refused before anything was sent.
    Chromeleon(Error),
    /// The browser or the connection failed.
    Cdp(CdpError),
}

impl fmt::Display for OxideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OxideError::Chromeleon(e) => write!(f, "{e}"),
            OxideError::Cdp(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OxideError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OxideError::Chromeleon(e) => Some(e),
            OxideError::Cdp(e) => Some(e),
        }
    }
}

impl From<Error> for OxideError {
    fn from(e: Error) -> Self {
        OxideError::Chromeleon(e)
    }
}

impl From<CdpError> for OxideError {
    fn from(e: CdpError) -> Self {
        OxideError::Cdp(e)
    }
}

/// A CDP command chromiumoxide's generated protocol does not know.
///
/// `Method::identifier` takes `&self`, so the method name is a value rather
/// than a type-level constant; `Serialize` forwards to `params`, which is what
/// chromiumoxide puts on the wire. That combination is the untyped send that
/// the crate otherwise does not offer.
#[derive(Debug, Clone)]
pub struct RawCommand {
    /// The CDP method, e.g. `"Chromeleon.enable"`.
    pub method: Cow<'static, str>,
    /// The params object, serialized as-is.
    pub params: Value,
}

impl RawCommand {
    /// A command with the given method and params.
    pub fn new(method: impl Into<Cow<'static, str>>, params: Value) -> Self {
        RawCommand {
            method: method.into(),
            params,
        }
    }
}

impl Serialize for RawCommand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.params.serialize(serializer)
    }
}

impl Method for RawCommand {
    fn identifier(&self) -> MethodId {
        self.method.clone()
    }
}

impl Command for RawCommand {
    type Response = Value;
}

/// Send an arbitrary **browser-level** CDP command.
pub async fn send_raw(
    browser: &Browser,
    method: impl Into<Cow<'static, str>>,
    params: Value,
) -> Result<Value, OxideError> {
    Ok(browser
        .execute(RawCommand::new(method, params))
        .await?
        .result)
}

/// Send an arbitrary command on a **page** session.
pub async fn send_raw_page(
    page: &Page,
    method: impl Into<Cow<'static, str>>,
    params: Value,
) -> Result<Value, OxideError> {
    Ok(page.execute(RawCommand::new(method, params)).await?.result)
}

/// A browser context behind an authenticated per-context proxy.
///
/// Runs the whole handshake: takes the registration slot for this
/// `(connection, server)`, sends `Target.setProxyCredentials` with the decoded
/// credentials, and creates the context with the same normalized server string
/// — which is what consumes the registration. The slot is released only after
/// the context exists.
///
/// Open pages in it by passing the id to `CreateTargetParams`:
///
/// ```no_run
/// # #[cfg(feature = "chromiumoxide")]
/// # async fn demo(browser: &chromiumoxide::Browser) -> Result<(), Box<dyn std::error::Error>> {
/// use chromiumoxide::cdp::browser_protocol::target::CreateTargetParams;
///
/// let context = chromeleon::oxide::new_proxy_context(browser, "http://u:p@gw:12321").await?;
/// let mut target = CreateTargetParams::new("https://api.ipify.org");
/// target.browser_context_id = Some(context);
/// let page = browser.new_page(target).await?;
/// # let _ = page;
/// # Ok(()) }
/// ```
pub async fn new_proxy_context(
    browser: &Browser,
    proxy: impl IntoProxySpec,
) -> Result<BrowserContextId, OxideError> {
    new_proxy_context_inner(browser, proxy, None).await
}

/// [`new_proxy_context`], tagging the registration with a `credentialsId`.
///
/// ⚠️ Requires a browser that knows the field — v151.5 and later. On an older
/// binary, including the v151.4 published as `latest`, use
/// [`new_proxy_context`].
pub async fn new_proxy_context_with_id(
    browser: &Browser,
    proxy: impl IntoProxySpec,
    credentials_id: &str,
) -> Result<BrowserContextId, OxideError> {
    new_proxy_context_inner(browser, proxy, Some(credentials_id)).await
}

async fn new_proxy_context_inner(
    browser: &Browser,
    proxy: impl IntoProxySpec,
    credentials_id: Option<&str>,
) -> Result<BrowserContextId, OxideError> {
    // The websocket address is the root connection, which is exactly what the
    // browser keys pending registrations on.
    let conn = ConnKey::new(browser.websocket_address().clone());
    let registration = proxy_registration(conn, proxy).await?;

    let mut credentials = registration.credentials_params();
    if let Some(id) = credentials_id {
        credentials = credentials.with_credentials_id(id);
    }
    send_raw(browser, CREDENTIALS_METHOD, credentials.to_value()).await?;

    // Still holding the registration: this call consumes it.
    //
    // Sent as a raw command rather than through `Browser::create_browser_context`
    // for two reasons: `proxyCredentialsId` is a Chromeleon extension that
    // chromiumoxide's generated params do not carry, and going through our own
    // params keeps the server string the exact bytes we registered instead of
    // whatever a round trip through someone else's struct produces.
    let mut context_params = registration.create_context_params();
    if let Some(id) = credentials_id {
        context_params = context_params.with_credentials_id(id);
    }
    let created = send_raw(browser, CREATE_CONTEXT_METHOD, context_params.to_value()).await?;
    drop(registration);

    match created["browserContextId"].as_str() {
        Some(id) => Ok(BrowserContextId::from(id.to_string())),
        None => Err(OxideError::Chromeleon(Error::RegistrationRefused {
            detail: format!("createBrowserContext returned no browserContextId: {created}"),
        })),
    }
}

/// chromiumoxide's own default switches, minus the ones a Chromeleon session
/// should not carry.
///
/// chromiumoxide adds ~24 Puppeteer-derived flags, and its opt-out
/// (`disable_default_args`) is all-or-nothing — so the way to drop a few is to
/// drop them all and hand back the keepers. Removed here:
///
/// * everything in [`SUPPRESSED_DEFAULT_ARGS`](crate::launch::SUPPRESSED_DEFAULT_ARGS)
///   that chromiumoxide actually adds — 6 of our 13 appear in its list;
/// * `--enable-automation`, which is a bot signal on a browser whose whole
///   purpose is not to look like one;
/// * `--lang=en_US`, which overrides the persona's own language;
/// * `--enable-blink-features=IdleDetection`, a surface nothing here needs.
///
/// ⚠️ **One suppression cannot be honoured**: chromiumoxide appends
/// `--disable-extensions` whenever no extension is configured, *outside* the
/// `disable_default_args` branch, so a `Browser::launch` always carries it. It
/// measured inert in the flag bisection, unlike the two that did not — but if
/// you need it gone, launch with [`Launcher`](crate::launch::Launcher) and
/// attach instead.
///
/// This list is a snapshot of chromiumoxide's, so a flag it adds in a later
/// release will not appear until this is updated — which is the safe direction:
/// a new default arrives only when someone has looked at it.
pub fn kept_default_args() -> Vec<String> {
    CHROMIUMOXIDE_DEFAULT_ARGS
        .iter()
        .filter(|arg| !is_suppressed_default_arg(arg))
        .filter(|arg| {
            !matches!(
                **arg,
                "--enable-automation" | "--lang=en_US" | "--enable-blink-features=IdleDetection"
            )
        })
        .map(|arg| arg.to_string())
        .collect()
}

/// chromiumoxide's `DEFAULT_ARGS`, as of 0.9.
const CHROMIUMOXIDE_DEFAULT_ARGS: &[&str] = &[
    "--disable-background-networking",
    "--enable-features=NetworkService,NetworkServiceInProcess",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-breakpad",
    "--disable-client-side-phishing-detection",
    "--disable-component-extensions-with-background-pages",
    "--disable-default-apps",
    "--disable-dev-shm-usage",
    "--disable-features=TranslateUI",
    "--disable-hang-monitor",
    "--disable-ipc-flooding-protection",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--disable-renderer-backgrounding",
    "--disable-sync",
    "--force-color-profile=srgb",
    "--metrics-recording-only",
    "--no-first-run",
    "--enable-automation",
    "--password-store=basic",
    "--use-mock-keychain",
    "--enable-blink-features=IdleDetection",
    "--lang=en_US",
];

/// A `BrowserConfigBuilder` with the flags a Chromeleon session wants.
///
/// ⚠️ **This cannot give you a clean environment.** chromiumoxide's
/// `BrowserConfig` only *adds* environment variables — it has no `env_clear` or
/// `env_remove` — so the controller's `PROXY_*` variables are inherited by the
/// browser however this config is built, and `HANDSHAKE.md` says they must not
/// be. When that matters (it does whenever the controller itself runs behind a
/// proxy), spawn with [`Launcher`](crate::launch::Launcher) and attach:
///
/// ```no_run
/// # #[cfg(feature = "chromiumoxide")]
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use chromeleon::launch::Launcher;
/// use chromiumoxide::Browser;
/// use std::time::Duration;
///
/// let (child, ws_url) = Launcher::new("/opt/chromeleon/chrome")
///     .remote_debugging_port(9222)
///     .spawn_and_wait(Duration::from_secs(20))?;      // PROXY_* stripped here
/// let (browser, handler) = Browser::connect(ws_url).await?;
/// # let _ = (child, browser, handler);
/// # Ok(()) }
/// ```
///
/// Also set a `user_data_dir`: chromiumoxide's default is a single shared path,
/// so two concurrent launches fight over one profile.
pub fn browser_config(executable: impl AsRef<std::path::Path>) -> BrowserConfigBuilder {
    chromiumoxide::BrowserConfig::builder()
        .chrome_executable(executable.as_ref())
        .disable_default_args()
        .args(kept_default_args())
        .args(LAUNCH_ARGS.iter().map(|a| a.to_string()))
}

// --- captcha ---------------------------------------------------------------

/// Start `Chromeleon` captcha events on a page session.
///
/// The domain exists only when the browser was launched with
/// `--captcha-solver`; without it this fails as an *unknown method*.
pub async fn enable_captcha(page: &Page) -> Result<Value, OxideError> {
    send_raw_page(page, ENABLE_METHOD, serde_json::json!({})).await
}

/// Stop `Chromeleon` captcha events on a page session.
pub async fn disable_captcha(page: &Page) -> Result<Value, OxideError> {
    send_raw_page(page, DISABLE_METHOD, serde_json::json!({})).await
}

/// Evaluate JS in the solver's isolated world (pierces closed shadow roots).
///
/// The result arrives as a [`SolverEvalResultEvent`], not as this call's return
/// value, and events are broadcast browser-wide — so there is nothing tying a
/// result back to this call except what you put in the expression's output.
pub async fn solver_eval(
    page: &Page,
    expression: &str,
    frame_url_contains: &str,
) -> Result<Value, OxideError> {
    send_raw_page(
        page,
        SOLVER_EVAL_METHOD,
        crate::captcha::solver_eval_params(expression, frame_url_contains).to_value(),
    )
    .await
}

macro_rules! chromeleon_event {
    ($name:ident, $method:expr, $doc:expr) => {
        #[doc = $doc]
        ///
        /// Subscribe with `page.event_listener::<Self>()` — chromiumoxide
        /// dispatches unknown events by name, so no generated type is needed.
        #[derive(Debug, Clone, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            /// The sitekey, or the embedder host for the hCaptcha-checkbox,
            /// DataDome and PerimeterX solvers.
            #[serde(default)]
            pub sitekey: String,
            /// Which solver family is working it (`captchaSolving` only).
            #[serde(default)]
            pub method: Option<SolveMethod>,
            /// Attempts made (`captchaSolved` / `captchaFailed`).
            #[serde(default)]
            pub attempts: u32,
            /// Wall time in milliseconds (`captchaSolved`).
            #[serde(default)]
            pub time_ms: f64,
            /// Why it stopped (`captchaFailed`).
            #[serde(default)]
            pub reason: String,
        }

        impl MethodType for $name {
            fn method_id() -> MethodId {
                $method.into()
            }
        }

        impl CustomEvent for $name {}

        impl $name {
            /// As the driver-agnostic [`CaptchaEvent`].
            pub fn to_event(&self) -> Option<CaptchaEvent> {
                CaptchaEvent::parse($method, &serde_json::to_value(self.as_params()).ok()?)
            }

            fn as_params(&self) -> Value {
                serde_json::json!({
                    "sitekey": self.sitekey,
                    "method": self.method,
                    "attempts": self.attempts,
                    "timeMs": self.time_ms,
                    "reason": self.reason,
                })
            }
        }
    };
}

chromeleon_event!(
    CaptchaDetectedEvent,
    CAPTCHA_DETECTED,
    "`Chromeleon.captchaDetected`: a challenge was found."
);
chromeleon_event!(
    CaptchaSolvingEvent,
    CAPTCHA_SOLVING,
    "`Chromeleon.captchaSolving`: a solve started."
);
chromeleon_event!(
    CaptchaSolvedEvent,
    CAPTCHA_SOLVED,
    "`Chromeleon.captchaSolved`: a solve finished and the token was written."
);
chromeleon_event!(
    CaptchaFailedEvent,
    CAPTCHA_FAILED,
    "`Chromeleon.captchaFailed`: a solve gave up."
);

/// `Chromeleon.solverEvalResult`: the string a [`solver_eval`] produced.
///
/// Subscribe with `page.event_listener::<Self>()`. ⚠️ Events are broadcast to
/// every enabled session in the browser process, so this cannot be correlated
/// to a particular `solverEval` call by the protocol alone.
#[derive(Debug, Clone, Deserialize)]
pub struct SolverEvalResultEvent {
    /// The expression's string result.
    #[serde(default)]
    pub result: String,
}

impl MethodType for SolverEvalResultEvent {
    fn method_id() -> MethodId {
        SOLVER_EVAL_RESULT.into()
    }
}

impl CustomEvent for SolverEvalResultEvent {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_raw_command_serializes_to_its_params_and_carries_its_method() {
        let cmd = RawCommand::new(
            CREDENTIALS_METHOD,
            serde_json::json!({"proxyServer": "http://gw:1"}),
        );
        assert_eq!(cmd.identifier().as_ref(), "Target.setProxyCredentials");
        assert_eq!(
            serde_json::to_value(&cmd).unwrap(),
            serde_json::json!({"proxyServer": "http://gw:1"})
        );
    }

    #[test]
    fn curated_defaults_drop_the_tells() {
        let kept = kept_default_args();
        for gone in [
            "--enable-automation",
            "--lang=en_US",
            "--use-mock-keychain",
            "--metrics-recording-only",
            "--disable-breakpad",
        ] {
            assert!(!kept.iter().any(|a| a == gone), "{gone} survived");
        }
        // …while the harmless ones stay, or this would be a different browser.
        for kept_arg in [
            "--disable-dev-shm-usage",
            "--no-first-run",
            "--disable-sync",
        ] {
            assert!(kept.iter().any(|a| a == kept_arg), "{kept_arg} was dropped");
        }
    }

    #[test]
    fn the_documented_overlap_with_our_suppression_list_is_the_real_one() {
        use crate::launch::SUPPRESSED_DEFAULT_ARGS;
        let overlap: Vec<&str> = CHROMIUMOXIDE_DEFAULT_ARGS
            .iter()
            .copied()
            .filter(|a| crate::launch::is_suppressed_default_arg(a))
            .collect();
        // The doc comment says 6; a chromiumoxide release that adds another of
        // our switches should fail here rather than quietly make the docs wrong.
        assert_eq!(overlap.len(), 6, "overlap changed: {overlap:?}");
        // …and `--disable-extensions` is NOT among them, because chromiumoxide
        // adds it separately and unconditionally. That is the caveat in the docs.
        assert!(SUPPRESSED_DEFAULT_ARGS.contains(&"--disable-extensions"));
        assert!(!overlap.contains(&"--disable-extensions"));
    }

    #[test]
    fn events_convert_to_the_driver_agnostic_form() {
        let solved = CaptchaSolvedEvent {
            sitekey: "abc".into(),
            method: None,
            attempts: 2,
            time_ms: 12.0,
            reason: String::new(),
        };
        assert_eq!(
            solved.to_event(),
            Some(CaptchaEvent::Solved {
                sitekey: "abc".into(),
                attempts: 2,
                time_ms: 12.0
            })
        );
    }
}
