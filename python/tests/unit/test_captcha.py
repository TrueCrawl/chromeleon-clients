"""Captcha solver: launch flags + the Chromeleon CDP domain helpers.

Pure-stub, no browser — mirrors test_chromeleon_client.py.
"""
from chromeleon import (
    CAPTCHA_EVENTS,
    CAPTCHA_SOLVED,
    DISABLE_METHOD,
    ENABLE_METHOD,
    LAUNCH_ARGS,
    SOLVER_EVAL_METHOD,
    captcha_launch_args,
    disable_captcha,
    enable_captcha,
    launch,
    solver_eval,
    solver_eval_params,
)


class _Chromium:
    def __init__(self):
        self.kw = None

    def launch(self, **kwargs):
        self.kw = kwargs
        return "browser"


class _CDP:
    """Playwright-shaped CDP session: send(method, params=None)."""

    def __init__(self):
        self.calls = []

    def send(self, method, params=None):
        self.calls.append((method, params))
        return "ok"


# --- launch flags ---------------------------------------------------------

def test_captcha_launch_args_default_is_just_the_solver_switch():
    assert captcha_launch_args() == ("--captcha-solver",)


def test_captcha_launch_args_with_model_path_override():
    assert captcha_launch_args("/models") == (
        "--captcha-solver", "--captcha-model-path=/models")


def test_launch_captcha_true_appends_solver_and_keeps_webrtc_policy():
    ch = _Chromium()
    launch(ch, "/x", captcha=True)
    assert "--captcha-solver" in ch.kw["args"]
    assert LAUNCH_ARGS[0] in ch.kw["args"]
    assert all("--captcha-model-path" not in a for a in ch.kw["args"])


def test_launch_captcha_model_path_implies_solver():
    ch = _Chromium()
    launch(ch, "/x", captcha_model_path="/m")
    assert "--captcha-solver" in ch.kw["args"]
    assert "--captcha-model-path=/m" in ch.kw["args"]


def test_launch_without_captcha_has_no_solver_flag():
    ch = _Chromium()
    launch(ch, "/x")
    assert all("captcha" not in a for a in ch.kw["args"])


def test_launch_does_not_duplicate_a_caller_supplied_captcha_flag():
    ch = _Chromium()
    launch(ch, "/x", captcha=True, args=["--captcha-solver"])
    assert ch.kw["args"].count("--captcha-solver") == 1


# --- CDP domain helpers ---------------------------------------------------

def test_enable_and_disable_send_the_right_commands():
    cdp = _CDP()
    enable_captcha(cdp)
    disable_captcha(cdp)
    assert cdp.calls == [(ENABLE_METHOD, None), (DISABLE_METHOD, None)]


def test_solver_eval_sends_expression_and_frame():
    cdp = _CDP()
    solver_eval(cdp, "document.title", "checkout")
    assert cdp.calls == [
        (SOLVER_EVAL_METHOD,
         {"expression": "document.title", "frameUrlContains": "checkout"}),
    ]


def test_solver_eval_params_defaults_to_primary_main_frame():
    assert solver_eval_params("x") == {"expression": "x", "frameUrlContains": ""}


def test_captcha_events_are_the_four_lifecycle_events():
    assert CAPTCHA_SOLVED in CAPTCHA_EVENTS
    assert len(CAPTCHA_EVENTS) == 4
