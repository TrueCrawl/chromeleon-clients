"""launch() must suppress Playwright's own Google-services switches by default.

A Playwright-launched Chromeleon was blocked by Google materially more often than
the same binary launched bare on the same exit (113 paired trials on 113 distinct
exits, +22.1 pp, McNemar p = 1.6e-6). Playwright adds ~46 flags of its own that a
bare launch does not, and a caller has no way to know which of them we care
about — so launch() owns that decision.
"""
from __future__ import annotations

from chromeleon import launch
from chromeleon.core import SUPPRESSED_DEFAULT_ARGS


class _FakeChromium:
    def __init__(self) -> None:
        self.calls: list[dict] = []

    def launch(self, **kwargs):
        self.calls.append(kwargs)
        return object()


def test_launch_suppresses_playwright_default_args() -> None:
    chromium = _FakeChromium()
    launch(chromium, "/bin/chrome")
    assert chromium.calls[0]["ignore_default_args"] == list(SUPPRESSED_DEFAULT_ARGS)
    assert "--disable-field-trial-config" in chromium.calls[0]["ignore_default_args"]


def test_explicit_ignore_default_args_replaces_the_default() -> None:
    chromium = _FakeChromium()
    launch(chromium, "/bin/chrome", ignore_default_args=["--only-this"])
    assert chromium.calls[0]["ignore_default_args"] == ["--only-this"]
