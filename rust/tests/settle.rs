//! What a settle watch has to guarantee, against a scripted CDP page session.
//!
//! No browser: the fake below answers commands and hands over lifecycle events
//! the way a driver would, which is enough to pin every rule that used to be a
//! silent wrong answer — a bot wall reported as a load, an `about:blank` replay
//! measured as a 0ms page, an iframe answering for the document, and a browser
//! launched without `--page-settle` turning a navigation into an exception.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::poll_fn;
use std::task::Poll;
use std::time::{Duration, Instant};

use chromeleon::settle::{
    BlockingPageSession, BlockingSettleWatch, PageSession, SettleOutcome, SettleState, SettleVia,
    SettleWatch, WaitOptions, GET_FRAME_TREE_METHOD, LIFECYCLE_EVENT, NETWORK_ALMOST_IDLE,
    PAGE_ENABLE_METHOD, SET_LIFECYCLE_EVENTS_ENABLED_METHOD, WAIT_FOR_SETTLE_METHOD,
};
use chromeleon::Error;

use futures_lite::future::block_on;
use serde_json::{json, Value};

/// A short replay window: the fake has no events to replay unless a test queued
/// some, and 350ms of real waiting per test is 350ms of nothing.
const FAST_ARM: Duration = Duration::from_millis(10);

/// A driver's error type, shaped like the real ones: its own failures plus ours.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FakeError {
    Chromeleon(Error),
    Driver(String),
}

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FakeError::Chromeleon(e) => write!(f, "{e}"),
            FakeError::Driver(text) => f.write_str(text),
        }
    }
}

impl From<Error> for FakeError {
    fn from(e: Error) -> Self {
        FakeError::Chromeleon(e)
    }
}

/// What the session should do with one command.
#[derive(Debug, Clone)]
enum Scripted {
    Reply(Value),
    Fail(&'static str),
    /// Never answers — the case the client-side deadline exists for.
    Hang,
}

#[derive(Default)]
struct Inner {
    scripted: HashMap<&'static str, Scripted>,
    sent: Vec<(String, Value)>,
    events: VecDeque<(String, Value)>,
    detached: usize,
}

impl Inner {
    fn reply_to(
        &mut self,
        method: &'static str,
        params: Value,
    ) -> Option<Result<Value, FakeError>> {
        self.sent.push((method.to_string(), params));
        match self.scripted.get(method).cloned() {
            Some(Scripted::Reply(value)) => Some(Ok(value)),
            Some(Scripted::Fail(text)) => Some(Err(FakeError::Driver(text.to_string()))),
            Some(Scripted::Hang) => None,
            // The arm sequence's own commands, answered the way Chrome does.
            None if method == GET_FRAME_TREE_METHOD => {
                Some(Ok(json!({"frameTree": {"frame": {"id": "MAIN"}}})))
            }
            None => Some(Ok(json!({}))),
        }
    }

    /// The contract: hand over the next event, or wait out `timeout` and say so.
    fn take_event(&mut self, timeout: Duration) -> Option<(String, Value)> {
        match self.events.pop_front() {
            Some(event) => Some(event),
            None => {
                std::thread::sleep(timeout);
                None
            }
        }
    }

    fn count_sent(&self, method: &str) -> usize {
        self.sent.iter().filter(|(m, _)| m == method).count()
    }

    fn params_of(&self, method: &str) -> Option<&Value> {
        self.sent
            .iter()
            .find(|(m, _)| m == method)
            .map(|(_, params)| params)
    }
}

/// One page CDP session, scripted.
#[derive(Default)]
struct Session {
    inner: RefCell<Inner>,
}

impl Session {
    fn new() -> Self {
        Session::default()
    }

    fn answering(self, method: &'static str, reply: Value) -> Self {
        self.inner
            .borrow_mut()
            .scripted
            .insert(method, Scripted::Reply(reply));
        self
    }

    fn failing(self, method: &'static str, text: &'static str) -> Self {
        self.inner
            .borrow_mut()
            .scripted
            .insert(method, Scripted::Fail(text));
        self
    }

    fn hanging(self, method: &'static str) -> Self {
        self.inner
            .borrow_mut()
            .scripted
            .insert(method, Scripted::Hang);
        self
    }

    /// Queue events for the watch to read. Queued before arming, they are the
    /// incumbent document's replay; queued after, they are the navigation's.
    fn queue(&self, events: impl IntoIterator<Item = Value>) {
        let mut inner = self.inner.borrow_mut();
        for params in events {
            inner
                .events
                .push_back((LIFECYCLE_EVENT.to_string(), params));
        }
    }
}

impl PageSession for Session {
    type Error = FakeError;

    async fn send(&self, method: &'static str, params: Value) -> Result<Value, Self::Error> {
        // Bound first: a `borrow_mut()` in the match scrutinee lives to the end
        // of the match, and would still be held across the await below.
        let scripted = self.inner.borrow_mut().reply_to(method, params);
        match scripted {
            Some(answer) => answer,
            // Pending forever, without ever polling ready.
            None => poll_fn(|_| Poll::<Result<Value, Self::Error>>::Pending).await,
        }
    }

    async fn next_event(&self, timeout: Duration) -> Option<(String, Value)> {
        self.inner.borrow_mut().take_event(timeout)
    }

    async fn detach(&self) {
        self.inner.borrow_mut().detached += 1;
    }
}

/// A `Page.lifecycleEvent`, as the browser spells it.
fn lifecycle(frame: &str, loader: &str, name: &str, timestamp: f64) -> Value {
    json!({"frameId": frame, "loaderId": loader, "name": name, "timestamp": timestamp})
}

fn armed(session: Session) -> SettleWatch<Session> {
    block_on(SettleWatch::arm_after(session, FAST_ARM)).expect("arm")
}

fn settled_reply() -> Value {
    json!({
        "outcome": "settled",
        "elapsedMs": 7306.0,
        "textLength": 48213,
        "navigations": 1,
        "httpStatus": 200,
        "reason": "quiet",
    })
}

// --- the primary -----------------------------------------------------------

#[test]
fn arming_happens_before_the_navigation_and_in_the_order_the_browser_needs() {
    // The challenge header is recorded at COMMIT time by a tracker attached
    // when the handler is constructed, so all of this has to be in place before
    // anything is navigated — and the lifecycle feed has to be turned on after
    // the frame tree is read, or the replay cannot be told from the real thing.
    let watch = armed(Session::new());
    let session = watch.session();
    let sent: Vec<String> = session
        .inner
        .borrow()
        .sent
        .iter()
        .map(|(m, _)| m.clone())
        .collect();
    assert_eq!(
        sent,
        [
            PAGE_ENABLE_METHOD,
            GET_FRAME_TREE_METHOD,
            SET_LIFECYCLE_EVENTS_ENABLED_METHOD
        ]
    );
    assert_eq!(
        session
            .inner
            .borrow()
            .params_of(SET_LIFECYCLE_EVENTS_ENABLED_METHOD),
        Some(&json!({"enabled": true}))
    );
    assert_eq!(watch.main_frame_id(), Some("MAIN"));
}

#[test]
fn a_settled_reply_is_the_answer_verbatim() {
    let mut watch = armed(Session::new().answering(WAIT_FOR_SETTLE_METHOD, settled_reply()));
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(30_000))).expect("wait");

    assert_eq!(state.outcome, SettleOutcome::Settled);
    assert_eq!(state.elapsed_ms, 7306.0);
    assert_eq!(state.text_length, Some(48213));
    assert_eq!(state.navigations, 1);
    assert_eq!(state.http_status, Some(200));
    assert_eq!(state.reason, "quiet");
    assert_eq!(state.via, SettleVia::WaitForSettle);
    assert!(state.is_settled() && !state.blocked());

    // The browser is asked to give up just before we do, so its answer wins.
    let sent = watch.session().inner.borrow();
    assert_eq!(
        sent.params_of(WAIT_FOR_SETTLE_METHOD),
        Some(&json!({"timeoutMs": 29_750}))
    );
}

#[test]
fn a_challenge_is_surfaced_as_a_wall_and_never_as_a_load() {
    // 12.5% of proxied navigations in production measurement.
    let mut watch = armed(Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"outcome": "challenge", "elapsedMs": 812.0, "textLength": 1893,
               "httpStatus": 200, "reason": "datadome"}),
    ));
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("wait");
    assert_eq!(state.outcome, SettleOutcome::Challenge);
    assert!(state.blocked(), "a challenge is a wall");
    assert!(!state.is_settled(), "and never a successful load");
    assert_eq!(state.via, SettleVia::WaitForSettle);
}

#[test]
fn a_wall_shorter_than_min_chars_reports_timeout_and_is_still_blocked() {
    // The trap: under --page-settle-min-chars a short wall never settles, so the
    // OUTCOME reads as an ordinary slow page. Only the status gives it away,
    // which is why the classification is outcome OR status.
    let mut watch = armed(Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"outcome": "timeout", "elapsedMs": 30000.0, "textLength": 42,
               "httpStatus": 403, "reason": "min_chars_not_reached"}),
    ));
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("wait");
    assert_eq!(state.outcome, SettleOutcome::Timeout);
    assert_eq!(state.http_status, Some(403));
    assert!(state.blocked(), "403 behind a timeout is still a wall");
}

#[test]
fn the_same_answer_comes_back_twice_without_a_second_command() {
    let mut watch = armed(Session::new().answering(WAIT_FOR_SETTLE_METHOD, settled_reply()));
    let first = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("first");
    let second = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("second");
    assert_eq!(first, second);
    assert_eq!(
        watch
            .session()
            .inner
            .borrow()
            .count_sent(WAIT_FOR_SETTLE_METHOD),
        1,
        "wait() is idempotent, not a second navigation's worth of commands"
    );
}

// --- the fallback ----------------------------------------------------------

#[test]
fn a_browser_without_the_switch_falls_back_instead_of_raising() {
    // Launched without --page-settle: the domain is not registered at all, so
    // this is -32601 (method not found), not "settling is off".
    let mut watch = armed(Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"error": {"code": -32601, "message": "'Chromeleon.waitForSettle' wasn't found"}}),
    ));
    // Queued AFTER arming: this is the navigation, not the replay.
    watch.session().queue([
        lifecycle("MAIN", "NAV-1", "init", 100.0),
        lifecycle("MAIN", "NAV-1", "load", 103.0),
        lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 103.989),
    ]);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("no raise");

    assert_eq!(state.via, SettleVia::NetworkAlmostIdle);
    assert_eq!(state.outcome, SettleOutcome::Settled);
    // Measured on the BROWSER's clock: 100.0s → 103.989s.
    assert!(
        (state.elapsed_ms - 3989.0).abs() < 0.01,
        "{}",
        state.elapsed_ms
    );
    // Nothing sampled the text or the status on this path, and saying so is the
    // point — 0 would read as an empty page.
    assert_eq!(state.text_length, None);
    assert_eq!(state.http_status, None);
    assert_eq!(state.navigations, 1);
    assert!(
        state.reason.contains("--page-settle"),
        "the path taken has to be visible: {}",
        state.reason
    );
}

#[test]
fn a_driver_that_raises_minus_32601_falls_back_the_same_way() {
    // A typed driver turns the CDP error into its own before we see it.
    let mut watch = armed(Session::new().failing(
        WAIT_FOR_SETTLE_METHOD,
        "CDP error -32601: 'Chromeleon.waitForSettle' wasn't found",
    ));
    watch.session().queue([
        lifecycle("MAIN", "NAV-1", "init", 10.0),
        lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 14.0),
    ]);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("no raise");
    assert_eq!(state.via, SettleVia::NetworkAlmostIdle);
    assert_eq!(state.elapsed_ms, 4000.0);
}

#[test]
fn a_driver_failure_that_is_not_the_missing_command_is_raised() {
    // Falling back over a dead socket would turn a broken session into a page
    // that merely "did not settle".
    let session = Session::new().failing(WAIT_FOR_SETTLE_METHOD, "websocket closed");
    let mut watch = armed(session);
    let failed = block_on(watch.wait(WaitOptions::new().timeout_ms(500)));
    assert_eq!(failed, Err(FakeError::Driver("websocket closed".into())));
}

#[test]
fn the_load_event_is_the_last_resort_and_says_so() {
    let mut watch = armed(Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"error": {"code": -32601, "message": "wasn't found"}}),
    ));
    watch.session().queue([
        lifecycle("MAIN", "NAV-1", "init", 1.0),
        lifecycle("MAIN", "NAV-1", "load", 2.5),
    ]);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(300))).expect("no raise");
    // networkAlmostIdle never came — 36% of proxied loads — so the load event is
    // the best evidence there is, and the caller can see that it was.
    assert_eq!(state.via, SettleVia::Load);
    assert_eq!(state.outcome, SettleOutcome::Settled);
    assert_eq!(state.elapsed_ms, 1500.0);
}

#[test]
fn the_about_blank_replay_is_never_the_answer() {
    // setLifecycleEventsEnabled REPLAYS a complete lifecycle for the page the
    // tab is already on. Kept, every fallback signal reads as ~0ms.
    let session = Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"error": {"code": -32601, "message": "wasn't found"}}),
    );
    session.queue([
        lifecycle("MAIN", "BLANK", "init", 0.0),
        lifecycle("MAIN", "BLANK", "load", 0.001),
        lifecycle("MAIN", "BLANK", NETWORK_ALMOST_IDLE, 0.002),
        lifecycle("MAIN", "BLANK", "networkIdle", 0.003),
    ]);
    // The real replay window, because that is the thing under test.
    let mut watch = block_on(SettleWatch::arm(session)).expect("arm");
    assert_eq!(watch.tracker().stale_loader_count(), 1);

    watch.session().queue([
        lifecycle("MAIN", "NAV-1", "init", 5.0),
        // The incumbent document is still emitting; it is still not ours.
        lifecycle("MAIN", "BLANK", NETWORK_ALMOST_IDLE, 5.1),
        lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 12.108),
    ]);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("wait");
    assert_eq!(state.via, SettleVia::NetworkAlmostIdle);
    assert!(
        (state.elapsed_ms - 7108.0).abs() < 0.01,
        "the replay was measured instead of the navigation: {}",
        state.elapsed_ms
    );
    assert_eq!(state.navigations, 1);
}

#[test]
fn a_subframe_does_not_answer_for_the_page() {
    // bbc.com/news emits lifecycle from 23 frames: first-across-all reports
    // networkIdle at 699ms when the main frame's real value is 6094ms.
    let mut watch = armed(Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"error": {"code": -32601, "message": "wasn't found"}}),
    ));
    let mut queued = vec![lifecycle("MAIN", "NAV-1", "init", 0.0)];
    for frame in 1..23 {
        queued.push(lifecycle(
            &format!("SUB-{frame}"),
            "NAV-1",
            NETWORK_ALMOST_IDLE,
            0.699,
        ));
    }
    queued.push(lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 6.094));
    watch.session().queue(queued);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000))).expect("wait");
    assert_eq!(state.via, SettleVia::NetworkAlmostIdle);
    assert!(
        (state.elapsed_ms - 6094.0).abs() < 0.01,
        "an iframe answered for the document: {}",
        state.elapsed_ms
    );
}

#[test]
fn the_lifecycle_path_can_be_asked_for_directly() {
    // The documented choice for UN-proxied work: 1.8x faster to the same median
    // content, and it misses 1% of direct loads against 36% of proxied ones.
    let mut watch = armed(Session::new().answering(WAIT_FOR_SETTLE_METHOD, settled_reply()));
    watch.session().queue([
        lifecycle("MAIN", "NAV-1", "init", 0.0),
        lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 3.989),
    ]);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(5_000).prefer_lifecycle(true)))
        .expect("wait");
    assert_eq!(state.via, SettleVia::NetworkAlmostIdle);
    assert_eq!(state.elapsed_ms, 3989.0);
    assert_eq!(
        watch
            .session()
            .inner
            .borrow()
            .count_sent(WAIT_FOR_SETTLE_METHOD),
        0,
        "the command was sent anyway"
    );
}

// --- the bounds ------------------------------------------------------------

#[test]
fn a_command_that_never_answers_still_ends_at_the_deadline() {
    // A page that does not settle is NORMAL on proxied traffic: bounded, and
    // never an exception.
    let session = Session::new().hanging(WAIT_FOR_SETTLE_METHOD);
    let mut watch = armed(session);
    let started = Instant::now();
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(250))).expect("no raise");
    let waited = started.elapsed();

    assert_eq!(state.outcome, SettleOutcome::Timeout);
    assert_eq!(state.via, SettleVia::Timeout);
    assert!(
        waited >= Duration::from_millis(200),
        "returned early: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "hung past its timeout: {waited:?}"
    );
    assert!(state.reason.contains("250ms"), "{}", state.reason);
}

#[test]
fn a_page_that_never_settles_is_a_state_not_an_error() {
    let session = Session::new().answering(
        WAIT_FOR_SETTLE_METHOD,
        json!({"error": {"code": -32601, "message": "wasn't found"}}),
    );
    let mut watch = armed(session);
    let state = block_on(watch.wait(WaitOptions::new().timeout_ms(120))).expect("no raise");
    assert_eq!(state.outcome, SettleOutcome::Timeout);
    assert_eq!(state.via, SettleVia::Timeout);
    assert!(!state.blocked());
    assert_eq!(state.navigations, 0);
}

// --- the session's lifetime ------------------------------------------------

#[test]
fn a_session_that_cannot_name_its_main_frame_is_refused_and_released() {
    // Guessing here is the 699ms-instead-of-6094ms bug, and it is silent.
    let session = Session::new().answering(GET_FRAME_TREE_METHOD, json!({"frameTree": {}}));
    let refused = block_on(SettleWatch::arm_after(session, FAST_ARM))
        .err()
        .expect("a session with no main frame must be refused, not guessed at");
    match refused {
        FakeError::Chromeleon(Error::MissingMainFrame { detail }) => {
            assert!(detail.contains(GET_FRAME_TREE_METHOD), "{detail}");
        }
        other => panic!("expected MissingMainFrame, got {other:?}"),
    }
}

#[test]
fn a_failed_arm_detaches_the_session_it_was_handed() {
    // Ownership moved into `arm`; if it does not release the session on the way
    // out, nobody can.
    struct Counted(std::rc::Rc<std::cell::Cell<usize>>);
    impl PageSession for Counted {
        type Error = FakeError;
        async fn send(&self, method: &'static str, _p: Value) -> Result<Value, Self::Error> {
            if method == GET_FRAME_TREE_METHOD {
                return Ok(json!({}));
            }
            Ok(json!({}))
        }
        async fn next_event(&self, _t: Duration) -> Option<(String, Value)> {
            None
        }
        async fn detach(&self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let detached = std::rc::Rc::new(std::cell::Cell::new(0));
    let failed = block_on(SettleWatch::arm_after(
        Counted(std::rc::Rc::clone(&detached)),
        FAST_ARM,
    ));
    assert!(failed.is_err());
    assert_eq!(detached.get(), 1, "the session was leaked");
}

#[test]
fn close_turns_the_feed_off_detaches_and_can_be_called_twice() {
    let mut watch = armed(Session::new().answering(WAIT_FOR_SETTLE_METHOD, settled_reply()));
    block_on(watch.wait(WaitOptions::new().timeout_ms(500))).expect("wait");
    block_on(watch.close());
    block_on(watch.close());

    let session = watch.into_session();
    let inner = session.inner.borrow();
    assert_eq!(inner.detached, 1, "close is not idempotent");
    assert_eq!(
        inner.count_sent(SET_LIFECYCLE_EVENTS_ENABLED_METHOD),
        2,
        "the feed this watch turned on was not turned off"
    );
    assert_eq!(
        inner.sent.last().map(|(_, params)| params.clone()),
        Some(json!({"enabled": false}))
    );
}

// --- the blocking twin -----------------------------------------------------

/// The same scripted session for a synchronous driver.
#[derive(Default)]
struct SyncSession {
    inner: Inner,
}

impl SyncSession {
    fn answering(mut self, method: &'static str, reply: Value) -> Self {
        self.inner.scripted.insert(method, Scripted::Reply(reply));
        self
    }

    fn queue(&mut self, events: impl IntoIterator<Item = Value>) {
        for params in events {
            self.inner
                .events
                .push_back((LIFECYCLE_EVENT.to_string(), params));
        }
    }
}

impl BlockingPageSession for SyncSession {
    type Error = FakeError;

    fn send(&mut self, method: &'static str, params: Value) -> Result<Value, Self::Error> {
        // A blocking transport cannot hang forever here without a read timeout,
        // which is the caveat on the trait; the fake always answers.
        self.inner.reply_to(method, params).unwrap_or(Ok(json!({})))
    }

    fn next_event(&mut self, timeout: Duration) -> Option<(String, Value)> {
        self.inner.take_event(timeout)
    }

    fn detach(&mut self) {
        self.inner.detached += 1;
    }
}

#[test]
fn the_blocking_twin_gives_the_same_answers() {
    let mut watch = BlockingSettleWatch::arm_after(
        SyncSession::default().answering(WAIT_FOR_SETTLE_METHOD, settled_reply()),
        FAST_ARM,
    )
    .expect("arm");
    let state = watch
        .wait(WaitOptions::new().timeout_ms(5_000))
        .expect("wait");
    assert_eq!(state, SettleState::from_reply(&settled_reply()).unwrap());
    assert_eq!(state.via, SettleVia::WaitForSettle);

    // …and the fallback, on a browser launched without the switch.
    let mut fell_back = BlockingSettleWatch::arm_after(
        SyncSession::default().answering(
            WAIT_FOR_SETTLE_METHOD,
            json!({"error": {"code": -32601, "message": "wasn't found"}}),
        ),
        FAST_ARM,
    )
    .expect("arm");
    fell_back.session_mut().queue([
        lifecycle("MAIN", "NAV-1", "init", 2.0),
        lifecycle("MAIN", "NAV-1", NETWORK_ALMOST_IDLE, 13.285),
    ]);
    let state = fell_back
        .wait(WaitOptions::new().timeout_ms(2_000))
        .expect("no raise");
    assert_eq!(state.via, SettleVia::NetworkAlmostIdle);
    assert!(
        (state.elapsed_ms - 11285.0).abs() < 0.01,
        "{}",
        state.elapsed_ms
    );
}

#[test]
fn the_blocking_twin_detaches_exactly_once() {
    struct Counted {
        detached: std::rc::Rc<std::cell::Cell<usize>>,
        disabled: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl BlockingPageSession for Counted {
        type Error = FakeError;
        fn send(&mut self, method: &'static str, params: Value) -> Result<Value, Self::Error> {
            if method == GET_FRAME_TREE_METHOD {
                return Ok(json!({"frameTree": {"frame": {"id": "MAIN"}}}));
            }
            if method == SET_LIFECYCLE_EVENTS_ENABLED_METHOD && params == json!({"enabled": false})
            {
                self.disabled.set(self.disabled.get() + 1);
            }
            if method == WAIT_FOR_SETTLE_METHOD {
                return Ok(json!({"outcome": "settled", "elapsedMs": 1.0}));
            }
            Ok(json!({}))
        }
        fn next_event(&mut self, _timeout: Duration) -> Option<(String, Value)> {
            None
        }
        fn detach(&mut self) {
            self.detached.set(self.detached.get() + 1);
        }
    }

    let detached = std::rc::Rc::new(std::cell::Cell::new(0));
    let disabled = std::rc::Rc::new(std::cell::Cell::new(0));
    {
        let mut watch = BlockingSettleWatch::arm_after(
            Counted {
                detached: std::rc::Rc::clone(&detached),
                disabled: std::rc::Rc::clone(&disabled),
            },
            Duration::ZERO,
        )
        .expect("arm");
        watch
            .wait(WaitOptions::new().timeout_ms(500))
            .expect("wait");
        watch.close();
        assert_eq!(disabled.get(), 1);
    } // dropped here
    assert_eq!(detached.get(), 1, "close then drop detached twice");
}
