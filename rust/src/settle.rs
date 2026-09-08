//! Page completion: `Chromeleon.waitForSettle`, with Blink's
//! `networkAlmostIdle` as the fallback.
//!
//! "The page has finished" is not a thing CDP answers. `load` fires when the
//! subresources of the *first* document are in, `domcontentloaded` before that,
//! and neither says anything about the text a scraper came for. Chromeleon's
//! own command does: [`WAIT_FOR_SETTLE_METHOD`] settles when the main frame's
//! **rendered text** has been unchanged for a quiet window, sampled in the
//! browser process over Blink's inner-text channel. No JavaScript runs in the
//! page, nothing is registered on the document, and there is no isolated world
//! — which is the point of it over a `MutationObserver` that a bot wall can
//! see.
//!
//! # Why this order
//!
//! Measured over 960 navigations, 80 sites, 3 rounds:
//!
//! ```text
//!                        direct p50 | direct never | proxied p50 | proxied never
//! networkAlmostIdle         3,989ms |          1%  |   11,285ms  |        36%
//! Chromeleon.waitForSettle  7,306ms |          6%  |   12,108ms  |         7%
//! ```
//!
//! `waitForSettle` is the primary because **through a proxy** — which is what
//! this client is for — `networkAlmostIdle` never fires on more than a third of
//! loads, against 7% for `waitForSettle`. On a direct connection
//! `networkAlmostIdle` is 1.8x faster for the same median content, which is why
//! it stays available and is the documented choice for un-proxied work
//! ([`WaitOptions::prefer_lifecycle`]). On completeness, direct:
//! `waitForSettle` reproduced the final text exactly on 79% of loads,
//! `networkAlmostIdle` on 59%, the load event on 40%, `domcontentloaded` on 7%.
//!
//! # Two-phase, because the ordering mistake is fatal and silent
//!
//! The challenge header is recorded at **commit** time by a tracker attached
//! when the browser-side handler is constructed. A CDP session attached to an
//! already-committed document cannot see it and degrades to status-code-only
//! detection without saying so. So there is no one-shot `wait_for_settle(page)`
//! here: [`SettleWatch::arm`] runs before the navigation, [`SettleWatch::wait`]
//! after it, and the shape makes the mistake impossible to spell.
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
//! let mut watch = SettleWatch::arm(session).await?;   // attaches BEFORE the navigation
//! // …now navigate: committing the document is enough, the watch is recording.
//! let state = watch.wait(WaitOptions::new().timeout_ms(30_000)).await?;
//! if state.blocked() {
//!     // A bot wall — 12.5% of proxied navigations in production measurement.
//!     // NEVER treat this as a successful load.
//! }
//! watch.close().await;
//! # Ok(()) }
//! ```
//!
//! # The launch switch
//!
//! The `Chromeleon` settle domain is registered **only** when the browser was
//! launched with [`PAGE_SETTLE_SWITCH`], exactly as the captcha domain is
//! registered only under `--captcha-solver`. Without it the command comes back
//! as CDP's `-32601`, *method not found* — not as "settling is off" — and this
//! module falls back rather than raising. [`settle_launch_args`] renders the
//! switch and its tuning, and [`Launcher::page_settle`](crate::launch::Launcher::page_settle)
//! puts them on the command line. It is deliberately **not** in
//! [`LAUNCH_ARGS`](crate::launch::LAUNCH_ARGS): it changes browser behaviour and
//! must be opt-in.
//!
//! # What a driver has to supply
//!
//! This crate depends on no browser library, so a settle watch cannot own a CDP
//! session any more than the handshake can own a connection. [`PageSession`] is
//! the whole contract — send a command on a **page** session, hand over the
//! next event, and (optionally) say what a detach means — and it is a few lines
//! over chromiumoxide, over `headless_chrome`, or over a raw DevTools
//! WebSocket. [`BlockingPageSession`] is the same contract for synchronous
//! drivers.

use std::collections::HashSet;
use std::fmt;
use std::future::{poll_fn, Future};
use std::pin::pin;
use std::task::Poll;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::core::Error;

/// The command. Registered only under [`PAGE_SETTLE_SWITCH`].
pub const WAIT_FOR_SETTLE_METHOD: &str = "Chromeleon.waitForSettle";

/// Launch switch that registers the settle domain.
pub const PAGE_SETTLE_SWITCH: &str = "--page-settle";
/// Quiet window the rendered text must be unchanged for. Browser default 4000ms.
pub const PAGE_SETTLE_QUIET_WINDOW_SWITCH: &str = "--page-settle-quiet-window-ms";
/// Minimum rendered-text length before a settle is believed.
pub const PAGE_SETTLE_MIN_CHARS_SWITCH: &str = "--page-settle-min-chars";
/// Browser-side ceiling on one `waitForSettle`.
pub const PAGE_SETTLE_TIMEOUT_SWITCH: &str = "--page-settle-timeout-ms";
/// How often the browser samples the rendered text.
pub const PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH: &str = "--page-settle-sample-interval-ms";
/// Include text inside shadow roots in the sample.
pub const PAGE_SETTLE_PIERCE_SHADOW_SWITCH: &str = "--page-settle-pierce-shadow";

/// `Page.enable` — the lifecycle feed is a Page-domain feed.
pub const PAGE_ENABLE_METHOD: &str = "Page.enable";
/// `Page.getFrameTree` — how the main frame id is learned.
pub const GET_FRAME_TREE_METHOD: &str = "Page.getFrameTree";
/// `Page.setLifecycleEventsEnabled` — turns [`LIFECYCLE_EVENT`] on.
pub const SET_LIFECYCLE_EVENTS_ENABLED_METHOD: &str = "Page.setLifecycleEventsEnabled";
/// The event the fallback reads.
pub const LIFECYCLE_EVENT: &str = "Page.lifecycleEvent";

/// Blink's "at most 2 in-flight requests, sustained 500ms".
pub const NETWORK_ALMOST_IDLE: &str = "networkAlmostIdle";
/// Blink's "0 in-flight requests, sustained 500ms" — the stricter sibling.
pub const NETWORK_IDLE: &str = "networkIdle";
/// The `load` lifecycle name, the last-resort fallback signal.
pub const LOAD_LIFECYCLE: &str = "load";

/// CDP's *method not found*. The browser answers `waitForSettle` with it when
/// it was launched without [`PAGE_SETTLE_SWITCH`].
pub const METHOD_NOT_FOUND: i64 = -32601;

/// Statuses a bot wall is served with. See [`SettleState::blocked`].
pub const BLOCKED_STATUSES: &[u16] = &[401, 403, 407, 429, 503];

/// How long [`SettleWatch::arm`] keeps reading before it calls the loaders it
/// has seen stale.
///
/// `Page.setLifecycleEventsEnabled` **replays** a full lifecycle for the
/// incumbent document — the `about:blank` a fresh tab is sitting on — and every
/// one of those events is already `networkAlmostIdle`-complete. Counting them
/// makes every fallback signal read as ~0ms, which looks like a very fast site
/// and is really a watch that measured nothing.
pub const REPLAY_WINDOW: Duration = Duration::from_millis(350);

/// The browser's own default quiet window, for reference; not sent unless asked.
pub const DEFAULT_QUIET_WINDOW_MS: u32 = 4000;

/// The wait bound applied when [`WaitOptions::timeout_ms`] is not set.
pub const DEFAULT_TIMEOUT_MS: u32 = 30_000;

/// How far ahead of our own deadline the browser is asked to give up, so its
/// answer (with an outcome and a status in it) wins the race against a bare
/// client-side timeout.
const COMMAND_TIMEOUT_GRACE_MS: u32 = 250;

// --- launch ----------------------------------------------------------------

/// The optional `--page-settle-*` tuning switches.
///
/// Every field is `None` by default, which leaves the browser on its own
/// defaults (a 4000ms quiet window). Renders through [`settle_launch_args`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SettleTuning {
    /// `--page-settle-quiet-window-ms`.
    pub quiet_window_ms: Option<u32>,
    /// `--page-settle-min-chars`.
    pub min_chars: Option<u32>,
    /// `--page-settle-timeout-ms`.
    pub timeout_ms: Option<u32>,
    /// `--page-settle-sample-interval-ms`.
    pub sample_interval_ms: Option<u32>,
    /// `--page-settle-pierce-shadow`.
    pub pierce_shadow: bool,
}

impl SettleTuning {
    /// The browser's defaults: the switch alone, nothing tuned.
    pub fn new() -> Self {
        SettleTuning::default()
    }

    /// Set the quiet window the rendered text must be unchanged for.
    pub fn quiet_window_ms(mut self, ms: u32) -> Self {
        self.quiet_window_ms = Some(ms);
        self
    }

    /// Set the minimum rendered-text length.
    ///
    /// ⚠️ A bot wall SHORTER than this reports `timeout`, not `challenge` —
    /// which is why [`SettleState::blocked`] classifies on the outcome **or**
    /// the status.
    pub fn min_chars(mut self, chars: u32) -> Self {
        self.min_chars = Some(chars);
        self
    }

    /// Set the browser-side ceiling on one settle.
    pub fn timeout_ms(mut self, ms: u32) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    /// Set how often the browser samples the rendered text.
    pub fn sample_interval_ms(mut self, ms: u32) -> Self {
        self.sample_interval_ms = Some(ms);
        self
    }

    /// Sample text inside shadow roots too.
    pub fn pierce_shadow(mut self, on: bool) -> Self {
        self.pierce_shadow = on;
        self
    }
}

/// Launch flags that register the settle domain.
///
/// ```
/// # use chromeleon::settle::{settle_launch_args, SettleTuning};
/// assert_eq!(settle_launch_args(SettleTuning::new()), ["--page-settle"]);
/// assert_eq!(
///     settle_launch_args(SettleTuning::new().quiet_window_ms(1500)),
///     ["--page-settle", "--page-settle-quiet-window-ms=1500"]
/// );
/// ```
///
/// Kept out of [`LAUNCH_ARGS`](crate::launch::LAUNCH_ARGS) on purpose — like
/// [`captcha_launch_args`](crate::captcha::captcha_launch_args), this changes
/// what the browser does and is opt-in.
pub fn settle_launch_args(tuning: SettleTuning) -> Vec<String> {
    let mut args = vec![PAGE_SETTLE_SWITCH.to_string()];
    if let Some(ms) = tuning.quiet_window_ms {
        args.push(format!("{PAGE_SETTLE_QUIET_WINDOW_SWITCH}={ms}"));
    }
    if let Some(chars) = tuning.min_chars {
        args.push(format!("{PAGE_SETTLE_MIN_CHARS_SWITCH}={chars}"));
    }
    if let Some(ms) = tuning.timeout_ms {
        args.push(format!("{PAGE_SETTLE_TIMEOUT_SWITCH}={ms}"));
    }
    if let Some(ms) = tuning.sample_interval_ms {
        args.push(format!("{PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH}={ms}"));
    }
    if tuning.pierce_shadow {
        args.push(PAGE_SETTLE_PIERCE_SHADOW_SWITCH.to_string());
    }
    args
}

// --- the answer ------------------------------------------------------------

/// What the browser (or the fallback) concluded.
///
/// `#[non_exhaustive]`, and unknown wire values are kept as
/// [`SettleOutcome::Other`] rather than dropped: a newer binary reporting an
/// outcome this build predates must not turn a finished navigation into a
/// parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SettleOutcome {
    /// The rendered text stopped changing for the quiet window.
    Settled,
    /// It never did, inside the time allowed. Normal on proxied traffic.
    Timeout,
    /// A bot wall. **Not** a successful load, however cleanly it settled.
    Challenge,
    /// Something a newer binary reports that this client predates.
    Other(String),
}

impl SettleOutcome {
    /// Read the wire value.
    pub fn from_wire(outcome: &str) -> Self {
        match outcome {
            "settled" => SettleOutcome::Settled,
            "timeout" => SettleOutcome::Timeout,
            "challenge" => SettleOutcome::Challenge,
            other => SettleOutcome::Other(other.to_string()),
        }
    }

    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            SettleOutcome::Settled => "settled",
            SettleOutcome::Timeout => "timeout",
            SettleOutcome::Challenge => "challenge",
            SettleOutcome::Other(other) => other,
        }
    }
}

impl fmt::Display for SettleOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which signal actually produced the answer.
///
/// Always worth logging: a fleet quietly running on
/// [`SettleVia::NetworkAlmostIdle`] is a fleet launched without
/// [`PAGE_SETTLE_SWITCH`], and on proxied traffic that is the 36%-never-fires
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SettleVia {
    /// The browser's own command answered. The primary path.
    WaitForSettle,
    /// Blink's `networkAlmostIdle`, on the main frame, for this navigation.
    NetworkAlmostIdle,
    /// The `load` lifecycle event — reproduced the final text on 40% of loads.
    Load,
    /// Nothing did, before the deadline.
    Timeout,
}

impl SettleVia {
    /// The name to log.
    pub fn as_str(&self) -> &'static str {
        match self {
            SettleVia::WaitForSettle => "waitForSettle",
            SettleVia::NetworkAlmostIdle => "networkAlmostIdle",
            SettleVia::Load => "load",
            SettleVia::Timeout => "timeout",
        }
    }
}

impl fmt::Display for SettleVia {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The result of a wait.
///
/// Built by [`SettleWatch::wait`] and by [`SettleState::from_reply`]; the
/// fields a fallback cannot know (`text_length`, `http_status`) are `None`
/// there rather than zero, because "0 characters" and "we never asked" are
/// different answers and only one of them is a reason to retry.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SettleState {
    /// What the browser concluded.
    pub outcome: SettleOutcome,
    /// Milliseconds the navigation took, from the browser's own clock when the
    /// browser answered, and from the main frame's lifecycle timestamps when
    /// the fallback did.
    pub elapsed_ms: f64,
    /// Rendered-text length at settle. `None` on a fallback, which never
    /// sampled the text.
    pub text_length: Option<u64>,
    /// Navigations the watch saw — a redirect chain counts more than one.
    pub navigations: u32,
    /// The committed document's HTTP status. `None` on a fallback.
    pub http_status: Option<u16>,
    /// The browser's own free-form explanation, or this client's when it fell
    /// back. Match on it defensively; the set grows.
    pub reason: String,
    /// Which signal produced this answer.
    pub via: SettleVia,
}

impl SettleState {
    /// **The classification callers need**: a bot wall, or a status that is one.
    ///
    /// Two ways to meet the same wall, and a caller that checks only the first
    /// misses the short ones: a challenge page shorter than
    /// `--page-settle-min-chars` reports [`SettleOutcome::Timeout`], and only
    /// [`SettleState::http_status`] gives it away.
    ///
    /// ```
    /// # use chromeleon::settle::SettleState;
    /// # use serde_json::json;
    /// let short_wall = SettleState::from_reply(
    ///     &json!({"outcome": "timeout", "httpStatus": 403, "elapsedMs": 30000.0}),
    /// ).unwrap();
    /// assert!(short_wall.blocked());
    /// ```
    pub fn blocked(&self) -> bool {
        self.outcome == SettleOutcome::Challenge
            || self
                .http_status
                .is_some_and(|status| BLOCKED_STATUSES.contains(&status))
    }

    /// Some signal reported completion, and it was not a wall.
    ///
    /// A challenge page settles beautifully — that is what makes it a good
    /// challenge page — so [`SettleState::blocked`] is folded in here rather
    /// than left for the caller to remember. The sibling clients spell this
    /// `settled`.
    pub fn is_settled(&self) -> bool {
        self.outcome == SettleOutcome::Settled && !self.blocked()
    }

    /// Parse a `Chromeleon.waitForSettle` reply.
    ///
    /// Takes the command's **`result` object**, and also the whole CDP envelope
    /// — a raw WebSocket hands you the latter, and looking one level down for
    /// the outcome is cheaper than making every caller unwrap it. `None` when
    /// there is no `outcome` at all, which is what an error envelope and an
    /// unrelated reply both look like.
    ///
    /// Unknown members are ignored and missing ones read as absent, the same
    /// tolerance [`CaptchaEvent::parse`](crate::captcha::CaptchaEvent::parse)
    /// applies: a client that hard-fails on payload drift turns a working
    /// navigation into a crash.
    pub fn from_reply(reply: &Value) -> Option<SettleState> {
        let body = settle_body(reply);
        let outcome = body.get("outcome")?.as_str()?;
        Some(SettleState {
            outcome: SettleOutcome::from_wire(outcome),
            elapsed_ms: body.get("elapsedMs").and_then(Value::as_f64).unwrap_or(0.0),
            text_length: body.get("textLength").and_then(Value::as_u64),
            navigations: body
                .get("navigations")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
            http_status: body
                .get("httpStatus")
                .and_then(Value::as_u64)
                .and_then(|s| u16::try_from(s).ok()),
            reason: body
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            via: SettleVia::WaitForSettle,
        })
    }
}

/// The reply body, whether it arrived bare or inside a CDP envelope.
fn settle_body(reply: &Value) -> &Value {
    if reply.get("outcome").is_some() {
        return reply;
    }
    match reply.get("result") {
        Some(result) if result.get("outcome").is_some() => result,
        _ => reply,
    }
}

/// True when a CDP reply **envelope** carries `-32601`, i.e. the browser was
/// launched without [`PAGE_SETTLE_SWITCH`].
///
/// A typed driver turns that into its own error before you see it, which is
/// what [`PageSession::method_unavailable`] is for; a raw WebSocket hands you
/// the envelope as a perfectly successful send.
pub fn is_method_not_found(reply: &Value) -> bool {
    let Some(error) = reply.get("error") else {
        return false;
    };
    if error.get("code").and_then(Value::as_i64) == Some(METHOD_NOT_FOUND) {
        return true;
    }
    error
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(error_reads_as_unavailable)
}

/// True when a driver's error text reads like CDP's *method not found*.
///
/// The default of [`PageSession::method_unavailable`], and a heuristic on
/// purpose: every driver renders a CDP error its own way, and the alternative —
/// raising out of `wait()` because a browser was launched without a switch — is
/// the failure this module exists to avoid. Override the trait method when your
/// driver gives you the code.
pub fn error_reads_as_unavailable(text: &str) -> bool {
    if text.contains(&METHOD_NOT_FOUND.to_string()) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    lower.contains("wasn't found")
        || lower.contains("was not found")
        || lower.contains("method not found")
        || lower.contains("not implemented")
        || lower.contains("unknown method")
}

/// The `error` member of a CDP envelope, rendered for a [`SettleState::reason`].
fn error_detail(reply: &Value) -> Option<String> {
    match reply.get("error") {
        Some(error) if !error.is_null() => Some(error.to_string()),
        _ => None,
    }
}

/// The main frame's id from a `Page.getFrameTree` reply, envelope or not.
pub fn main_frame_id(frame_tree_reply: &Value) -> Option<&str> {
    let tree = frame_tree_reply
        .get("frameTree")
        .or_else(|| frame_tree_reply.get("result")?.get("frameTree"))?;
    tree.get("frame")?.get("id")?.as_str()
}

// --- the lifecycle state machine -------------------------------------------

/// The fallback's bookkeeping, with no transport in it.
///
/// Feed it `Page.lifecycleEvent`s and it answers the only two questions the
/// fallback has: *is this event this navigation's* and *is it the main
/// frame's*. Public because a driver that drives its own event loop should not
/// have to reimplement the two rules, both of which were measured rather than
/// guessed:
///
/// * **Replayed lifecycle is discarded.** `Page.setLifecycleEventsEnabled`
///   replays the incumbent `about:blank`'s complete lifecycle, so everything
///   seen inside [`REPLAY_WINDOW`] is recorded as a stale loader and never
///   counted again.
/// * **Subframes are ignored.** `bbc.com/news` emits lifecycle from 23 frames;
///   first-across-all reports `networkIdle` at 699ms when the main frame's real
///   value is 6094ms.
#[derive(Debug, Clone)]
pub struct LifecycleTracker {
    main_frame_id: Option<String>,
    stale_loaders: HashSet<String>,
    live_loaders: HashSet<String>,
    arming: bool,
    started_at: Option<f64>,
    saw_almost_idle: bool,
    almost_idle_at: Option<f64>,
    saw_load: bool,
    load_at: Option<f64>,
}

impl Default for LifecycleTracker {
    fn default() -> Self {
        LifecycleTracker::new()
    }
}

impl LifecycleTracker {
    /// A tracker that is still **arming**: everything it is shown is stale
    /// until [`LifecycleTracker::finish_arming`].
    pub fn new() -> Self {
        LifecycleTracker {
            main_frame_id: None,
            stale_loaders: HashSet::new(),
            live_loaders: HashSet::new(),
            arming: true,
            started_at: None,
            saw_almost_idle: false,
            almost_idle_at: None,
            saw_load: false,
            load_at: None,
        }
    }

    /// Name the main frame. Without it no fallback signal is ever counted,
    /// because a tracker that cannot tell a subframe from the main frame would
    /// answer with the first iframe that finished.
    pub fn set_main_frame_id(&mut self, frame_id: impl Into<String>) {
        self.main_frame_id = Some(frame_id.into());
    }

    /// The main frame id, when one has been learned.
    pub fn main_frame_id(&self) -> Option<&str> {
        self.main_frame_id.as_deref()
    }

    /// Still discarding replayed lifecycle?
    pub fn arming(&self) -> bool {
        self.arming
    }

    /// Close the replay window: what has been seen is the incumbent document's,
    /// what comes next is the navigation's.
    pub fn finish_arming(&mut self) {
        self.arming = false;
    }

    /// Show the tracker one CDP event. Anything but [`LIFECYCLE_EVENT`] is
    /// ignored, so this can sit directly in a driver's event loop.
    pub fn observe(&mut self, method: &str, params: &Value) {
        if method != LIFECYCLE_EVENT {
            return;
        }
        let loader_id = params
            .get("loaderId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if self.arming {
            self.stale_loaders.insert(loader_id.to_string());
            return;
        }
        let frame_id = params
            .get("frameId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match self.main_frame_id.as_deref() {
            Some(main) if main == frame_id => {}
            _ => return,
        }
        if self.stale_loaders.contains(loader_id) {
            return;
        }
        let timestamp = params.get("timestamp").and_then(Value::as_f64);
        if self.live_loaders.insert(loader_id.to_string()) && self.started_at.is_none() {
            self.started_at = timestamp;
        }
        match params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            NETWORK_ALMOST_IDLE if !self.saw_almost_idle => {
                self.saw_almost_idle = true;
                self.almost_idle_at = timestamp;
            }
            LOAD_LIFECYCLE if !self.saw_load => {
                self.saw_load = true;
                self.load_at = timestamp;
            }
            _ => {}
        }
    }

    /// How many loaders were written off as replay.
    pub fn stale_loader_count(&self) -> usize {
        self.stale_loaders.len()
    }

    /// Main-frame navigations counted since arming — a redirect chain is more
    /// than one.
    pub fn navigations(&self) -> u32 {
        u32::try_from(self.live_loaders.len()).unwrap_or(u32::MAX)
    }

    /// Has a main-frame, non-stale `networkAlmostIdle` arrived?
    pub fn network_almost_idle(&self) -> bool {
        self.saw_almost_idle
    }

    /// Has a main-frame, non-stale `load` arrived?
    pub fn loaded(&self) -> bool {
        self.saw_load
    }

    /// The best fallback evidence so far, and the CDP timestamp it carried.
    ///
    /// `networkAlmostIdle` beats `load`, which is the measured order of
    /// completeness (59% against 40%).
    pub fn signal(&self) -> Option<(SettleVia, Option<f64>)> {
        if self.saw_almost_idle {
            Some((SettleVia::NetworkAlmostIdle, self.almost_idle_at))
        } else if self.saw_load {
            Some((SettleVia::Load, self.load_at))
        } else {
            None
        }
    }

    /// Milliseconds from this navigation's first main-frame lifecycle event to
    /// `timestamp`, when both clocks are known.
    ///
    /// CDP lifecycle timestamps are monotonic **seconds**, and the browser's
    /// clock is the one to measure against: an elapsed taken from the client's
    /// own `Instant` includes whatever the driver's event loop was busy with.
    pub fn elapsed_ms(&self, timestamp: Option<f64>) -> Option<f64> {
        Some((timestamp? - self.started_at?) * 1000.0)
    }
}

// --- options ---------------------------------------------------------------

/// Per-wait tuning, and the client-side bound.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WaitOptions {
    /// Overrides the browser's quiet window for this wait.
    pub quiet_window_ms: Option<u32>,
    /// Overrides the browser's minimum rendered-text length for this wait.
    pub min_chars: Option<u32>,
    /// The whole wait's bound, client-side and browser-side. Default
    /// [`DEFAULT_TIMEOUT_MS`].
    pub timeout_ms: Option<u32>,
    /// Skip the command and take the `networkAlmostIdle` path directly.
    pub prefer_lifecycle: bool,
}

impl WaitOptions {
    /// The defaults: the browser's own tuning, [`DEFAULT_TIMEOUT_MS`].
    pub fn new() -> Self {
        WaitOptions::default()
    }

    /// Override the quiet window for this wait.
    pub fn quiet_window_ms(mut self, ms: u32) -> Self {
        self.quiet_window_ms = Some(ms);
        self
    }

    /// Override the minimum rendered-text length for this wait.
    pub fn min_chars(mut self, chars: u32) -> Self {
        self.min_chars = Some(chars);
        self
    }

    /// Bound the wait. Never exceeded, whatever the browser does.
    pub fn timeout_ms(mut self, ms: u32) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    /// Use Blink's `networkAlmostIdle` instead of the command.
    ///
    /// The documented choice for **un-proxied** work: on a direct connection it
    /// is 1.8x faster to the same median content and misses 1% of loads.
    /// Through a proxy it misses 36%, so this is not the default.
    pub fn prefer_lifecycle(mut self, on: bool) -> Self {
        self.prefer_lifecycle = on;
        self
    }

    /// Params for `Chromeleon.waitForSettle`, for a driver that sends the
    /// command itself.
    ///
    /// An unset field is not sent at all — never as `null` — so the browser
    /// keeps its launch-time default. [`SettleWatch::wait`] passes
    /// [`WaitOptions::timeout_ms`] less a 250ms grace, so that the browser's own
    /// answer (which carries the status and the challenge classification) beats
    /// the client-side cutoff instead of racing it to the same instant.
    ///
    /// ```
    /// # use chromeleon::settle::WaitOptions;
    /// # use serde_json::json;
    /// assert_eq!(
    ///     WaitOptions::new().quiet_window_ms(2500).command_params(Some(20_000)),
    ///     json!({"quietWindowMs": 2500, "timeoutMs": 20_000})
    /// );
    /// assert_eq!(WaitOptions::new().command_params(None), json!({}));
    /// ```
    pub fn command_params(&self, timeout_ms: Option<u32>) -> Value {
        let mut params = json!({});
        if let Some(quiet) = self.quiet_window_ms {
            params["quietWindowMs"] = json!(quiet);
        }
        if let Some(chars) = self.min_chars {
            params["minChars"] = json!(chars);
        }
        if let Some(ms) = timeout_ms {
            params["timeoutMs"] = json!(ms);
        }
        params
    }
}

/// The browser is asked to give up slightly before we do, so that its answer —
/// which carries the outcome and the status — beats our empty one.
fn command_timeout_ms(timeout_ms: u32) -> u32 {
    if timeout_ms > 2 * COMMAND_TIMEOUT_GRACE_MS {
        timeout_ms - COMMAND_TIMEOUT_GRACE_MS
    } else {
        timeout_ms
    }
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
}

/// Build the answer the fallback can honestly give.
///
/// `armed_at` is the client-clock origin — the moment before the navigation
/// started, which is the closest thing to the browser's own zero when no
/// lifecycle timestamp is available to measure against.
fn fallback_state(tracker: &LifecycleTracker, armed_at: Instant, reason: String) -> SettleState {
    let (via, outcome, at) = match tracker.signal() {
        Some((via, at)) => (via, SettleOutcome::Settled, at),
        None => (SettleVia::Timeout, SettleOutcome::Timeout, None),
    };
    SettleState {
        outcome,
        elapsed_ms: tracker
            .elapsed_ms(at)
            .unwrap_or_else(|| armed_at.elapsed().as_secs_f64() * 1000.0),
        text_length: None,
        navigations: tracker.navigations(),
        http_status: None,
        reason,
        via,
    }
}

// --- async -----------------------------------------------------------------

/// One CDP **page** session, as a settle watch needs to see it.
///
/// Three methods, two of which have defaults, and no browser library in sight —
/// the same trade the rest of this crate makes: you own the connection and the
/// runtime, this owns the protocol.
///
/// The contract, all of which the fallback depends on:
///
/// * [`send`](PageSession::send) issues a command on the **page** session, not
///   the browser one, and returns either the command's `result` object or the
///   whole CDP envelope. Both are understood.
/// * [`next_event`](PageSession::next_event) hands over the next CDP event as
///   `(method, params)`, and returns `None` when `timeout` elapses with none —
///   *after* waiting that long, because that timeout is also the watch's clock.
///   `None` ends the current wait (it reads as "nothing more is coming"), so an
///   implementation that returns it early ends the wait early; a closed event
///   channel is exactly that case and exactly that meaning.
///   Events must be **buffered from before the session was handed over**: an
///   implementation that starts listening on the first call drops the very
///   lifecycle events the fallback exists to read.
/// * It must be **cancel-safe**: a `next_event` future can be dropped before it
///   resolves (that is how a settled command stops the drain), and doing so
///   must not swallow an event. A channel receive is; a receive that pops into
///   a local first is not.
pub trait PageSession {
    /// The driver's error type. It only has to absorb ours.
    type Error: From<Error> + fmt::Display;

    /// Send a CDP command on this page session.
    fn send(
        &self,
        method: &'static str,
        params: Value,
    ) -> impl Future<Output = Result<Value, Self::Error>>;

    /// The next CDP event, or `None` once `timeout` has elapsed without one.
    fn next_event(&self, timeout: Duration) -> impl Future<Output = Option<(String, Value)>>;

    /// Is this error CDP's `-32601`, i.e. a browser launched without
    /// [`PAGE_SETTLE_SWITCH`]?
    ///
    /// The default reads the error's `Display`, which is all a generic client
    /// can do; override it when your driver exposes the code.
    fn method_unavailable(&self, err: &Self::Error) -> bool {
        error_reads_as_unavailable(&err.to_string())
    }

    /// Release the session. The default does nothing, which is right when the
    /// caller owns it beyond the watch; detach here when the watch owns it.
    fn detach(&self) -> impl Future<Output = ()> {
        async {}
    }
}

impl<S: PageSession + ?Sized> PageSession for &S {
    type Error = S::Error;

    fn send(
        &self,
        method: &'static str,
        params: Value,
    ) -> impl Future<Output = Result<Value, Self::Error>> {
        (**self).send(method, params)
    }

    fn next_event(&self, timeout: Duration) -> impl Future<Output = Option<(String, Value)>> {
        (**self).next_event(timeout)
    }

    fn method_unavailable(&self, err: &Self::Error) -> bool {
        (**self).method_unavailable(err)
    }

    fn detach(&self) -> impl Future<Output = ()> {
        (**self).detach()
    }
}

/// A page-completion watch: armed **before** the navigation, waited on after.
///
/// See the [module docs](crate::settle) for why it is two phases and not one
/// call.
#[derive(Debug)]
pub struct SettleWatch<S: PageSession> {
    session: S,
    tracker: LifecycleTracker,
    armed_at: Instant,
    result: Option<SettleState>,
    closed: bool,
}

impl<S: PageSession> SettleWatch<S> {
    /// Arm the watch on a page session, before anything is navigated.
    ///
    /// `Page.enable`, `Page.getFrameTree` for the main frame id,
    /// `Page.setLifecycleEventsEnabled`, then [`REPLAY_WINDOW`] of reading to
    /// write off the incumbent document's replayed lifecycle.
    ///
    /// The session is **detached** if any of that fails, so a failed arm does
    /// not leak the session it was handed.
    pub async fn arm(session: S) -> Result<Self, S::Error> {
        SettleWatch::arm_after(session, REPLAY_WINDOW).await
    }

    /// [`SettleWatch::arm`] with a different replay window.
    ///
    /// Only worth touching on a browser slow enough to replay for longer than
    /// [`REPLAY_WINDOW`]; a window of zero disables the protection and every
    /// fallback signal will read as the `about:blank`'s.
    pub async fn arm_after(session: S, replay_window: Duration) -> Result<Self, S::Error> {
        match SettleWatch::prepare(&session, replay_window).await {
            Ok(tracker) => Ok(SettleWatch {
                session,
                tracker,
                armed_at: Instant::now(),
                result: None,
                closed: false,
            }),
            Err(e) => {
                session.detach().await;
                Err(e)
            }
        }
    }

    async fn prepare(session: &S, replay_window: Duration) -> Result<LifecycleTracker, S::Error> {
        session.send(PAGE_ENABLE_METHOD, json!({})).await?;
        let frame_tree = session.send(GET_FRAME_TREE_METHOD, json!({})).await?;
        let mut tracker = LifecycleTracker::new();
        match main_frame_id(&frame_tree) {
            Some(id) => tracker.set_main_frame_id(id),
            // Refused rather than guessed: a watch that cannot name the main
            // frame counts the first iframe that finishes, which is the 699ms
            // answer to a 6094ms page.
            None => {
                return Err(S::Error::from(Error::MissingMainFrame {
                    detail: format!("{GET_FRAME_TREE_METHOD} named no main frame: {frame_tree}"),
                }))
            }
        }
        session
            .send(
                SET_LIFECYCLE_EVENTS_ENABLED_METHOD,
                json!({"enabled": true}),
            )
            .await?;
        // Everything that arrives now belongs to the document the tab is
        // already on, replayed by the command above.
        let deadline = Instant::now() + replay_window;
        while let Some(left) = remaining(deadline) {
            match session.next_event(left).await {
                Some((method, params)) => tracker.observe(&method, &params),
                None => break,
            }
        }
        tracker.finish_arming();
        Ok(tracker)
    }

    /// The main frame this watch is following.
    pub fn main_frame_id(&self) -> Option<&str> {
        self.tracker.main_frame_id()
    }

    /// The lifecycle bookkeeping, for logging and tests.
    pub fn tracker(&self) -> &LifecycleTracker {
        &self.tracker
    }

    /// When the watch was armed — the client-clock zero for a navigation, since
    /// the navigation can only have started after this.
    pub fn armed_at(&self) -> Instant {
        self.armed_at
    }

    /// The session, still yours.
    pub fn session(&self) -> &S {
        &self.session
    }

    /// Take the session back, closing nothing.
    pub fn into_session(self) -> S {
        self.session
    }

    /// Wait for the page to finish.
    ///
    /// Sends [`WAIT_FOR_SETTLE_METHOD`]; if the browser has no such command
    /// (launched without [`PAGE_SETTLE_SWITCH`]) or answers something
    /// unreadable, falls back to a main-frame, non-stale `networkAlmostIdle`,
    /// then to the `load` event, then to [`SettleVia::Timeout`] — **without
    /// raising**, because on proxied traffic a page that never settles is a
    /// normal outcome and not an error.
    ///
    /// Never returns later than `timeout_ms`. Calling it twice returns the same
    /// answer without sending anything.
    ///
    /// The only `Err` is your driver's: a send that failed for a reason other
    /// than the command being absent.
    pub async fn wait(&mut self, options: WaitOptions) -> Result<SettleState, S::Error> {
        if let Some(done) = &self.result {
            return Ok(done.clone());
        }
        let armed_at = self.armed_at;
        let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));

        // Disjoint borrows: the command reads the session while the drain
        // writes the tracker, and both run until one of them wins.
        let session = &self.session;
        let tracker = &mut self.tracker;

        let mut reason = String::new();
        let mut answer = None;
        if !options.prefer_lifecycle {
            let command = async {
                Answer::Reply(
                    session
                        .send(
                            WAIT_FOR_SETTLE_METHOD,
                            options.command_params(Some(command_timeout_ms(timeout_ms))),
                        )
                        .await,
                )
            };
            // The drain is the clock as well as the evidence: it reads events
            // into the tracker until the deadline, so a command that never
            // answers still ends in a bounded, explainable state.
            let drain = async {
                drain_until(session, tracker, deadline, false).await;
                Answer::Deadline
            };
            match race(command, drain).await {
                Answer::Reply(Ok(reply)) => match SettleState::from_reply(&reply) {
                    Some(state) => answer = Some(state),
                    None if is_method_not_found(&reply) => {
                        reason = unavailable_reason(&reply.to_string());
                    }
                    None => {
                        reason = match error_detail(&reply) {
                            Some(detail) => format!(
                                "{WAIT_FOR_SETTLE_METHOD} was refused ({detail}); \
                                 fell back to the lifecycle feed"
                            ),
                            None => format!(
                                "{WAIT_FOR_SETTLE_METHOD} answered without an outcome ({reply}); \
                                 fell back to the lifecycle feed"
                            ),
                        };
                    }
                },
                Answer::Reply(Err(e)) => {
                    if !session.method_unavailable(&e) {
                        return Err(e);
                    }
                    reason = unavailable_reason(&e.to_string());
                }
                Answer::Deadline => {
                    reason =
                        format!("{WAIT_FOR_SETTLE_METHOD} did not answer within {timeout_ms}ms");
                }
            }
        } else {
            reason = format!("{NETWORK_ALMOST_IDLE} was asked for instead of the command");
        }

        let state = match answer {
            Some(state) => state,
            None => {
                // Stops as soon as there is a signal: the command is out of the
                // picture, so the first main-frame networkAlmostIdle is the
                // answer rather than something better to wait past.
                drain_until(session, tracker, deadline, true).await;
                fallback_state(tracker, armed_at, reason)
            }
        };
        self.result = Some(state.clone());
        Ok(state)
    }

    /// Stop the lifecycle feed and release the session.
    ///
    /// Idempotent, and it swallows errors — a session whose page has already
    /// gone is the normal way to arrive here. Rust cannot do this in `Drop`
    /// (there is nothing to await in), so it is a call: a watch dropped without
    /// it leaves the caller's session exactly as it was, which is safe because
    /// this crate never created it. The blocking twin at least detaches on
    /// drop, since that needs no runtime.
    pub async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self
            .session
            .send(
                SET_LIFECYCLE_EVENTS_ENABLED_METHOD,
                json!({"enabled": false}),
            )
            .await;
        self.session.detach().await;
    }
}

fn unavailable_reason(detail: &str) -> String {
    format!(
        "this browser has no {WAIT_FOR_SETTLE_METHOD} — it was launched without \
         {PAGE_SETTLE_SWITCH} ({detail}); fell back to the lifecycle feed"
    )
}

enum Answer<E> {
    Reply(Result<Value, E>),
    Deadline,
}

/// Read events into the tracker until the deadline.
///
/// `stop_on_idle` ends it as soon as the main frame goes `networkAlmostIdle` —
/// which is the answer. It deliberately does NOT stop on `load`: the load event
/// is the last resort, consulted when the deadline arrives, and stopping there
/// would hand back the 40%-complete signal while the 59% one was one event
/// away.
async fn drain_until<S: PageSession>(
    session: &S,
    tracker: &mut LifecycleTracker,
    deadline: Instant,
    stop_on_idle: bool,
) {
    while let Some(left) = remaining(deadline) {
        if stop_on_idle && tracker.network_almost_idle() {
            return;
        }
        match session.next_event(left).await {
            Some((method, params)) => tracker.observe(&method, &params),
            None => return,
        }
    }
}

/// Whichever finishes first. `futures::select!` without the dependency — one
/// poll each, in order, which is also what makes the command win a tie.
async fn race<T, A, B>(a: A, b: B) -> T
where
    A: Future<Output = T>,
    B: Future<Output = T>,
{
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(move |cx| {
        if let Poll::Ready(value) = a.as_mut().poll(cx) {
            return Poll::Ready(value);
        }
        b.as_mut().poll(cx)
    })
    .await
}

// --- blocking --------------------------------------------------------------

/// [`PageSession`] for a synchronous driver.
///
/// The same contract, `&mut self` because a blocking transport is a socket and
/// nothing is racing it. One consequence worth knowing before you pick this
/// form: a blocking `send` cannot be interrupted, so the client-side deadline
/// only applies to the fallback drain — the command itself is bounded by the
/// `timeoutMs` it is given and by your transport's read timeout. Set one.
pub trait BlockingPageSession {
    /// The driver's error type. It only has to absorb ours.
    type Error: From<Error> + fmt::Display;

    /// Send a CDP command on this page session.
    fn send(&mut self, method: &'static str, params: Value) -> Result<Value, Self::Error>;

    /// The next CDP event, or `None` once `timeout` has elapsed without one —
    /// which also ends the current wait, so do not return it early. A socket
    /// read timeout is the usual implementation.
    fn next_event(&mut self, timeout: Duration) -> Option<(String, Value)>;

    /// Is this error CDP's `-32601`? See [`PageSession::method_unavailable`].
    fn method_unavailable(&self, err: &Self::Error) -> bool {
        error_reads_as_unavailable(&err.to_string())
    }

    /// Release the session. Called on drop as well as by
    /// [`BlockingSettleWatch::close`], so keep it cheap and repeatable.
    fn detach(&mut self) {}
}

/// Blocking twin of [`SettleWatch`], for synchronous drivers.
///
/// Same two phases, same fallbacks, same refusal to raise over a page that did
/// not settle.
#[derive(Debug)]
pub struct BlockingSettleWatch<S: BlockingPageSession> {
    session: S,
    tracker: LifecycleTracker,
    armed_at: Instant,
    result: Option<SettleState>,
    closed: bool,
}

impl<S: BlockingPageSession> BlockingSettleWatch<S> {
    /// Arm the watch on a page session, before anything is navigated.
    pub fn arm(session: S) -> Result<Self, S::Error> {
        BlockingSettleWatch::arm_after(session, REPLAY_WINDOW)
    }

    /// [`BlockingSettleWatch::arm`] with a different replay window.
    pub fn arm_after(mut session: S, replay_window: Duration) -> Result<Self, S::Error> {
        match BlockingSettleWatch::prepare(&mut session, replay_window) {
            Ok(tracker) => Ok(BlockingSettleWatch {
                session,
                tracker,
                armed_at: Instant::now(),
                result: None,
                closed: false,
            }),
            Err(e) => {
                session.detach();
                Err(e)
            }
        }
    }

    fn prepare(session: &mut S, replay_window: Duration) -> Result<LifecycleTracker, S::Error> {
        session.send(PAGE_ENABLE_METHOD, json!({}))?;
        let frame_tree = session.send(GET_FRAME_TREE_METHOD, json!({}))?;
        let mut tracker = LifecycleTracker::new();
        match main_frame_id(&frame_tree) {
            Some(id) => tracker.set_main_frame_id(id),
            None => {
                return Err(S::Error::from(Error::MissingMainFrame {
                    detail: format!("{GET_FRAME_TREE_METHOD} named no main frame: {frame_tree}"),
                }))
            }
        }
        session.send(
            SET_LIFECYCLE_EVENTS_ENABLED_METHOD,
            json!({"enabled": true}),
        )?;
        let deadline = Instant::now() + replay_window;
        while let Some(left) = remaining(deadline) {
            match session.next_event(left) {
                Some((method, params)) => tracker.observe(&method, &params),
                None => break,
            }
        }
        tracker.finish_arming();
        Ok(tracker)
    }

    /// The main frame this watch is following.
    pub fn main_frame_id(&self) -> Option<&str> {
        self.tracker.main_frame_id()
    }

    /// The lifecycle bookkeeping, for logging and tests.
    pub fn tracker(&self) -> &LifecycleTracker {
        &self.tracker
    }

    /// The session, still yours.
    pub fn session(&self) -> &S {
        &self.session
    }

    /// The session, mutably — a blocking driver usually needs it that way.
    pub fn session_mut(&mut self) -> &mut S {
        &mut self.session
    }

    /// When the watch was armed. See [`SettleWatch::armed_at`].
    pub fn armed_at(&self) -> Instant {
        self.armed_at
    }

    /// Wait for the page to finish. See [`SettleWatch::wait`].
    pub fn wait(&mut self, options: WaitOptions) -> Result<SettleState, S::Error> {
        if let Some(done) = &self.result {
            return Ok(done.clone());
        }
        let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));

        let mut reason = String::new();
        let mut answer = None;
        if options.prefer_lifecycle {
            reason = format!("{NETWORK_ALMOST_IDLE} was asked for instead of the command");
        } else {
            match self.session.send(
                WAIT_FOR_SETTLE_METHOD,
                options.command_params(Some(command_timeout_ms(timeout_ms))),
            ) {
                Ok(reply) => match SettleState::from_reply(&reply) {
                    Some(state) => answer = Some(state),
                    None if is_method_not_found(&reply) => {
                        reason = unavailable_reason(&reply.to_string());
                    }
                    None => {
                        reason = match error_detail(&reply) {
                            Some(detail) => format!(
                                "{WAIT_FOR_SETTLE_METHOD} was refused ({detail}); \
                                 fell back to the lifecycle feed"
                            ),
                            None => format!(
                                "{WAIT_FOR_SETTLE_METHOD} answered without an outcome ({reply}); \
                                 fell back to the lifecycle feed"
                            ),
                        };
                    }
                },
                Err(e) => {
                    if !self.session.method_unavailable(&e) {
                        return Err(e);
                    }
                    reason = unavailable_reason(&e.to_string());
                }
            }
        }

        let state = match answer {
            Some(state) => state,
            None => {
                while let Some(left) = remaining(deadline) {
                    // Only networkAlmostIdle ends the wait early; `load` is the
                    // last resort, read off at the deadline.
                    if self.tracker.network_almost_idle() {
                        break;
                    }
                    match self.session.next_event(left) {
                        Some((method, params)) => self.tracker.observe(&method, &params),
                        None => break,
                    }
                }
                fallback_state(&self.tracker, self.armed_at, reason)
            }
        };
        self.result = Some(state.clone());
        Ok(state)
    }

    /// Stop the lifecycle feed and release the session.
    ///
    /// Idempotent. Dropping the watch instead still calls
    /// [`BlockingPageSession::detach`], but sends nothing — a blocking CDP call
    /// in a destructor is not something to do behind the caller's back — so
    /// call this when you want the feed turned off too.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.session.send(
            SET_LIFECYCLE_EVENTS_ENABLED_METHOD,
            json!({"enabled": false}),
        );
        self.session.detach();
    }
}

impl<S: BlockingPageSession> Drop for BlockingSettleWatch<S> {
    /// Releases the session on every exit path, including the ones you did not
    /// write — an early `return`, a `?`, a panic.
    fn drop(&mut self) {
        if !self.closed {
            self.closed = true;
            self.session.detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Page.lifecycleEvent` as the browser spells it.
    fn lifecycle(frame: &str, loader: &str, name: &str, timestamp: f64) -> Value {
        json!({"frameId": frame, "loaderId": loader, "name": name, "timestamp": timestamp})
    }

    fn armed(main_frame: &str) -> LifecycleTracker {
        let mut tracker = LifecycleTracker::new();
        tracker.set_main_frame_id(main_frame);
        tracker.finish_arming();
        tracker
    }

    #[test]
    fn launch_args_stay_minimal_without_tuning() {
        assert_eq!(settle_launch_args(SettleTuning::new()), ["--page-settle"]);
        assert_eq!(
            settle_launch_args(
                SettleTuning::new()
                    .quiet_window_ms(1500)
                    .min_chars(64)
                    .timeout_ms(20_000)
                    .sample_interval_ms(100)
                    .pierce_shadow(true)
            ),
            [
                "--page-settle",
                "--page-settle-quiet-window-ms=1500",
                "--page-settle-min-chars=64",
                "--page-settle-timeout-ms=20000",
                "--page-settle-sample-interval-ms=100",
                "--page-settle-pierce-shadow",
            ]
        );
    }

    #[test]
    fn a_reply_is_read_back_field_for_field() {
        let reply = json!({
            "outcome": "settled",
            "elapsedMs": 7306.0,
            "textLength": 48213,
            "navigations": 2,
            "httpStatus": 200,
            "reason": "quiet",
            "somethingNewerBinariesSend": true,
        });
        let state = SettleState::from_reply(&reply).expect("a settled reply");
        assert_eq!(state.outcome, SettleOutcome::Settled);
        assert_eq!(state.elapsed_ms, 7306.0);
        assert_eq!(state.text_length, Some(48213));
        assert_eq!(state.navigations, 2);
        assert_eq!(state.http_status, Some(200));
        assert_eq!(state.reason, "quiet");
        assert_eq!(state.via, SettleVia::WaitForSettle);
        assert!(state.is_settled());
        assert!(!state.blocked());

        // A raw WebSocket hands over the whole envelope; same answer.
        assert_eq!(
            SettleState::from_reply(&json!({"id": 7, "result": reply})),
            Some(state)
        );
    }

    #[test]
    fn an_outcome_the_client_predates_is_kept_not_dropped() {
        let state = SettleState::from_reply(&json!({"outcome": "quiesced"})).expect("parsed");
        assert_eq!(state.outcome, SettleOutcome::Other("quiesced".into()));
        assert_eq!(state.outcome.as_str(), "quiesced");
        // Missing members read as absent rather than as zero.
        assert_eq!(state.text_length, None);
        assert_eq!(state.http_status, None);
        assert_eq!(state.navigations, 0);
    }

    #[test]
    fn a_reply_with_no_outcome_is_not_a_state() {
        assert!(SettleState::from_reply(&json!({})).is_none());
        assert!(SettleState::from_reply(&json!({"error": {"code": -32601}})).is_none());
    }

    #[test]
    fn a_wall_is_blocked_whichever_way_it_arrives() {
        // The tall wall: the browser calls it what it is.
        let challenge =
            SettleState::from_reply(&json!({"outcome": "challenge", "httpStatus": 200})).unwrap();
        assert!(challenge.blocked());
        assert!(!challenge.is_settled(), "a wall is never a successful load");

        // The short one: under --page-settle-min-chars it reports a TIMEOUT, and
        // only the status gives it away. A caller that switched on the outcome
        // alone would treat this as a slow page and retry into the same wall.
        for status in BLOCKED_STATUSES {
            let short = SettleState::from_reply(
                &json!({"outcome": "timeout", "httpStatus": status, "elapsedMs": 30000.0}),
            )
            .unwrap();
            assert!(short.blocked(), "status {status} read as clean");
        }

        let clean =
            SettleState::from_reply(&json!({"outcome": "settled", "httpStatus": 200})).unwrap();
        assert!(!clean.blocked());
        // A timeout with no status is a page that did not settle, not a wall.
        let slow = SettleState::from_reply(&json!({"outcome": "timeout"})).unwrap();
        assert!(!slow.blocked());
    }

    #[test]
    fn method_not_found_is_recognised_by_code_and_by_message() {
        assert!(is_method_not_found(
            &json!({"error": {"code": -32601, "message": "'Chromeleon.waitForSettle' wasn't found"}})
        ));
        assert!(is_method_not_found(
            &json!({"error": {"message": "'Chromeleon.waitForSettle' wasn't found"}})
        ));
        assert!(!is_method_not_found(&json!({"outcome": "settled"})));
        assert!(!is_method_not_found(
            &json!({"error": {"code": -32602, "message": "Invalid parameters"}})
        ));

        // The driver-error heuristic behind PageSession::method_unavailable.
        assert!(error_reads_as_unavailable(
            "CDP error -32601: not implemented"
        ));
        assert!(error_reads_as_unavailable(
            "'Chromeleon.waitForSettle' wasn't found"
        ));
        assert!(error_reads_as_unavailable("Unknown method"));
        assert!(!error_reads_as_unavailable("websocket closed"));
    }

    #[test]
    fn the_main_frame_id_survives_either_reply_shape() {
        let tree = json!({"frameTree": {"frame": {"id": "MAIN"}, "childFrames": [
            {"frameTree": {"frame": {"id": "AD"}}}
        ]}});
        assert_eq!(main_frame_id(&tree), Some("MAIN"));
        assert_eq!(main_frame_id(&json!({"result": tree})), Some("MAIN"));
        assert_eq!(main_frame_id(&json!({"result": {}})), None);
    }

    #[test]
    fn the_about_blank_replay_is_written_off() {
        // setLifecycleEventsEnabled replays a COMPLETE lifecycle for the page
        // the tab is already on. Counting it makes every measurement ~0ms.
        let mut tracker = LifecycleTracker::new();
        tracker.set_main_frame_id("MAIN");
        for name in ["init", "load", "networkAlmostIdle", "networkIdle"] {
            tracker.observe(LIFECYCLE_EVENT, &lifecycle("MAIN", "BLANK", name, 100.0));
        }
        tracker.finish_arming();
        assert!(!tracker.network_almost_idle(), "the replay was counted");
        assert_eq!(tracker.stale_loader_count(), 1);

        // …and the same loader still does not count afterwards.
        tracker.observe(
            LIFECYCLE_EVENT,
            &lifecycle("MAIN", "BLANK", NETWORK_ALMOST_IDLE, 100.5),
        );
        assert_eq!(tracker.signal(), None);
        assert_eq!(tracker.navigations(), 0);

        // The real navigation, with its own loader, does.
        tracker.observe(LIFECYCLE_EVENT, &lifecycle("MAIN", "NAV-1", "init", 101.0));
        tracker.observe(
            LIFECYCLE_EVENT,
            &lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 107.094),
        );
        assert_eq!(
            tracker.signal(),
            Some((SettleVia::NetworkAlmostIdle, Some(107.094)))
        );
        assert_eq!(tracker.navigations(), 1);
    }

    #[test]
    fn subframes_do_not_answer_for_the_main_frame() {
        // bbc.com/news emits lifecycle from 23 frames. First-across-all reports
        // networkIdle at 699ms; the main frame's real value is 6094ms.
        let mut tracker = armed("MAIN");
        tracker.observe(LIFECYCLE_EVENT, &lifecycle("MAIN", "NAV-1", "init", 0.0));
        for frame in 1..23 {
            tracker.observe(
                LIFECYCLE_EVENT,
                &lifecycle(&format!("SUB-{frame}"), "NAV-1", NETWORK_ALMOST_IDLE, 0.699),
            );
            tracker.observe(
                LIFECYCLE_EVENT,
                &lifecycle(&format!("SUB-{frame}"), "NAV-1", LOAD_LIFECYCLE, 0.699),
            );
        }
        assert_eq!(tracker.signal(), None, "an iframe answered for the page");

        tracker.observe(
            LIFECYCLE_EVENT,
            &lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 6.094),
        );
        let (via, at) = tracker.signal().expect("the main frame's own signal");
        assert_eq!(via, SettleVia::NetworkAlmostIdle);
        // Measured on the BROWSER's clock, from this navigation's first event.
        let elapsed = tracker.elapsed_ms(at).expect("both timestamps known");
        assert!((elapsed - 6094.0).abs() < 0.01, "{elapsed}");
    }

    #[test]
    fn a_tracker_that_cannot_name_the_main_frame_counts_nothing() {
        let mut tracker = LifecycleTracker::new();
        tracker.finish_arming();
        tracker.observe(
            LIFECYCLE_EVENT,
            &lifecycle("ANY", "NAV-1", NETWORK_ALMOST_IDLE, 1.0),
        );
        assert_eq!(tracker.signal(), None);
    }

    #[test]
    fn network_almost_idle_beats_load_and_a_redirect_counts_twice() {
        let mut tracker = armed("MAIN");
        tracker.observe(LIFECYCLE_EVENT, &lifecycle("MAIN", "NAV-1", "init", 0.0));
        tracker.observe(
            LIFECYCLE_EVENT,
            &lifecycle("MAIN", "NAV-1", LOAD_LIFECYCLE, 1.0),
        );
        assert_eq!(tracker.signal(), Some((SettleVia::Load, Some(1.0))));
        assert!(tracker.loaded());

        tracker.observe(LIFECYCLE_EVENT, &lifecycle("MAIN", "NAV-2", "init", 1.2));
        tracker.observe(
            LIFECYCLE_EVENT,
            &lifecycle("MAIN", "NAV-2", NETWORK_ALMOST_IDLE, 3.989),
        );
        assert_eq!(
            tracker.signal(),
            Some((SettleVia::NetworkAlmostIdle, Some(3.989)))
        );
        assert_eq!(tracker.navigations(), 2, "a redirect chain is two loaders");
        // Still measured from the FIRST event of the chain.
        assert_eq!(tracker.elapsed_ms(Some(3.989)), Some(3989.0));
    }

    #[test]
    fn anything_that_is_not_a_lifecycle_event_is_ignored() {
        let mut tracker = armed("MAIN");
        tracker.observe(
            "Chromeleon.captchaSolved",
            &json!({"frameId": "MAIN", "loaderId": "NAV-1"}),
        );
        tracker.observe(
            "Page.frameNavigated",
            &lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 1.0),
        );
        assert_eq!(tracker.signal(), None);
    }

    #[test]
    fn the_browser_is_asked_to_give_up_before_we_do() {
        // …so that its answer — which carries the outcome and the status — wins
        // the race against a bare client-side timeout.
        assert_eq!(command_timeout_ms(30_000), 29_750);
        let params = WaitOptions::new()
            .timeout_ms(30_000)
            .command_params(Some(command_timeout_ms(30_000)));
        assert_eq!(params["timeoutMs"], 29_750);
        assert!(params.get("quietWindowMs").is_none());
        assert!(params.get("minChars").is_none());

        // A timeout too short to split is passed through rather than floored to
        // something meaningless.
        assert_eq!(command_timeout_ms(400), 400);

        let tuned = WaitOptions::new()
            .quiet_window_ms(1500)
            .min_chars(64)
            .command_params(Some(command_timeout_ms(10_000)));
        assert_eq!(
            tuned,
            json!({"timeoutMs": 9_750, "quietWindowMs": 1500, "minChars": 64})
        );
    }
}
