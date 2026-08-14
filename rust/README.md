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
