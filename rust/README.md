# chromeleon (Rust)

A thin, **driver-agnostic** Rust client for Chromeleon's per-context proxy
handshake. No browser library is a dependency: it works with
[chromiumoxide](https://docs.rs/chromiumoxide), with `headless_chrome`, and with
a raw DevTools WebSocket, in async or blocking code.

See [`HANDSHAKE.md`](https://github.com/TrueCrawl/chromeleon-clients/blob/main/HANDSHAKE.md)
for the protocol this implements.

## Install

```toml
[dependencies]
chromeleon = "0.1"     # https://crates.io/crates/chromeleon
# optional: the chromiumoxide adapter (needs rustc >= 1.85, chromiumoxide's own floor)
chromeleon = { version = "0.1", features = ["chromiumoxide"] }
```

MSRV **1.75**, and CI builds and tests on exactly that toolchain — but read the
caveat, because it is about your dependency resolution, not our code. Newer
releases in this tree require 1.85 (`async-lock` 3.4.2 declares it; `url` 2.5.4+
reaches `idna_adapter`, which is edition 2024 and will not even *parse* on an old
cargo), and a plain `cargo build` takes the newest. On 1.75 you therefore need
one of: the `Cargo.lock` committed here, `[resolver]
incompatible-rust-versions = "fallback"` (cargo ≥ 1.84, so not the cargo that
ships with 1.75), or your own `cargo update --precise` pins. On any recent
toolchain none of this applies.

## Launch, then attach

Rust drives browsers by spawning them and attaching over CDP, so that is the
shape of the launch helper. It merges the WebRTC policy flag a per-context proxy
needs and strips the controller's `PROXY_*` variables from the browser's
environment — the two things your driver has no way to know about.

```rust
use chromeleon::launch::Launcher;

let (mut child, ws_url) = Launcher::new("/opt/chromeleon/chrome")
    .remote_debugging_port(9222)
    .arg("--headless=new")
    .fingerprint_os("Linux")       // ⚠️ see below — the default is Windows
    .captcha(true)                 // the built-in solver, on
    .spawn_and_wait(Duration::from_secs(20))?;   // returns the DevTools WebSocket URL
```

Two things that bite:

- **Never pass proxy credentials at launch.** The binary fails closed on an
  unresolved exit IP, because that would unbind the persona's geo and disable
  WebRTC masking. Per-context proxies are launched with no `--proxy-server`.
- **The persona OS defaults to Windows.** It is read from the browser's command
  line when a *context* is created, so a launcher that never sets
  `--fingerprint-os` ships Windows personas from a Linux host, silently.
  `Launcher::fingerprint_os` is the switch.

## The handshake

One call, for any async CDP connection:

```rust
use chromeleon::adapters::new_proxy_context_raw;

let created = new_proxy_context_raw(
    ws_url,                                     // identifies the connection
    "http://user:pass@gateway:12321",           // credentials are decoded for you
    |method, params| cdp.call(method, params),
).await?;
let context_id = created["browserContextId"].as_str().unwrap();
```

…or step by step, when your driver creates contexts its own way:

```rust
use chromeleon::{check_registration, registration::proxy_registration, CREDENTIALS_METHOD};

let reg = proxy_registration(ws_url, "http://user:pass@gateway:12321").await?;
check_registration(&browser.send(CREDENTIALS_METHOD, reg.credentials_params().to_value()).await?)?;
let context = browser.create_context(reg.server()).await?;   // same bytes, still under the guard
drop(reg);                                                   // released only after the create
```

The guard is the API's whole point: the registration is single-use — consumed by
the create, and consumed even when that create then fails — so `Registration` is held
across **both** commands and serializes anything else that wants the same
`(connection, server)`. Different servers on one connection run concurrently,
which is the throughput case:

```rust
let (a, b) = futures::join!(
    new_proxy_context_raw(&ws, PROXY_A, send),
    new_proxy_context_raw(&ws, PROXY_B, send),   // different server: no waiting
);
```

Synchronous drivers use the same lock through
`new_proxy_context_raw_blocking` / `proxy_registration_blocking`.

## chromiumoxide

```rust
use chromeleon::oxide::new_proxy_context;

let (browser, mut handler) = Browser::connect(ws_url).await?;   // from Launcher, above
tokio::spawn(async move { while handler.next().await.is_some() {} });

let context = new_proxy_context(&browser, "http://user:pass@gateway:12321").await?;
let mut target = CreateTargetParams::new("https://api.ipify.org");
target.browser_context_id = Some(context);
let page = browser.new_page(target).await?;
```

`Target.setProxyCredentials` is not in chromiumoxide's generated protocol and it
has no untyped `send`; `oxide::RawCommand` is the escape hatch, and
`oxide::send_raw` / `send_raw_page` expose it. `oxide::browser_config` curates
chromiumoxide's own default flags (it adds `--enable-automation` and
`--lang=en_US`, neither of which belongs on this browser) — but note it **cannot
strip the controller's `PROXY_*`**, because `BrowserConfig` only adds
environment variables. That is why the example above launches with `Launcher`
and attaches. Full example: `examples/chromiumoxide_proxy_context.rs`.

## Captcha solving

Launch with `.captcha(true)` and the built-in reCAPTCHA/hCaptcha solver handles
challenges by itself — you do not call it, and the release binary embeds the
models. The `Chromeleon` CDP domain only reports the lifecycle: enable it on a
**page** session and parse the events.

```rust
use chromeleon::captcha::{CaptchaEvent, ENABLE_METHOD};

page_session.send(ENABLE_METHOD, json!({})).await?;
while let Some((method, params)) = events.next().await {
    match CaptchaEvent::parse(&method, &params) {
        Some(CaptchaEvent::Solved { time_ms, .. }) => println!("solved in {time_ms:.0}ms"),
        Some(CaptchaEvent::Failed { reason, .. })  => println!("failed: {reason}"),
        _ => {}
    }
}
```

`solver_eval_params(expression, frame_url_contains)` runs JS in the solver's
isolated world, which pierces **closed** shadow roots; its string result arrives
on the `SOLVER_EVAL_RESULT` event, not as the command's return value.

Three things the other clients get wrong or leave out, checked against the
browser source:

- `method` is `audio` (reCAPTCHA), `checkbox` (hCaptcha/Turnstile), `slider`
  (DataDome) or `press-hold` (PerimeterX). There is **no image mode** — image
  grids and reCAPTCHA v3 are out of scope for the solver.
- `sitekey` carries the **embedder host**, not a sitekey, for the
  hCaptcha-checkbox, DataDome and PerimeterX solvers.
- The domain exists **only** when the browser was launched with
  `--captcha-solver`; otherwise `Chromeleon.enable` is an unknown method. And
  its events are broadcast to every enabled session in the process, so a
  `solverEval` result cannot be correlated to the call that produced it.

## Page completion

`load` fires when the first document's subresources are in, and says nothing
about the text you came for. `Chromeleon.waitForSettle` does: it settles when
the main frame's **rendered text** has been unchanged for a quiet window,
sampled in the browser process over Blink's inner-text channel — no JavaScript
in the page, no isolated world, nothing registered on the document, which is the
point of it over a `MutationObserver` a bot wall can see.

```rust
use chromeleon::settle::{SettleWatch, WaitOptions};

let mut watch = SettleWatch::arm(session).await?;      // BEFORE the navigation
page.goto(url).await?;                                 // committing is enough
let state = watch.wait(WaitOptions::new().timeout_ms(30_000)).await?;

state.outcome        // Settled | Timeout | Challenge | Other(_)
state.via            // WaitForSettle | NetworkAlmostIdle | Load | Timeout
state.blocked()      // a bot wall: the classification callers need
state.elapsed_ms; state.text_length; state.http_status; state.navigations;
watch.close().await;
```

Two phases, not one call, because the ordering mistake is fatal and silent: the
challenge header is recorded at **commit** time by a tracker attached when the
handler is constructed, so a session attached to an already-committed document
cannot see it and degrades to status-code-only detection without saying so. A
one-shot helper called after `goto` would miss everything, so this API cannot
spell one. `BlockingSettleWatch` is the same thing for synchronous drivers; a
complete raw-WebSocket example is `examples/settle_wait.rs`.

**Launch with it on.** The domain is registered only under `--page-settle` —
`Launcher::page_settle(true)`, or `page_settle_tuning(SettleTuning::new()…)` for
the quiet window, min-chars, timeout, sample interval and shadow-piercing
switches. Without it the command is an *unknown method* (`-32601`), not
"settling is off". Like `--captcha-solver` it is deliberately **not** in
`LAUNCH_ARGS`: it changes what the browser does, so it is opt-in.

### Why this order

Measured over 960 navigations, 80 sites, 3 rounds:

| | direct p50 | direct never | proxied p50 | proxied never |
|---|---|---|---|---|
| `networkAlmostIdle` | 3,989ms | 1% | 11,285ms | 36% |
| `Chromeleon.waitForSettle` | 7,306ms | 6% | 12,108ms | 7% |

`waitForSettle` is the primary because **through a proxy** — which is what this
client is for — `networkAlmostIdle` never fires on more than a third of loads,
against 7%. On a direct connection `networkAlmostIdle` is 1.8x faster for the
same median content, which is why it stays available and is the documented
choice for un-proxied work: `WaitOptions::new().prefer_lifecycle(true)`. On
completeness, direct: `waitForSettle` reproduced the final text exactly on 79%
of loads, `networkAlmostIdle` 59%, the load event 40%, `domcontentloaded` 7%.

### What the fallback gets right

When the command is unavailable (`-32601`) or answers something unreadable, the
watch falls back **without raising** — a page that never settles is a normal
outcome on proxied traffic, not an exception — to a main-frame,
non-stale `networkAlmostIdle`, then the `load` event, then `Timeout`. `via` says
which, so a fleet quietly running on the weaker signal is visible in a log line
rather than in a month of thin extractions. Two rules it enforces, both of which
were silent wrong answers before they were rules:

- **The `about:blank` replay is discarded.** `Page.setLifecycleEventsEnabled`
  replays a *complete* lifecycle for the document the tab is already on, so
  arming records every loader seen in its first 350ms as stale. Keep them and
  every fallback signal reads as ~0ms — an instant, wrong "settled".
- **Subframes never answer for the page.** `bbc.com/news` emits lifecycle from
  23 frames; first-across-all reports `networkIdle` at 699ms when the main
  frame's real value is 6094ms. Only `frameId == the main frame` counts, which
  is why a session that cannot name its main frame is refused at **arm** time.

### Blocked is not a slow page

`state.blocked()` is `outcome == Challenge` **or** an
`http_status` in {401, 403, 407, 429, 503} — 12.5% of proxied navigations in
production measurement. Both halves are load-bearing: a wall shorter than
`--page-settle-min-chars` never reaches the `challenge` outcome, it reports
`timeout` with the 403 still on the committed document, and a caller switching
on the outcome alone retries straight back into it.

### Supplying the session

This crate depends on no browser library, so a watch cannot own a CDP session
any more than the handshake can own a connection: implement `PageSession` (or
`BlockingPageSession`) over your driver. Two methods, and the second is where
the runtime choice stays yours:

```rust
impl PageSession for MySession {
    type Error = MyError;                       // only has to absorb chromeleon::Error

    async fn send(&self, method: &'static str, params: Value) -> Result<Value, MyError> {
        self.cdp.call(method, params).await     // result object or whole envelope, either way
    }

    async fn next_event(&self, timeout: Duration) -> Option<(String, Value)> {
        // must be buffered from before the session was handed over, and
        // cancel-safe: a dropped `next_event` must not swallow an event
        tokio::time::timeout(timeout, self.events.lock().await.recv()).await.ok().flatten()
    }
}
```

## API

| | |
|---|---|
| `Launcher` | the browser process: flags merged, `PROXY_*` stripped |
| `proxy_registration(conn, proxy) -> Registration` | hold the slot (also `_blocking`) |
| `with_proxy_registration(conn, proxy, body)` | the same, closure-shaped |
| `new_proxy_context_raw(conn, proxy, send)` | both commands over one `send` |
| `new_proxy_context_with(conn, proxy, send, create)` | when the driver owns context creation |
| `ProxySpec` | a parsed proxy; its server is always normalized |
| `normalize_server`, `parse_proxy`, `check_registration` | the pieces, if you want them |
| `captcha::*` | solver switches, CDP methods, typed events |
| `settle::SettleWatch` / `BlockingSettleWatch` | page completion: arm, then wait (also `LifecycleTracker` on its own) |
| `settle::settle_launch_args`, `Launcher::page_settle` | `--page-settle` and its tuning switches |
| `perf::sticky_geo_env`, `perf::BrowserPool` | launch-latency helpers |
| `oxide::*` (feature) | chromiumoxide adapter: contexts, raw commands, typed events |

`proxy` is anything `IntoProxySpec` accepts: a URL string
(`http://user:pass@host:port`), a `ProxySpec`, or a `(server, username,
password)` tuple. URL credentials are percent-decoded; credentials you pass
separately are literal.

## What this crate refuses to do

Each of these was a silent failure in a real session, which is why it is an
error here and not a best effort:

- **An unnormalized server.** `ProxySpec` can only be built through a
  constructor that normalizes, so the string you register and the string the
  context is created with cannot drift apart.
- **A guessed host.** An empty server, a bad port, or a forbidden character in
  the host is an error. A driver's own fallback quietly turns
  `http://host:notaport` into the host `http`, and the browser then proxies
  nowhere you meant.
- **A guessed credential.** A URL credential whose escapes decode to non-UTF-8
  bytes is `Error::UndecodableCredential`, not a replacement character —
  authenticating with a secret that is *nearly* yours fails at the proxy, not
  here, and looks like a network problem.
- **Dropping credentials.** If you pass a server that embeds `user:pass@` and no
  separate credentials, they are taken from the URL rather than discarded.

## Parity with the Python and Node clients

All three implement `HANDSHAKE.md`, and `../conformance/vectors.json` holds 94
proxy inputs with the answer every client must give. This client passes all 91
that its types can express (the other three are dynamic-typing cases — a null
server, a non-string credential — that cannot be constructed here).

Twenty of those cases are ones where the Python and Node clients **disagree**
with each other. The corpus records who was right and why; this client follows
the ruling, so it deliberately differs from each of the older clients on some
inputs. The `observed` field of each case is the losing behaviour, kept visible
rather than flattened.

## Version-dependent behaviour

Up to v151.4 (the binary published as `latest`) the browser **rejects** a second
registration for a `(connection, server)` while one is pending. From v151.5 it
**queues**, and accepts a `credentialsId` correlation token —
`CredentialsParams::with_credentials_id`, `oxide::new_proxy_context_with_id`,
opt-in because an older binary does not know the field. This client always takes
the lock either way: it cannot know which binary it is talking to, and the lock
costs nothing against a browser that would have queued.

## Test

```sh
cargo test                              # units, corpus, the lock's guarantees
cargo test --features chromiumoxide     # plus the adapter
python3 ../conformance/check.py         # all three clients against one corpus
```

No browser is needed for any of it.
