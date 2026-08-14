//! The built-in captcha solver, and the `Chromeleon` CDP domain that observes it.
//!
//! Chromeleon ships a solver for reCAPTCHA, hCaptcha, Turnstile, DataDome and
//! PerimeterX. Once enabled at launch it works AUTOMATICALLY — it detects the
//! widget, solves it and writes the token; you do not call it. The release
//! binary embeds the models, so [`CAPTCHA_SOLVER_SWITCH`] alone is enough.
//! Which challenge each family gets is [`SolveMethod`]; there is no image-grid
//! mode, and reCAPTCHA v3 is out of scope.
//!
//! The `Chromeleon` CDP domain only OBSERVES and STEERS that solver: enable it
//! on a **page** CDP session to receive lifecycle events, and use
//! [`SOLVER_EVAL_METHOD`] to run JS in the solver's isolated world (world 10),
//! which pierces CLOSED shadow roots. Like the proxy handshake these are
//! protocol facts, so the constants and the parameter/event types live here and
//! the sending is your driver's job.
//!
//! Two behaviours that are not obvious from the method names, and that both
//! read as "the client is broken" when you meet them:
//!
//! * **The domain only exists when the browser was launched with
//!   `--captcha-solver`.** It is stripped from `/json/protocol` otherwise, so
//!   [`ENABLE_METHOD`] comes back as an *unknown method*, not as "solver off".
//! * **Events are broadcast to every enabled session in the browser process**,
//!   not routed to the page that raised them. A [`SOLVER_EVAL_RESULT`] cannot be
//!   correlated to the [`SOLVER_EVAL_METHOD`] call that caused it, and a solve
//!   on one page is visible to a session enabled on another. Treat the events as
//!   a process-wide feed, and if you need per-page attribution, put an
//!   identifying token in the expression's own output.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Launch switch that turns the solver on.
pub const CAPTCHA_SOLVER_SWITCH: &str = "--captcha-solver";
/// Model-dir override. Dev/self-host only — the release binary embeds models.
pub const CAPTCHA_MODEL_PATH_SWITCH: &str = "--captcha-model-path";

/// Start lifecycle notifications on a page CDP session.
pub const ENABLE_METHOD: &str = "Chromeleon.enable";
/// Stop lifecycle notifications on a page CDP session.
pub const DISABLE_METHOD: &str = "Chromeleon.disable";
/// Evaluate JS in the solver's isolated world. See [`solver_eval_params`].
pub const SOLVER_EVAL_METHOD: &str = "Chromeleon.solverEval";

/// A challenge was found on the page. Payload: `{sitekey}`.
pub const CAPTCHA_DETECTED: &str = "Chromeleon.captchaDetected";
/// A solve started. Payload: `{sitekey, method}`.
pub const CAPTCHA_SOLVING: &str = "Chromeleon.captchaSolving";
/// A solve succeeded and the token was written. Payload: `{sitekey, attempts, timeMs}`.
pub const CAPTCHA_SOLVED: &str = "Chromeleon.captchaSolved";
/// A solve gave up. Payload: `{sitekey, attempts, reason}`.
pub const CAPTCHA_FAILED: &str = "Chromeleon.captchaFailed";
/// The result of a [`SOLVER_EVAL_METHOD`] call. Payload: `{result}`.
pub const SOLVER_EVAL_RESULT: &str = "Chromeleon.solverEvalResult";

/// The four lifecycle events: detected → solving → solved | failed.
pub const CAPTCHA_EVENTS: &[&str] = &[
    CAPTCHA_DETECTED,
    CAPTCHA_SOLVING,
    CAPTCHA_SOLVED,
    CAPTCHA_FAILED,
];

/// Launch flags that turn on the built-in captcha solver.
///
/// The release binary embeds the models, so this is just
/// `--captcha-solver`; `model_path` is a dev/self-host override that also
/// appends `--captcha-model-path=<dir>`.
pub fn captcha_launch_args(model_path: Option<&str>) -> Vec<String> {
    let mut args = vec![CAPTCHA_SOLVER_SWITCH.to_string()];
    if let Some(dir) = model_path {
        args.push(format!("{CAPTCHA_MODEL_PATH_SWITCH}={dir}"));
    }
    args
}

/// Params for `Chromeleon.solverEval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverEvalParams<'a> {
    /// JS to run in the solver's isolated world.
    pub expression: &'a str,
    /// Substring of the target subframe's URL; empty means the main frame.
    pub frame_url_contains: &'a str,
}

impl SolverEvalParams<'_> {
    /// The same params as a `serde_json::Value`, for drivers that take one.
    pub fn to_value(self) -> Value {
        serde_json::json!({
            "expression": self.expression,
            "frameUrlContains": self.frame_url_contains,
        })
    }
}

/// Params for `Chromeleon.solverEval`.
///
/// `expression` runs in the solver's isolated world (pierces CLOSED shadow
/// roots). `frame_url_contains` selects a subframe whose committed URL contains
/// that substring (for cross-origin OOPIFs); the empty string targets the
/// primary main frame.
///
/// ⚠️ The string result arrives as a [`SOLVER_EVAL_RESULT`] **event**, not as
/// the command's return value. A driver that only awaits the command return
/// sees an empty result and concludes the expression did nothing.
pub fn solver_eval_params<'a>(
    expression: &'a str,
    frame_url_contains: &'a str,
) -> SolverEvalParams<'a> {
    SolverEvalParams {
        expression,
        frame_url_contains,
    }
}

/// Which challenge the solver is working, one per solver family.
///
/// These are the values the binary actually emits — not a difficulty ladder and
/// not "audio, falling back to image": there is no image mode, because image
/// grids (and reCAPTCHA v3) are explicitly out of scope for the solver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SolveMethod {
    /// reCAPTCHA v2 / Enterprise, solved through the audio challenge.
    Audio,
    /// hCaptcha or Turnstile, solved at the checkbox.
    Checkbox,
    /// A DataDome slider.
    Slider,
    /// A PerimeterX press-and-hold.
    #[serde(rename = "press-hold")]
    PressHold,
    /// Anything a newer binary reports that this client predates.
    #[serde(other)]
    Other,
}

/// A parsed `Chromeleon` domain event.
///
/// ⚠️ `sitekey` is the challenge's sitekey only for reCAPTCHA. The
/// hCaptcha-checkbox, DataDome and PerimeterX orchestrators put the **embedder
/// host** in that field instead, so treat it as an identifier for the
/// challenge, not as a key you can look up.
///
/// Deserialization is deliberately tolerant — a field a newer binary adds is
/// ignored, and one it stops sending reads as a default — because a client that
/// hard-fails on an unknown payload turns a working solve into a crash.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CaptchaEvent {
    /// A challenge was found on the page.
    Detected {
        /// The sitekey, or the embedder host — see the type-level note.
        sitekey: String,
    },
    /// A solve started.
    Solving {
        /// The sitekey, or the embedder host — see the type-level note.
        sitekey: String,
        /// Which solver family is working it.
        method: Option<SolveMethod>,
    },
    /// A solve finished and the token was written.
    Solved {
        /// The sitekey, or the embedder host — see the type-level note.
        sitekey: String,
        /// How many attempts it took.
        attempts: u32,
        /// Wall time for the solve, in milliseconds.
        time_ms: f64,
    },
    /// A solve gave up.
    Failed {
        /// The sitekey, or the embedder host — see the type-level note.
        sitekey: String,
        /// How many attempts were made before giving up.
        attempts: u32,
        /// Why it stopped — a free-form classifier string, e.g.
        /// `"max_attempts_reached"`, with `"error"` as the unclassified
        /// default. Match on it defensively; the set grows.
        reason: String,
    },
    /// The result of a [`SOLVER_EVAL_METHOD`] call.
    SolverEvalResult {
        /// The expression's string result.
        result: String,
    },
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawEvent {
    #[serde(default)]
    sitekey: String,
    #[serde(default)]
    method: Option<SolveMethod>,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    time_ms: f64,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    result: String,
}

impl CaptchaEvent {
    /// Parse a CDP event into a typed payload.
    ///
    /// Returns `None` for any method outside the `Chromeleon` domain, so this
    /// can sit directly in a driver's event loop.
    pub fn parse(method: &str, params: &Value) -> Option<CaptchaEvent> {
        let raw: RawEvent = serde_json::from_value(params.clone()).unwrap_or_default();
        Some(match method {
            CAPTCHA_DETECTED => CaptchaEvent::Detected {
                sitekey: raw.sitekey,
            },
            CAPTCHA_SOLVING => CaptchaEvent::Solving {
                sitekey: raw.sitekey,
                method: raw.method,
            },
            CAPTCHA_SOLVED => CaptchaEvent::Solved {
                sitekey: raw.sitekey,
                attempts: raw.attempts,
                time_ms: raw.time_ms,
            },
            CAPTCHA_FAILED => CaptchaEvent::Failed {
                sitekey: raw.sitekey,
                attempts: raw.attempts,
                reason: raw.reason,
            },
            SOLVER_EVAL_RESULT => CaptchaEvent::SolverEvalResult { result: raw.result },
            _ => return None,
        })
    }

    /// The challenge identifier the event is about — a sitekey for reCAPTCHA,
    /// the embedder host for the other three solvers.
    pub fn sitekey(&self) -> Option<&str> {
        match self {
            CaptchaEvent::Detected { sitekey }
            | CaptchaEvent::Solving { sitekey, .. }
            | CaptchaEvent::Solved { sitekey, .. }
            | CaptchaEvent::Failed { sitekey, .. } => Some(sitekey),
            CaptchaEvent::SolverEvalResult { .. } => None,
        }
    }

    /// True for the two events that end a solve.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            CaptchaEvent::Solved { .. } | CaptchaEvent::Failed { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_args_stay_minimal_without_a_model_path() {
        assert_eq!(captcha_launch_args(None), ["--captcha-solver"]);
        assert_eq!(
            captcha_launch_args(Some("/opt/models")),
            ["--captcha-solver", "--captcha-model-path=/opt/models"]
        );
    }

    #[test]
    fn events_parse_and_tolerate_drift() {
        let solved = CaptchaEvent::parse(
            CAPTCHA_SOLVED,
            &serde_json::json!({"sitekey": "abc", "attempts": 2, "timeMs": 1234.5, "newField": 1}),
        );
        assert_eq!(
            solved,
            Some(CaptchaEvent::Solved {
                sitekey: "abc".into(),
                attempts: 2,
                time_ms: 1234.5
            })
        );
        // The four methods the binary actually emits, spelled as it spells them.
        for (wire, expected) in [
            ("audio", SolveMethod::Audio),
            ("checkbox", SolveMethod::Checkbox),
            ("slider", SolveMethod::Slider),
            ("press-hold", SolveMethod::PressHold),
            // An unknown method must not lose the event.
            ("telepathy", SolveMethod::Other),
        ] {
            let solving = CaptchaEvent::parse(
                CAPTCHA_SOLVING,
                &serde_json::json!({"sitekey": "abc", "method": wire}),
            );
            assert_eq!(
                solving,
                Some(CaptchaEvent::Solving {
                    sitekey: "abc".into(),
                    method: Some(expected)
                }),
                "method {wire:?}"
            );
        }
        assert!(CaptchaEvent::parse("Page.loadEventFired", &serde_json::json!({})).is_none());
    }

    #[test]
    fn solver_eval_params_are_camel_case_on_the_wire() {
        let params = solver_eval_params("document.title", "");
        assert_eq!(
            params.to_value(),
            serde_json::json!({"expression": "document.title", "frameUrlContains": ""})
        );
        assert_eq!(serde_json::to_value(params).unwrap(), params.to_value());
    }
}
