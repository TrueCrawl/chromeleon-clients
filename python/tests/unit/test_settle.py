"""Contracts for chromeleon.settle — page completion over the rendered text.

The behaviours under test are the ones that fail SILENTLY when hand-rolled:

  * a bot wall reported as a successful load (12.5% of proxied navigations);
  * ``-32601`` — the browser was launched without ``--page-settle`` — surfacing
    as an exception instead of a fallback;
  * the about:blank lifecycle REPLAY that ``Page.setLifecycleEventsEnabled``
    emits, which makes every fallback signal read as ~0 ms;
  * subframe lifecycle counted as the main frame's (bbc.com/news emits from 23
    frames; first-across-all says networkIdle at 699 ms where the main frame's
    real value is 6094 ms).

Pure stubs, no browser — mirrors test_chromeleon_client.py / test_captcha.py.
The sync page fake carries a fake clock because the sync Playwright API only
dispatches CDP callbacks while you are inside one of its calls: ``wait_for_timeout``
is the pump, so that is where a scripted event is delivered.
"""
import asyncio

import pytest

from chromeleon import (
    LAUNCH_ARGS,
    OUTCOME_CHALLENGE,
    OUTCOME_SETTLED,
    OUTCOME_TIMEOUT,
    PAGE_SETTLE_SWITCH,
    VIA_LOAD,
    VIA_NETWORK_ALMOST_IDLE,
    VIA_TIMEOUT,
    VIA_WAIT_FOR_SETTLE,
    WAIT_FOR_SETTLE_METHOD,
    LIFECYCLE_EVENT,
    SettleState,
    launch,
    settle_launch_args,
    settle_watch,
    settle_watch_async,
    wait_for_settle_params,
)

MAIN = "FRAME-MAIN"
SUB = "FRAME-SUB"
OLD = "LOADER-ABOUT-BLANK"      # the incumbent document, replayed at enable
NEW = "LOADER-TARGET"           # the document goto() commits
NEXT = "LOADER-REDIRECT"

SETTLED_REPLY = {
    "outcome": "settled",
    "elapsedMs": 7306,
    "textLength": 18422,
    "navigations": 2,
    "httpStatus": 200,
    "reason": "quiet window elapsed",
}


def lifecycle(name, frame=MAIN, loader=NEW):
    return {"frameId": frame, "loaderId": loader, "name": name, "timestamp": 1.0}


def emits(*events):
    """A pump step that delivers ``events`` the way the dispatcher would."""
    def step(page):
        for event in events:
            page.cdp.emit(event)
    return step


class _CDP:
    """Playwright-shaped page CDP session: send(method, params=None), on/off, detach."""

    def __init__(self, reply=None, error=None, frame_tree=None):
        self.log = []
        self.handlers = {}
        self.detached = False
        self.detach_error = None
        self._reply = SETTLED_REPLY if reply is None else reply
        self._error = error
        self._frame_tree = frame_tree if frame_tree is not None else {
            "frameTree": {
                "frame": {"id": MAIN, "loaderId": OLD, "url": "about:blank"},
                "childFrames": [{"frame": {"id": SUB, "loaderId": OLD}}],
            }
        }

    # -- driver surface ----------------------------------------------------

    def send(self, method, params=None):
        self.log.append(("send", method, params))
        if method == "Page.getFrameTree":
            return self._frame_tree
        if method == WAIT_FOR_SETTLE_METHOD:
            if self._error is not None:
                raise self._error
            return self._reply
        return {}

    def on(self, event, handler):
        self.log.append(("on", event))
        self.handlers.setdefault(event, []).append(handler)

    def off(self, event, handler):
        self.log.append(("off", event))
        self.handlers.get(event, []).remove(handler)

    def detach(self):
        self.log.append(("detach",))
        if self.detach_error is not None:
            raise self.detach_error
        self.detached = True

    # -- test surface ------------------------------------------------------

    def emit(self, params, event=LIFECYCLE_EVENT):
        for handler in list(self.handlers.get(event, ())):
            handler(params)

    @property
    def sends(self):
        return [(m, p) for kind, m, p in
                (e for e in self.log if e[0] == "send")]

    @property
    def settle_params(self):
        return next(p for m, p in self.sends if m == WAIT_FOR_SETTLE_METHOD)


class _Context:
    def __init__(self, cdp):
        self.cdp = cdp
        self.sessions = 0

    def new_cdp_session(self, page):
        self.sessions += 1
        return self.cdp


class _Page:
    """Sync page. ``wait_for_timeout`` pumps the dispatcher AND the fake clock."""

    def __init__(self, cdp, script=()):
        self.cdp = cdp
        self.context = _Context(cdp)
        self.clock = 1_000.0
        self.script = list(script)
        self.pumps = 0

    def wait_for_timeout(self, ms):
        self.pumps += 1
        self.clock += ms / 1000.0
        if self.script:
            step = self.script.pop(0)
            if step is not None:
                step(self)

    def now(self):
        return self.clock


def watch_on(page, **options):
    options.setdefault("now", page.now)
    return settle_watch(page, **options)


# ── arming ───────────────────────────────────────────────────────────────────

def test_arm_subscribes_before_it_enables_lifecycle_events():
    """Enable-then-subscribe would race the replay and never learn the stale
    loaders, which is the whole point of the window."""
    page = _Page(_CDP())
    with watch_on(page) as watch:
        assert watch.main_frame_id == MAIN
    assert page.cdp.log[:4] == [
        ("send", "Page.enable", None),
        ("send", "Page.getFrameTree", None),
        ("on", LIFECYCLE_EVENT),
        ("send", "Page.setLifecycleEventsEnabled", {"enabled": True}),
    ]


def test_arm_waits_out_the_replay_window():
    page = _Page(_CDP())
    with watch_on(page, replay_window_ms=350):
        pass
    assert page.pumps == 1
    assert page.clock == pytest.approx(1_000.35)


# ── the primary: Chromeleon.waitForSettle ────────────────────────────────────

def test_a_settled_reply_is_returned_verbatim():
    page = _Page(_CDP())
    with watch_on(page) as watch:
        state = watch.wait(timeout_ms=30_000)
    assert state == SettleState(
        outcome="settled", via=VIA_WAIT_FOR_SETTLE, elapsed_ms=7306,
        text_length=18422, navigations=2, http_status=200,
        reason="quiet window elapsed")
    assert state.blocked is False
    assert state.settled is True


def test_challenge_outcome_is_blocked_not_a_load():
    """A wall is 12.5% of proxied navigations. It is never a successful load."""
    reply = {"outcome": "challenge", "elapsedMs": 812, "textLength": 640,
             "navigations": 1, "httpStatus": 403, "reason": "datadome interstitial"}
    page = _Page(_CDP(reply=reply))
    with watch_on(page) as watch:
        state = watch.wait()
    assert state.outcome == OUTCOME_CHALLENGE
    assert state.blocked is True
    assert state.settled is False


def test_a_challenge_shorter_than_min_chars_times_out_but_is_still_blocked():
    """The wall did not reach minChars, so the outcome is "timeout" — the status
    is the only thing left that says it was a wall. Classify on outcome OR
    status, which is what .blocked does."""
    page = _Page(_CDP(reply={"outcome": "timeout", "elapsedMs": 30_000,
                             "textLength": 120, "httpStatus": 403}))
    with watch_on(page) as watch:
        state = watch.wait()
    assert state.outcome == OUTCOME_TIMEOUT
    assert state.blocked is True


@pytest.mark.parametrize("status", [401, 403, 407, 429, 503])
def test_every_wall_status_blocks(status):
    assert SettleState("settled", VIA_WAIT_FOR_SETTLE, http_status=status).blocked
    assert SettleState("settled", VIA_WAIT_FOR_SETTLE, http_status=status).settled is False


@pytest.mark.parametrize("status", [None, 200, 204, 301, 404, 500])
def test_ordinary_statuses_do_not_block(status):
    assert SettleState("settled", VIA_WAIT_FOR_SETTLE, http_status=status).blocked is False


def test_the_command_deadline_is_shaved_below_the_callers_budget():
    """The browser's own answer carries elapsedMs, httpStatus and the challenge
    classification, so it has to land BEFORE our cutoff, not race it."""
    page = _Page(_CDP())
    with watch_on(page) as watch:
        watch.wait(timeout_ms=30_000, quiet_window_ms=2_500, min_chars=400)
    assert page.cdp.settle_params == {
        "quietWindowMs": 2500, "minChars": 400, "timeoutMs": 29_750}


def test_unset_tuning_is_not_sent_as_null():
    page = _Page(_CDP())
    with watch_on(page) as watch:
        watch.wait(timeout_ms=10_000)
    assert page.cdp.settle_params == {"timeoutMs": 9_750}


def test_a_raw_devtools_envelope_is_unwrapped():
    page = _Page(_CDP(reply={"id": 7, "result": dict(SETTLED_REPLY)}))
    with watch_on(page) as watch:
        state = watch.wait()
    assert state.outcome == OUTCOME_SETTLED
    assert state.text_length == 18422


def test_wait_is_idempotent_and_sends_the_command_once():
    page = _Page(_CDP())
    with watch_on(page) as watch:
        first = watch.wait()
        second = watch.wait()
    assert first is second
    assert [m for m, _ in page.cdp.sends].count(WAIT_FOR_SETTLE_METHOD) == 1


def test_wait_before_arming_refuses_rather_than_attaching_late():
    page = _Page(_CDP())
    with pytest.raises(RuntimeError, match="not armed"):
        watch_on(page).wait()


# ── the fallback: Blink networkAlmostIdle ────────────────────────────────────

NOT_FOUND = Exception(
    "Protocol error (Chromeleon.waitForSettle): 'Chromeleon.waitForSettle' "
    "wasn't found")


def test_method_not_found_falls_back_instead_of_raising():
    """Launched without --page-settle. That is a fallback, not a failure."""
    page = _Page(_CDP(error=NOT_FOUND),
                 script=[None, emits(lifecycle("networkAlmostIdle"))])
    with watch_on(page) as watch:
        state = watch.wait(timeout_ms=5_000)
    assert state.via == VIA_NETWORK_ALMOST_IDLE
    assert state.outcome == OUTCOME_SETTLED
    assert state.navigations == 1


def test_a_minus_32601_envelope_from_a_raw_socket_also_falls_back():
    page = _Page(
        _CDP(reply={"error": {"code": -32601, "message": "'Chromeleon.waitForSettle' wasn't found"}}),
        script=[None, emits(lifecycle("networkAlmostIdle"))])
    with watch_on(page) as watch:
        assert watch.wait(timeout_ms=5_000).via == VIA_NETWORK_ALMOST_IDLE


def test_any_other_protocol_error_is_not_swallowed():
    """"Target closed" is not "the feature is off" — hiding it would leave the
    caller waiting out a full budget for a page that no longer exists."""
    page = _Page(_CDP(error=RuntimeError("Protocol error: Target closed")))
    with pytest.raises(RuntimeError, match="Target closed"):
        with watch_on(page) as watch:
            watch.wait(timeout_ms=1_000)


def test_a_refused_command_is_reported_not_treated_as_a_result():
    page = _Page(_CDP(reply={"error": {"code": -32000, "message": "disabled"}}))
    with pytest.raises(RuntimeError, match="refused"):
        with watch_on(page) as watch:
            watch.wait(timeout_ms=1_000)


def test_prefer_network_almost_idle_skips_the_command_entirely():
    """Direct connections: 1.8x faster for the same median content."""
    page = _Page(_CDP(), script=[None, emits(lifecycle("networkAlmostIdle"))])
    with watch_on(page, prefer=VIA_NETWORK_ALMOST_IDLE) as watch:
        state = watch.wait(timeout_ms=5_000)
    assert state.via == VIA_NETWORK_ALMOST_IDLE
    assert WAIT_FOR_SETTLE_METHOD not in [m for m, _ in page.cdp.sends]


def test_prefer_refuses_a_signal_it_cannot_wait_for():
    with pytest.raises(ValueError, match="prefer must be"):
        settle_watch(_Page(_CDP()), prefer="domcontentloaded")


def test_the_about_blank_replay_is_discarded():
    """setLifecycleEventsEnabled replays a FULL lifecycle for the incumbent
    document. Believe it and every fallback settles instantly at ~0 ms."""
    replay = emits(
        lifecycle("init", loader=OLD),
        lifecycle("DOMContentLoaded", loader=OLD),
        lifecycle("load", loader=OLD),
        lifecycle("networkIdle", loader=OLD),
        lifecycle("networkAlmostIdle", loader=OLD),
    )
    # …and a straggler from the same document AFTER the window closes.
    page = _Page(_CDP(error=NOT_FOUND),
                 script=[replay, emits(lifecycle("networkAlmostIdle", loader=OLD))])
    with watch_on(page, replay_window_ms=350) as watch:
        state = watch.wait(timeout_ms=500)
    assert state.via == VIA_TIMEOUT
    assert state.outcome == OUTCOME_TIMEOUT
    assert state.navigations == 0
    assert state.elapsed_ms >= 500


def test_the_new_document_still_settles_after_the_replay_was_discarded():
    page = _Page(_CDP(error=NOT_FOUND),
                 script=[emits(lifecycle("networkAlmostIdle", loader=OLD)),
                         emits(lifecycle("networkAlmostIdle", loader=NEW))])
    with watch_on(page) as watch:
        state = watch.wait(timeout_ms=5_000)
    assert state.via == VIA_NETWORK_ALMOST_IDLE
    assert state.elapsed_ms == pytest.approx(25, abs=1)


def test_subframe_lifecycle_is_ignored():
    """bbc.com/news emits lifecycle from 23 frames; first-across-all reports
    networkIdle at 699 ms where the main frame's real value is 6094 ms."""
    page = _Page(_CDP(error=NOT_FOUND), script=[
        None,
        emits(lifecycle("networkAlmostIdle", frame=SUB)),
        emits(lifecycle("networkAlmostIdle", frame=SUB)),
        emits(lifecycle("networkAlmostIdle", frame=MAIN)),
    ])
    with watch_on(page) as watch:
        state = watch.wait(timeout_ms=5_000)
    assert state.via == VIA_NETWORK_ALMOST_IDLE
    assert state.elapsed_ms == pytest.approx(75, abs=1)   # the 4th pump, not the 2nd


def test_network_idle_alone_does_not_settle():
    """networkIdle is the same rule at 0 in-flight requests — a different, later
    signal. Only networkAlmostIdle is the documented fallback."""
    page = _Page(_CDP(error=NOT_FOUND), script=[None, emits(lifecycle("networkIdle"))])
    with watch_on(page) as watch:
        assert watch.wait(timeout_ms=300).via == VIA_TIMEOUT


def test_the_load_event_is_the_second_tier():
    page = _Page(_CDP(error=NOT_FOUND), script=[None, emits(lifecycle("load"))])
    with watch_on(page) as watch:
        state = watch.wait(timeout_ms=300)
    assert state.via == VIA_LOAD
    assert state.outcome == OUTCOME_SETTLED
    assert state.elapsed_ms == pytest.approx(25, abs=1)
    assert "networkAlmostIdle never fired" in state.reason


def test_almost_idle_wins_over_a_load_that_already_fired():
    page = _Page(_CDP(error=NOT_FOUND), script=[
        None, emits(lifecycle("load")), emits(lifecycle("networkAlmostIdle"))])
    with watch_on(page) as watch:
        assert watch.wait(timeout_ms=5_000).via == VIA_NETWORK_ALMOST_IDLE


def test_navigations_counts_main_frame_documents_only():
    page = _Page(_CDP(error=NOT_FOUND), script=[
        None,
        emits(lifecycle("init", loader=NEW), lifecycle("init", frame=SUB, loader="X")),
        emits(lifecycle("init", loader=NEXT)),
        emits(lifecycle("networkAlmostIdle", loader=NEXT)),
    ])
    with watch_on(page) as watch:
        assert watch.wait(timeout_ms=5_000).navigations == 2


def test_wait_honours_its_timeout():
    """A page that never settles is a NORMAL proxied outcome: no exception, no
    hang, and no answer invented for fields the fallback cannot know."""
    page = _Page(_CDP(error=NOT_FOUND))
    with watch_on(page) as watch:
        state = watch.wait(timeout_ms=200)
    assert (state.outcome, state.via) == (OUTCOME_TIMEOUT, VIA_TIMEOUT)
    assert state.elapsed_ms == pytest.approx(200, abs=25)
    assert page.clock - 1_000.35 <= 0.225
    assert state.text_length is None and state.http_status is None


# ── teardown ─────────────────────────────────────────────────────────────────

def test_close_unsubscribes_and_detaches():
    page = _Page(_CDP())
    with watch_on(page) as watch:
        pass
    assert page.cdp.detached
    assert page.cdp.handlers[LIFECYCLE_EVENT] == []
    assert watch.session is None


def test_the_session_is_detached_even_when_the_body_raises():
    page = _Page(_CDP())
    with pytest.raises(ZeroDivisionError):
        with watch_on(page):
            1 / 0
    assert page.cdp.detached


def test_close_is_idempotent():
    page = _Page(_CDP())
    watch = watch_on(page).arm()
    watch.close()
    watch.close()
    assert [e for e in page.cdp.log if e[0] == "detach"] == [("detach",)]


def test_a_dead_page_does_not_turn_teardown_into_an_error():
    cdp = _CDP()
    cdp.detach_error = RuntimeError("Target page, context or browser has been closed")
    page = _Page(cdp)
    with watch_on(page) as watch:
        state = watch.wait()
    assert state.outcome == OUTCOME_SETTLED       # no exception on the way out


def test_a_borrowed_session_is_never_detached():
    """The caller may already hold a page session for the captcha events."""
    cdp = _CDP()
    page = _Page(cdp)
    with watch_on(page, session=cdp) as watch:
        watch.wait()
    assert cdp.detached is False
    assert page.context.sessions == 0
    assert cdp.handlers[LIFECYCLE_EVENT] == []    # still unsubscribed


# ── launch flags ─────────────────────────────────────────────────────────────

class _Chromium:
    def __init__(self):
        self.kw = None

    def launch(self, **kwargs):
        self.kw = kwargs
        return "browser"


def test_settle_launch_args_default_is_just_the_switch():
    assert settle_launch_args() == (PAGE_SETTLE_SWITCH,) == ("--page-settle",)


def test_settle_launch_args_tuning():
    assert settle_launch_args(quiet_window_ms=2500, min_chars=400,
                              timeout_ms=20_000, sample_interval_ms=100,
                              pierce_shadow=True) == (
        "--page-settle",
        "--page-settle-quiet-window-ms=2500",
        "--page-settle-min-chars=400",
        "--page-settle-timeout-ms=20000",
        "--page-settle-sample-interval-ms=100",
        "--page-settle-pierce-shadow",
    )


def test_page_settle_is_opt_in_and_not_a_default_launch_arg():
    """It changes how the browser behaves, so it is never on by accident."""
    assert all("page-settle" not in flag for flag in LAUNCH_ARGS)
    chromium = _Chromium()
    launch(chromium, "/x")
    assert all("page-settle" not in flag for flag in chromium.kw["args"])


def test_launch_page_settle_true_adds_the_switch_and_keeps_the_webrtc_policy():
    chromium = _Chromium()
    launch(chromium, "/x", page_settle=True)
    assert PAGE_SETTLE_SWITCH in chromium.kw["args"]
    assert LAUNCH_ARGS[0] in chromium.kw["args"]


def test_launch_page_settle_dict_tunes_it():
    chromium = _Chromium()
    launch(chromium, "/x", page_settle={"quiet_window_ms": 2500})
    assert "--page-settle-quiet-window-ms=2500" in chromium.kw["args"]


def test_launch_page_settle_empty_dict_still_turns_it_on():
    """Identity, not truthiness — the same trap ignore_default_args=[] sets."""
    chromium = _Chromium()
    launch(chromium, "/x", page_settle={})
    assert PAGE_SETTLE_SWITCH in chromium.kw["args"]


def test_launch_does_not_duplicate_a_caller_supplied_settle_flag():
    chromium = _Chromium()
    launch(chromium, "/x", page_settle={"quiet_window_ms": 2500},
           args=["--page-settle-quiet-window-ms=9000"])
    assert chromium.kw["args"].count("--page-settle-quiet-window-ms=9000") == 1
    assert "--page-settle-quiet-window-ms=2500" not in chromium.kw["args"]


def test_wait_for_settle_params_omits_what_it_was_not_given():
    assert wait_for_settle_params() == {}
    assert wait_for_settle_params(min_chars=200) == {"minChars": 200}


# ── asyncio twin ─────────────────────────────────────────────────────────────

class _AsyncCDP(_CDP):
    async def send(self, method, params=None):
        return _CDP.send(self, method, params)

    async def detach(self):
        _CDP.detach(self)


class _AsyncContext(_Context):
    async def new_cdp_session(self, page):
        return _Context.new_cdp_session(self, page)


class _AsyncPage:
    def __init__(self, cdp):
        self.cdp = cdp
        self.context = _AsyncContext(cdp)


def test_async_with_hands_back_the_asyncio_twin():
    async def scenario():
        page = _AsyncPage(_AsyncCDP())
        async with settle_watch(page, replay_window_ms=1) as watch:
            state = await watch.wait(timeout_ms=30_000)
        assert state.outcome == OUTCOME_SETTLED
        assert state.via == VIA_WAIT_FOR_SETTLE
        assert state.text_length == 18422
        assert page.cdp.detached
        assert page.cdp.log[:4] == [
            ("send", "Page.enable", None),
            ("send", "Page.getFrameTree", None),
            ("on", LIFECYCLE_EVENT),
            ("send", "Page.setLifecycleEventsEnabled", {"enabled": True}),
        ]

    asyncio.run(scenario())


def test_async_falls_back_to_network_almost_idle():
    async def scenario():
        cdp = _AsyncCDP(error=NOT_FOUND)
        page = _AsyncPage(cdp)
        async with settle_watch_async(page, replay_window_ms=1) as watch:
            loop = asyncio.get_running_loop()
            loop.call_later(0.01, cdp.emit, lifecycle("networkAlmostIdle"))
            loop.call_later(0.005, cdp.emit, lifecycle("networkAlmostIdle", frame=SUB))
            state = await watch.wait(timeout_ms=2_000)
        assert state.via == VIA_NETWORK_ALMOST_IDLE
        assert state.outcome == OUTCOME_SETTLED
        assert state.navigations == 1

    asyncio.run(scenario())


def test_async_discards_the_about_blank_replay():
    async def scenario():
        cdp = _AsyncCDP(error=NOT_FOUND)
        page = _AsyncPage(cdp)
        watch = settle_watch_async(page, replay_window_ms=20)
        loop = asyncio.get_running_loop()
        loop.call_later(0.005, cdp.emit, lifecycle("networkAlmostIdle", loader=OLD))
        async with watch:
            # …and a straggler from the same document after the window closed.
            loop.call_later(0.005, cdp.emit, lifecycle("networkAlmostIdle", loader=OLD))
            state = await watch.wait(timeout_ms=60)
        assert state.via == VIA_TIMEOUT
        assert state.navigations == 0

    asyncio.run(scenario())


def test_async_wait_honours_its_timeout_and_is_idempotent():
    async def scenario():
        page = _AsyncPage(_AsyncCDP(error=NOT_FOUND))
        async with settle_watch_async(page, replay_window_ms=1) as watch:
            first = await watch.wait(timeout_ms=40)
            second = await watch.wait(timeout_ms=40)
        assert first is second
        assert (first.outcome, first.via) == (OUTCOME_TIMEOUT, VIA_TIMEOUT)

    asyncio.run(scenario())


def test_async_borrowed_session_is_not_detached():
    async def scenario():
        cdp = _AsyncCDP()
        page = _AsyncPage(cdp)
        async with settle_watch_async(page, replay_window_ms=1, session=cdp) as watch:
            await watch.wait()
        assert cdp.detached is False

    asyncio.run(scenario())
