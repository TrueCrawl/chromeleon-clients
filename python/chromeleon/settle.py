"""Page completion: settle on RENDERED TEXT, with networkAlmostIdle as fallback.

Knowing that a page is *done* is the hard half of scraping it. Chromeleon
answers it in the BROWSER process, sampled over Blink's inner-text channel, with
one CDP command:

    Chromeleon.waitForSettle {quietWindowMs?, minChars?, timeoutMs?}
      -> {outcome, elapsedMs, textLength, navigations, httpStatus, reason}

It settles when the main frame's rendered text has been unchanged for a quiet
window. No JavaScript runs in the page, no isolated world, nothing registered on
the document — which is the whole point of it over a MutationObserver you inject
yourself.

The domain is only registered when the browser was launched with
``--page-settle`` (:func:`settle_launch_args`, or ``launch(..., page_settle=True)``).
Without it the call comes back ``-32601 method not found``, so this module also
arms Blink's own ``networkAlmostIdle`` lifecycle signal and falls back to it
without raising.

WHY THAT ORDER, measured over 960 navigations, 80 sites, 3 rounds::

                              direct p50 | never | proxied p50 | never
    networkAlmostIdle            3,989ms |   1%  |   11,285ms  |  36%
    Chromeleon.waitForSettle     7,306ms |   6%  |   12,108ms  |   7%

THROUGH A PROXY — which is what this client is for — networkAlmostIdle fails to
fire on more than a third of loads while waitForSettle misses 7%, so the command
is the primary. On a direct connection networkAlmostIdle is 1.8x faster for the
same median content, which is why it stays available and is the documented
choice for un-proxied work (``prefer=VIA_NETWORK_ALMOST_IDLE``). On completeness,
direct: waitForSettle reproduced the final text EXACTLY on 79% of loads,
networkAlmostIdle 59%, the load event 40%, domcontentloaded 7%.

Two phases, because a one-shot helper called after ``goto`` cannot work — it
would attach after the document committed and miss everything::

    with settle_watch(page) as watch:              # attaches CDP BEFORE the nav
        page.goto(url, wait_until="commit")
        state = watch.wait(timeout_ms=30_000)

    async with settle_watch(page) as watch:        # asyncio twin, same shape
        await page.goto(url, wait_until="commit")
        state = await watch.wait(timeout_ms=30_000)

⚠️ ATTACH BEFORE NAVIGATING. The challenge header is recorded at commit time by
a tracker the handler attaches when it is CONSTRUCTED; a session attached to an
already-committed document cannot see it and degrades silently to
status-code-only detection. The ``with`` block is what makes that ordering
mistake structurally impossible, so there is deliberately no ``wait_for_settle(page)``
convenience that takes the shape which cannot be used correctly.

⚠️ ``outcome == "challenge"`` is a BOT WALL, not a load — 12.5% of proxied
navigations in production measurement — and is never reported as a successful
one. A wall SHORTER than ``minChars`` comes back as ``"timeout"`` instead, but
``httpStatus`` still carries the 401/403/407/429/503, so classify on outcome OR
status. :attr:`SettleState.blocked` is exactly that classification.
"""
from __future__ import annotations

import asyncio
import time
from typing import Any, Callable, NamedTuple

__all__ = [
    "PAGE_SETTLE_SWITCH",
    "PAGE_SETTLE_QUIET_WINDOW_SWITCH",
    "PAGE_SETTLE_MIN_CHARS_SWITCH",
    "PAGE_SETTLE_TIMEOUT_SWITCH",
    "PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH",
    "PAGE_SETTLE_PIERCE_SHADOW_SWITCH",
    "WAIT_FOR_SETTLE_METHOD",
    "LIFECYCLE_EVENT",
    "OUTCOME_SETTLED",
    "OUTCOME_TIMEOUT",
    "OUTCOME_CHALLENGE",
    "SETTLE_OUTCOMES",
    "VIA_WAIT_FOR_SETTLE",
    "VIA_NETWORK_ALMOST_IDLE",
    "VIA_LOAD",
    "VIA_TIMEOUT",
    "BLOCKED_STATUSES",
    "REPLAY_WINDOW_MS",
    "DEFAULT_TIMEOUT_MS",
    "SettleState",
    "SettleWatch",
    "AsyncSettleWatch",
    "settle_launch_args",
    "settle_watch",
    "settle_watch_async",
    "wait_for_settle_params",
]

# --- protocol facts -------------------------------------------------------

#: Launch switch that REGISTERS the domain. Without it ``Chromeleon.waitForSettle``
#: is ``-32601 method not found`` and this module falls back to lifecycle events.
#: It is opt-in and deliberately NOT in ``LAUNCH_ARGS``: it changes how the
#: browser behaves, so turning it on is the caller's decision.
PAGE_SETTLE_SWITCH = "--page-settle"
#: Optional tuning switches. Quiet window defaults to 4000 ms in the binary.
PAGE_SETTLE_QUIET_WINDOW_SWITCH = "--page-settle-quiet-window-ms"
PAGE_SETTLE_MIN_CHARS_SWITCH = "--page-settle-min-chars"
PAGE_SETTLE_TIMEOUT_SWITCH = "--page-settle-timeout-ms"
PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH = "--page-settle-sample-interval-ms"
PAGE_SETTLE_PIERCE_SHADOW_SWITCH = "--page-settle-pierce-shadow"

#: The command, sent on a PAGE CDP session (as the captcha domain is).
WAIT_FOR_SETTLE_METHOD = "Chromeleon.waitForSettle"

#: Blink's fallback channel. ``networkAlmostIdle`` = at most 2 in-flight
#: requests sustained for 500 ms; ``networkIdle`` is the same rule at 0. Neither
#: is a command — they arrive only as ``Page.lifecycleEvent`` names.
LIFECYCLE_EVENT = "Page.lifecycleEvent"
_ALMOST_IDLE = "networkAlmostIdle"
_LOAD = "load"

#: ``outcome`` values the command can return.
OUTCOME_SETTLED = "settled"
OUTCOME_TIMEOUT = "timeout"
OUTCOME_CHALLENGE = "challenge"
SETTLE_OUTCOMES = (OUTCOME_SETTLED, OUTCOME_TIMEOUT, OUTCOME_CHALLENGE)

#: ``via`` values — WHICH signal produced the answer, in preference order.
VIA_WAIT_FOR_SETTLE = "waitForSettle"
VIA_NETWORK_ALMOST_IDLE = "networkAlmostIdle"
VIA_LOAD = "load"
VIA_TIMEOUT = "timeout"

#: Statuses that mean a wall rather than a page, even when the text settled.
BLOCKED_STATUSES = (401, 403, 407, 429, 503)

#: How long to let ``Page.setLifecycleEventsEnabled`` REPLAY the incumbent
#: about:blank's lifecycle before believing anything. Keep the replay and every
#: fallback signal reads as ~0 ms — an instant, wrong "settled".
REPLAY_WINDOW_MS = 350

#: Overall budget for :meth:`SettleWatch.wait` when the caller names none.
DEFAULT_TIMEOUT_MS = 30_000

#: Shaved off the command's own ``timeoutMs`` so the browser's answer — which
#: carries elapsedMs, httpStatus and the challenge classification — arrives
#: BEFORE our client-side cutoff instead of racing it to the same instant.
_REPLY_GRACE_MS = 250

#: Sync poll step. The sync Playwright API only dispatches CDP callbacks while
#: you are inside one of its calls, so the fallback has to pump, not sleep.
_POLL_INTERVAL_MS = 25

_NOT_FOUND_MARKERS = (
    "-32601",
    "wasn't found",
    "was not found",
    "method not found",
    "not implemented",
    "unknown method",
)


def settle_launch_args(
    *,
    quiet_window_ms: int | None = None,
    min_chars: int | None = None,
    timeout_ms: int | None = None,
    sample_interval_ms: int | None = None,
    pierce_shadow: bool = False,
) -> tuple[str, ...]:
    """Launch flags that register ``Chromeleon.waitForSettle``.

    ``--page-settle`` alone is enough; every other switch is a tuning override
    of a binary default (the quiet window is 4000 ms). Merge them yourself, or
    let :func:`chromeleon.launch` do it with ``page_settle=True`` /
    ``page_settle={"quiet_window_ms": 2500}``.

        settle_launch_args(quiet_window_ms=2500, pierce_shadow=True)
        # ('--page-settle', '--page-settle-quiet-window-ms=2500',
        #  '--page-settle-pierce-shadow')
    """
    args = [PAGE_SETTLE_SWITCH]
    for switch, value in (
        (PAGE_SETTLE_QUIET_WINDOW_SWITCH, quiet_window_ms),
        (PAGE_SETTLE_MIN_CHARS_SWITCH, min_chars),
        (PAGE_SETTLE_TIMEOUT_SWITCH, timeout_ms),
        (PAGE_SETTLE_SAMPLE_INTERVAL_SWITCH, sample_interval_ms),
    ):
        if value is not None:
            args.append(f"{switch}={int(value)}")
    if pierce_shadow:
        args.append(PAGE_SETTLE_PIERCE_SHADOW_SWITCH)
    return tuple(args)


def wait_for_settle_params(
    quiet_window_ms: int | None = None,
    min_chars: int | None = None,
    timeout_ms: int | None = None,
) -> dict[str, int]:
    """Params for ``Chromeleon.waitForSettle``.

    Every field is optional and an omitted one keeps the launch-time default, so
    ``None`` is not sent as ``null`` — it is not sent at all.
    """
    params: dict[str, int] = {}
    if quiet_window_ms is not None:
        params["quietWindowMs"] = int(quiet_window_ms)
    if min_chars is not None:
        params["minChars"] = int(min_chars)
    if timeout_ms is not None:
        params["timeoutMs"] = int(timeout_ms)
    return params


class SettleState(NamedTuple):
    """What the page did, and which signal said so.

    Every field comes back from the command verbatim. On a fallback path the
    two only it can know — ``text_length`` and ``http_status`` — are ``None``
    rather than a plausible zero, because a plausible number nobody measured is
    the failure mode this whole module exists to avoid.
    """

    outcome: str            # "settled" | "timeout" | "challenge"
    via: str                # "waitForSettle" | "networkAlmostIdle" | "load" | "timeout"
    elapsed_ms: int | None = None
    text_length: int | None = None
    navigations: int | None = None
    http_status: int | None = None
    reason: str | None = None

    @property
    def blocked(self) -> bool:
        """True when this was a bot wall rather than a page.

        Outcome OR status, because a challenge shorter than ``minChars`` never
        reaches the "challenge" outcome — it times out with a 403 still on the
        committed document.
        """
        return self.outcome == OUTCOME_CHALLENGE or self.http_status in BLOCKED_STATUSES

    @property
    def settled(self) -> bool:
        """True when some signal reported completion (never true for a wall)."""
        return self.outcome == OUTCOME_SETTLED and not self.blocked


class _Tracker:
    """Main-frame lifecycle bookkeeping. Pure state, no I/O, no driver.

    Two filters, both of which cost you the whole measurement when missing:

    * FRAME. Lifecycle is emitted by every frame — bbc.com/news emits from 23 of
      them. First-across-all-frames reports ``networkIdle`` at 699 ms when the
      main frame's real value is 6094 ms.
    * LOADER. ``Page.setLifecycleEventsEnabled`` replays a full lifecycle for the
      incumbent about:blank, so every signal is already "fired" before the
      navigation starts. Loaders seen during the replay window are stale
      forever.
    """

    def __init__(self) -> None:
        self.main_frame_id: str | None = None
        self.arming = True
        self.almost_idle_at: float | None = None
        self.load_at: float | None = None
        self._stale: set[str] = set()
        self._loaders: list[str] = []

    def note_frame_tree(self, tree: Any) -> str | None:
        """Record the MAIN frame id (and its incumbent loader, which is stale)."""
        frame = (((tree or {}).get("frameTree") or {}).get("frame")) or {}
        self.main_frame_id = frame.get("id")
        loader = frame.get("loaderId")
        if loader:
            self._stale.add(loader)
        return self.main_frame_id

    def close_replay_window(self) -> None:
        self.arming = False

    def on_lifecycle(self, params: Any, now: float) -> str | None:
        """Feed one ``Page.lifecycleEvent``; return its name if it counts."""
        params = params or {}
        loader = params.get("loaderId")
        if self.arming:
            # Anything that arrives before the window closes belongs to the
            # document we were attached to, not the one we are waiting for.
            if loader:
                self._stale.add(loader)
            return None
        if self.main_frame_id is None or params.get("frameId") != self.main_frame_id:
            return None
        if loader in self._stale:
            return None
        if loader and loader not in self._loaders:
            self._loaders.append(loader)
        name = params.get("name")
        if name == _ALMOST_IDLE and self.almost_idle_at is None:
            self.almost_idle_at = now
        elif name == _LOAD and self.load_at is None:
            self.load_at = now
        return name

    @property
    def navigations(self) -> int:
        """Main-frame documents committed since the watch was armed."""
        return len(self._loaders)


def _is_method_not_found(error: Any) -> bool:
    """Is this "the browser was launched without --page-settle"?

    Playwright raises with the protocol message and no code; a raw DevTools
    socket hands back ``{"error": {"code": -32601}}``. Both mean the same thing,
    and neither is a reason to fail a scrape.
    """
    if isinstance(error, dict):
        detail = error.get("error") if isinstance(error.get("error"), dict) else error
        if detail.get("code") == -32601:
            return True
        text = str(detail.get("message") or "")
    else:
        text = str(error)
    text = text.lower()
    return any(marker in text for marker in _NOT_FOUND_MARKERS)


class _WatchBase:
    """Everything both twins share: options, bookkeeping, and classification.

    The sync and async classes differ only in how they talk to the driver
    (``await`` or not), so nothing that decides anything lives in them.
    """

    def __init__(
        self,
        page: Any,
        *,
        session: Any | None = None,
        replay_window_ms: int = REPLAY_WINDOW_MS,
        prefer: str = VIA_WAIT_FOR_SETTLE,
        now: Callable[[], float] = time.monotonic,
    ) -> None:
        if prefer not in (VIA_WAIT_FOR_SETTLE, VIA_NETWORK_ALMOST_IDLE):
            raise ValueError(
                f"prefer must be {VIA_WAIT_FOR_SETTLE!r} or "
                f"{VIA_NETWORK_ALMOST_IDLE!r}, got {prefer!r}"
            )
        self._page = page
        self._session = session
        self._owns_session = session is None
        self._replay_window_ms = replay_window_ms
        self._prefer = prefer
        self._now = now
        self._tracker = _Tracker()
        self._state: SettleState | None = None
        self._armed = False

    # -- shared plumbing ---------------------------------------------------

    @property
    def session(self) -> Any:
        """The CDP session this watch is armed on (``None`` once closed)."""
        return self._session

    @property
    def main_frame_id(self) -> str | None:
        return self._tracker.main_frame_id

    def _on_lifecycle(self, params: Any) -> None:
        name = self._tracker.on_lifecycle(params, self._now())
        if name == _ALMOST_IDLE:
            self._wake()

    def _wake(self) -> None:
        """Async twin overrides this to release its waiter."""

    def _require_armed(self) -> None:
        if not self._armed:
            raise RuntimeError(
                "settle watch is not armed — call wait() INSIDE the "
                "`with settle_watch(page)` block, so the CDP session is attached "
                "before the navigation commits"
            )

    def _unsubscribe(self, session: Any) -> None:
        remove = getattr(session, "off", None) or getattr(session, "remove_listener", None)
        if remove is None:
            return
        try:
            remove(LIFECYCLE_EVENT, self._on_lifecycle)
        except Exception:
            pass          # a driver that never registered it is not a failure

    # -- classification ----------------------------------------------------

    def _command_params(
        self, quiet_window_ms: int | None, min_chars: int | None, timeout_ms: int
    ) -> dict[str, int]:
        # The browser gets the SHORTER deadline on purpose: its own reply is
        # richer than our cutoff (elapsedMs from its clock, httpStatus, the
        # challenge classification), so it must land first.
        return wait_for_settle_params(
            quiet_window_ms, min_chars, max(1, int(timeout_ms) - _REPLY_GRACE_MS)
        )

    def _state_from_reply(self, reply: Any) -> SettleState:
        """The command's answer, verbatim — we rename fields, nothing else."""
        if isinstance(reply, dict) and "outcome" not in reply and isinstance(
            reply.get("result"), dict
        ):
            reply = reply["result"]        # raw DevTools socket envelope
        reply = reply if isinstance(reply, dict) else {}
        return SettleState(
            outcome=str(reply.get("outcome") or OUTCOME_TIMEOUT),
            via=VIA_WAIT_FOR_SETTLE,
            elapsed_ms=reply.get("elapsedMs"),
            text_length=reply.get("textLength"),
            navigations=reply.get("navigations"),
            http_status=reply.get("httpStatus"),
            reason=reply.get("reason"),
        )

    def _fallback_state(self, started: float) -> SettleState:
        """Best available lifecycle answer once the command is out of play.

        networkAlmostIdle, else the load event, else nothing — and "nothing" is
        a normal proxied outcome, not an error.
        """
        tracker = self._tracker
        if tracker.almost_idle_at is not None:
            return SettleState(
                outcome=OUTCOME_SETTLED,
                via=VIA_NETWORK_ALMOST_IDLE,
                elapsed_ms=int((tracker.almost_idle_at - started) * 1000),
                navigations=tracker.navigations,
                reason="main-frame networkAlmostIdle",
            )
        if tracker.load_at is not None:
            return SettleState(
                outcome=OUTCOME_SETTLED,
                via=VIA_LOAD,
                elapsed_ms=int((tracker.load_at - started) * 1000),
                navigations=tracker.navigations,
                reason=("networkAlmostIdle never fired within the budget; "
                        "fell back to the main-frame load event"),
            )
        return SettleState(
            outcome=OUTCOME_TIMEOUT,
            via=VIA_TIMEOUT,
            elapsed_ms=int((self._now() - started) * 1000),
            navigations=tracker.navigations,
            reason="no main-frame completion signal within the budget",
        )

    def _refused(self, reply: Any) -> RuntimeError:
        return RuntimeError(f"{WAIT_FOR_SETTLE_METHOD} refused: {reply!r}")


class SettleWatch(_WatchBase):
    """Sync twin. Arm it around the navigation, then ask what happened.

        with settle_watch(page) as watch:
            page.goto(url, wait_until="commit")
            state = watch.wait(timeout_ms=30_000)
            if state.blocked:
                ...

    ``async with`` on the very same object hands back the asyncio twin instead,
    so async callers do not have to remember a second name.
    """

    def __init__(self, page: Any, **options: Any) -> None:
        super().__init__(page, **options)
        self._options = options
        self._twin: AsyncSettleWatch | None = None

    # -- context managers --------------------------------------------------

    def __enter__(self) -> "SettleWatch":
        return self.arm()

    def __exit__(self, *exc_info: Any) -> None:
        self.close()

    async def __aenter__(self) -> "AsyncSettleWatch":
        self._twin = AsyncSettleWatch(self._page, **self._options)
        await self._twin.arm()
        return self._twin

    async def __aexit__(self, *exc_info: Any) -> None:
        twin, self._twin = self._twin, None
        if twin is not None:
            await twin.aclose()

    # -- phase 1 -----------------------------------------------------------

    def arm(self) -> "SettleWatch":
        """Attach, learn the main frame, and start ignoring the incumbent."""
        if self._armed:
            return self
        if self._session is None:
            self._session = self._page.context.new_cdp_session(self._page)
        session = self._session
        session.send("Page.enable")
        self._tracker.note_frame_tree(session.send("Page.getFrameTree"))
        # Subscribe BEFORE enabling, or the replay races the subscription and
        # the stale loaders are never learned.
        session.on(LIFECYCLE_EVENT, self._on_lifecycle)
        session.send("Page.setLifecycleEventsEnabled", {"enabled": True})
        self._sleep(self._replay_window_ms)
        self._tracker.close_replay_window()
        self._armed = True
        return self

    # -- phase 2 -----------------------------------------------------------

    def wait(
        self,
        timeout_ms: int = DEFAULT_TIMEOUT_MS,
        *,
        quiet_window_ms: int | None = None,
        min_chars: int | None = None,
    ) -> SettleState:
        """Wait for the page, and NEVER raise merely because it did not settle.

        ``timeout_ms`` is the whole budget, command included. Calling this twice
        returns the first answer again rather than re-waiting — one watch spans
        one navigation.

        The sync driver call cannot be interrupted from this thread, so the
        command is bounded by the ``timeoutMs`` the browser is given (250 ms
        inside your budget) rather than by a second timer here; the lifecycle
        fallback is bounded by the budget directly. The asyncio twin enforces
        both client-side.
        """
        if self._state is not None:
            return self._state
        self._require_armed()
        started = self._now()
        deadline = started + timeout_ms / 1000.0

        if self._prefer == VIA_WAIT_FOR_SETTLE:
            params = self._command_params(quiet_window_ms, min_chars, timeout_ms)
            try:
                reply = self._session.send(WAIT_FOR_SETTLE_METHOD, params)
            except Exception as exc:                     # noqa: BLE001 — driver-shaped
                if not _is_method_not_found(exc):
                    raise
            else:
                if isinstance(reply, dict) and reply.get("error"):
                    if not _is_method_not_found(reply):
                        raise self._refused(reply)
                else:
                    self._state = self._state_from_reply(reply)
                    return self._state

        self._state = self._await_lifecycle(started, deadline)
        return self._state

    def _await_lifecycle(self, started: float, deadline: float) -> SettleState:
        while True:
            if self._tracker.almost_idle_at is not None:
                break
            remaining = deadline - self._now()
            if remaining <= 0:
                break
            self._sleep(min(_POLL_INTERVAL_MS, remaining * 1000.0))
        return self._fallback_state(started)

    def _sleep(self, ms: float) -> None:
        # page.wait_for_timeout is not a nicer time.sleep: it PUMPS the sync
        # dispatcher, which is the only way CDP callbacks ever run here.
        pump = getattr(self._page, "wait_for_timeout", None)
        if pump is not None:
            pump(ms)
        else:
            time.sleep(ms / 1000.0)

    # -- teardown ----------------------------------------------------------

    def close(self) -> None:
        """Detach. Idempotent, and never raises on an already-dead page."""
        session, self._session = self._session, None
        self._armed = False
        if session is None:
            return
        self._unsubscribe(session)
        if self._owns_session:
            try:
                session.detach()
            except Exception:
                pass      # the page may already be gone; that is not our error


class AsyncSettleWatch(_WatchBase):
    """asyncio twin of :class:`SettleWatch`.

        async with settle_watch(page) as watch:
            await page.goto(url, wait_until="commit")
            state = await watch.wait(timeout_ms=30_000)

    ``settle_watch_async(page)`` returns this directly, mirroring how
    ``new_proxy_context`` / ``new_proxy_context_async`` are paired.
    """

    def __init__(self, page: Any, **options: Any) -> None:
        super().__init__(page, **options)
        self._signal: asyncio.Event | None = None

    async def __aenter__(self) -> "AsyncSettleWatch":
        return await self.arm()

    async def __aexit__(self, *exc_info: Any) -> None:
        await self.aclose()

    def _wake(self) -> None:
        if self._signal is not None:
            self._signal.set()

    async def arm(self) -> "AsyncSettleWatch":
        """Attach, learn the main frame, and start ignoring the incumbent."""
        if self._armed:
            return self
        self._signal = asyncio.Event()
        if self._session is None:
            self._session = await self._page.context.new_cdp_session(self._page)
        session = self._session
        await session.send("Page.enable")
        self._tracker.note_frame_tree(await session.send("Page.getFrameTree"))
        session.on(LIFECYCLE_EVENT, self._on_lifecycle)
        await session.send("Page.setLifecycleEventsEnabled", {"enabled": True})
        await asyncio.sleep(self._replay_window_ms / 1000.0)
        self._tracker.close_replay_window()
        self._armed = True
        return self

    async def wait(
        self,
        timeout_ms: int = DEFAULT_TIMEOUT_MS,
        *,
        quiet_window_ms: int | None = None,
        min_chars: int | None = None,
    ) -> SettleState:
        """Wait for the page, and NEVER raise merely because it did not settle."""
        if self._state is not None:
            return self._state
        self._require_armed()
        started = self._now()
        deadline = started + timeout_ms / 1000.0

        if self._prefer == VIA_WAIT_FOR_SETTLE:
            params = self._command_params(quiet_window_ms, min_chars, timeout_ms)
            send = self._session.send(WAIT_FOR_SETTLE_METHOD, params)
            try:
                reply = await asyncio.wait_for(send, timeout=max(0.0, deadline - self._now()))
            except asyncio.TimeoutError:
                # The command outlived the whole budget: report the timeout
                # rather than hanging on it.
                self._state = self._fallback_state(started)
                return self._state
            except Exception as exc:                     # noqa: BLE001 — driver-shaped
                if not _is_method_not_found(exc):
                    raise
            else:
                if isinstance(reply, dict) and reply.get("error"):
                    if not _is_method_not_found(reply):
                        raise self._refused(reply)
                else:
                    self._state = self._state_from_reply(reply)
                    return self._state

        self._state = await self._await_lifecycle(started, deadline)
        return self._state

    async def _await_lifecycle(self, started: float, deadline: float) -> SettleState:
        # arm() created the signal; _require_armed() guarantees we got there.
        if self._tracker.almost_idle_at is None:
            try:
                await asyncio.wait_for(
                    self._signal.wait(), timeout=max(0.0, deadline - self._now())
                )
            except asyncio.TimeoutError:
                pass
        return self._fallback_state(started)

    async def aclose(self) -> None:
        """Detach. Idempotent, and never raises on an already-dead page."""
        session, self._session = self._session, None
        self._armed = False
        if session is None:
            return
        self._unsubscribe(session)
        if self._owns_session:
            try:
                result = session.detach()
                if hasattr(result, "__await__"):
                    await result
            except Exception:
                pass      # the page may already be gone; that is not our error


def settle_watch(page: Any, **options: Any) -> SettleWatch:
    """Arm a page-completion watch AROUND a navigation.

        with settle_watch(page) as watch:              # sync
            page.goto(url, wait_until="commit")
            state = watch.wait(timeout_ms=30_000)

        async with settle_watch(page) as watch:        # asyncio, same object
            await page.goto(url, wait_until="commit")
            state = await watch.wait(timeout_ms=30_000)

    Entering with ``async with`` hands back an :class:`AsyncSettleWatch`, because
    the driver calls differ (``await`` or not) even though the protocol does not.
    ``settle_watch_async(page)`` is the explicit spelling of the same thing.

    :param session: an existing page CDP session to reuse (e.g. the one already
        carrying the captcha events). We never detach a session we did not open.
    :param replay_window_ms: how long to treat lifecycle events as the incumbent
        document's replay (default 350 ms).
    :param prefer: ``VIA_WAIT_FOR_SETTLE`` (default) or ``VIA_NETWORK_ALMOST_IDLE``
        to skip the command outright — 1.8x faster, and only sane un-proxied.
    :param now: injectable monotonic clock, for tests.
    """
    return SettleWatch(page, **options)


def settle_watch_async(page: Any, **options: Any) -> AsyncSettleWatch:
    """asyncio twin of :func:`settle_watch`; takes the same options."""
    return AsyncSettleWatch(page, **options)
