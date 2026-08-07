# Using Chromeleon

Chromeleon is a patched Chromium that presents a realistic, OS‑coherent browser
fingerprint and removes the usual automation tells. You run the **prebuilt
binary** — there is no SDK or runtime to install — and drive it exactly the way
you would drive headless Chrome: with **Playwright / Puppeteer over CDP**, or by
launching it directly. Fingerprint spoofing, proxy/geo matching, and the run
mode (headless / headed / VNC) are all selected **per launch** with command‑line
flags.

This guide is for the prebuilt Linux tarball
(`Chromeleon-<version>-lin.x86_64.tar.gz` or `…-aarch64.tar.gz`). If you instead
want to build Chromeleon from source, see the project's [README.md](README.md)
and the `build/` docs in the source repository.

> **TL;DR**
> ```bash
> tar xzf Chromeleon-149.0.7827.53.4-lin.x86_64.tar.gz && cd chromeleon-linux
> ./chrome --headless=new --remote-debugging-port=9222 \
>          --remote-allow-origins='*' --fingerprint-os=Windows
> # then from your automation script:
> #   Playwright: chromium.connect_over_cdp("http://localhost:9222")
> ```

## Contents

1. [Install & run](#1-install--run)
2. [Connect & automate (Playwright / CDP)](#2-connect--automate)
3. [Choosing a fingerprint](#3-choosing-a-fingerprint)
4. [Run modes: headless / headed / VNC](#4-run-modes)
5. [Proxies (and the one WebRTC flag you must set)](#5-proxies)
6. [Automatic CAPTCHA solving (opt‑in)](#6-automatic-captcha-solving-opt-in)
7. [Extension emulation & translation (opt‑in)](#7-extension-emulation--translation-opt-in)
8. [Flag & environment reference](#8-flag--environment-reference)
9. [Local‑network policy & troubleshooting](#9-local-network-policy--troubleshooting)

---

## 1. Install & run

The release ships as a single self‑contained tarball:
`Chromeleon-<version>-lin.<arch>.tar.gz`, where `<arch>` is `x86_64` or
`aarch64` (it is chosen at pack time from the host architecture — use the file
that matches your machine).

### 1.1 Extract

```bash
tar xzf Chromeleon-149.0.7827.53.4-lin.x86_64.tar.gz
cd chromeleon-linux
```

The tarball always extracts into a top‑level **`chromeleon-linux/`** directory
(the version and arch live in the `.tar.gz` filename, not the directory name):

```
chromeleon-linux/
├── chrome                 # launch wrapper: sets LD_LIBRARY_PATH=./lib and execs chrome.bin
├── chrome.bin             # the Chromium binary — release builds bake the paks, locales,
│                          #   fonts, persona pool and CAPTCHA model in (self-extracted on first run)
├── chrome-sandbox
├── lib/                   # bundled system libraries (self-contained)
├── docs/                  # CAPTCHA_SOLVER.md, LOCAL_NETWORK_ACCESS.md
├── examples/              # runnable Playwright/CDP/proxy/VNC examples
├── README.md
├── USAGE.md               # this guide
├── Dockerfile
└── docker-entrypoint.sh
```

> Always launch via **`./chrome`** (the wrapper), not `./chrome.bin`. The wrapper
> sets `LD_LIBRARY_PATH=./lib` so the bundled libraries resolve.

### 1.2 Run directly (on the host)

```bash
# Headless, with CDP exposed for Playwright/Puppeteer to attach
./chrome --headless=new --remote-debugging-port=9222 --remote-allow-origins='*'

# Visible, via the built-in VNC backend instead of headless
./chrome --ozone-platform=vnc --vnc-port=5900 \
         --remote-debugging-port=9222 --remote-allow-origins='*'
```

Then point a CDP client at it (e.g. Playwright
`chromium.connect_over_cdp("http://localhost:9222")`).

- `--remote-allow-origins='*'` is required for non‑localhost / cross‑origin CDP
  attach (the Docker entrypoint sets it for you).
- If you use a proxy, also add
  `--webrtc-ip-handling-policy=disable_non_proxied_udp` (see
  [§5 Proxies](#5-proxies)). The Docker entrypoint adds it automatically.

### 1.3 Run via Docker

The `Dockerfile` and `docker-entrypoint.sh` sit at the tarball root, so the
extracted directory **is** the build context.

```bash
cd chromeleon-linux
docker build -t chromeleon .

# Headless (default): entrypoint runs Chrome on internal :9223 and socat-forwards
# it to 0.0.0.0:9222 so it's reachable from the host.
docker run -d --name chromeleon -p 9222:9222 chromeleon

# With a VNC live view
docker run -d --name chromeleon -p 9222:9222 -p 5900:5900 \
  chromeleon --ozone-platform=vnc --vnc-port=5900
```

Connect from the host with `chromium.connect_over_cdp("http://localhost:9222")`
(VNC: `vncviewer localhost:5900`). Any extra args after the image name are
appended to the Chrome command line by the entrypoint — so
`docker run … chromeleon --fingerprint-os=Windows` works. The entrypoint defaults
to `--headless=new` unless `--ozone-platform=vnc` is present, and uses `socat`
to re‑expose CDP on `0.0.0.0:9222` (Chrome binds CDP to `127.0.0.1` and ignores
`--remote-debugging-address` in newer versions).

---

## 2. Connect & automate

There are two ways to drive Chromeleon. For production, prefer **connect over
CDP**: launch the browser once, attach many client sessions to it.

### 2.1 Connect over CDP (Python)

```bash
# Chromeleon must be running with CDP exposed, e.g.:
docker run -d -p 9222:9222 chromeleon
```

```python
import asyncio
from playwright.async_api import async_playwright

async def main(cdp_url="http://localhost:9222"):
    async with async_playwright() as p:
        browser = await p.chromium.connect_over_cdp(cdp_url)
        print(f"Connected! Browser version: {browser.version}")

        # Each new context gets its own fingerprint (and can have its own proxy)
        context = await browser.new_context()
        page = await context.new_page()
        await page.goto("https://demo.fingerprint.com/playground",
                        wait_until="networkidle")
        await context.close()
        await browser.close()

asyncio.run(main())
```

Point `cdp_url` at a remote host (e.g. `http://10.0.0.5:9222`) to attach to a
browser on another machine.

### 2.2 Launch the binary directly (Python / JS)

```python
from playwright.async_api import async_playwright

launch_args = [
    "--disable-blink-features=AutomationControlled",
    "--no-first-run",
    "--webrtc-ip-handling-policy=disable_non_proxied_udp",  # required if proxying
]
# Fingerprint OS is a LAUNCH-TIME choice — see §3
launch_args.append("--fingerprint-os=Windows")

async with async_playwright() as p:
    browser = await p.chromium.launch(
        executable_path="./chrome",
        args=launch_args,
        headless=True,
    )
    context = await browser.new_context(viewport={"width": 1920, "height": 1080})
    page = await context.new_page()
```

```javascript
const { chromium } = require('playwright');

const launchArgs = [
    '--disable-blink-features=AutomationControlled',
    '--no-first-run',
    '--webrtc-ip-handling-policy=disable_non_proxied_udp', // required if proxying
    '--fingerprint-os=Windows',
];
const browser = await chromium.launch({
    executablePath: './chrome',
    args: launchArgs,
    headless: true,
});
const context = await browser.newContext({ viewport: { width: 1920, height: 1080 } });
const page = await context.newPage();
```

> **Note.** `--fingerprint-os` is a launch‑time flag: it is set when the browser
> process starts and **cannot** be changed over CDP after you attach. Runnable
> versions of all of these live in `examples/` (`connect_over_cdp_example.py`,
> `playwright_example.py`, `playwright_example.js`). Prereqs:
> `pip install playwright && playwright install`.

---

## 3. Choosing a fingerprint

A fingerprint (persona) is selected **per launch** and sampled from the bundled
**PersonaPool** — an empirical distribution of real browser fingerprints. The
simplest path is two flags:

| Flag | Values | Effect |
| --- | --- | --- |
| `--fingerprint-os=<OS>` | `Windows`, `macOS`, `Linux` (lowercase `windows`/`mac`/`linux`, and `win`/`win64`, are also accepted) | Target OS to spoof. |
| `--fingerprint-seed=N` | any integer | Deterministic persona: the same seed reproduces the same fingerprint every launch. Omit it and a random persona is sampled each launch. |

```bash
# Spoof Windows, random persona
./chrome --fingerprint-os=Windows about:blank

# Spoof macOS with a fixed, reproducible persona
./chrome --fingerprint-os=macOS --fingerprint-seed=42 about:blank
```

For full control over individual fields (e.g.
`navigator.userAgentData.architecture`) you can deliver a complete persona JSON
instead of letting the binary sample one — set the `CONFIG` environment variable
(or `--cr-cfg-file=<path>` for large configs). This is the production launch
path; the CLI flags above are the convenient front end.

> **Precedence gotcha.** A delivered config (`CONFIG` env, `--cr-cfg-file`,
> `--cr-cfg`) is **authoritative** — `--fingerprint-os/-seed/-browser` will *not*
> override it unless you also set `CHROMELEON_FORCE_REGEN=1`. `--no-fingerprint`
> (or `NO_FINGERPRINT=1`) disables spoofing entirely and beats everything.
>
> **macOS config size.** On Linux the `CONFIG` env var must stay under ~120 KB.
> Large personas (notably macOS, ~135 KB) must be delivered via
> `--cr-cfg-file=<path>` or child processes fail to spawn.

---

## 4. Run modes

All three modes use the same binary; they differ only by which flags you pass.

**Headless (default for automation).** In Docker the entrypoint adds
`--headless=new` automatically (unless VNC is requested) and exposes CDP on
`:9222`. Driving the raw binary, pass it yourself:

```bash
./chrome --headless=new --remote-debugging-port=9222 --remote-allow-origins='*'
```

**Headed.** Launch without `--headless` on a machine with a display. Useful for
local debugging when you want a real window.

**VNC (built‑in server, for live viewing).** Watch and interact with the browser
in real time via any VNC viewer while still controlling it over CDP:

```bash
# Local binary: VNC on 5900, CDP on 9222
./chrome --ozone-platform=vnc --vnc-port=5900 \
         --remote-debugging-port=9222 --remote-allow-origins='*' \
         --no-sandbox --disable-gpu about:blank

# Docker: publish both ports
docker run -d -p 9222:9222 -p 5900:5900 chromeleon \
    --ozone-platform=vnc --vnc-port=5900 --remote-debugging-port=9222

# Then connect any VNC viewer:
vncviewer localhost:5900
```

`--vnc-port` defaults to `5900`; `--vnc-password=<pw>` adds a password (default
is open). VNC and headless are **mutually exclusive** — requesting VNC gives you
a visible session, not a headless one. A runnable end‑to‑end example (launch with
VNC, drive over CDP, keep the session alive for a viewer) is in
`examples/vnc_example.py`.

---

## 5. Proxies

Chromeleon attaches proxies **per browser context** via Playwright's standard
`new_context(proxy={...})`. Each context can use a different proxy, and
Chromeleon automatically generates a **matching fingerprint** (timezone, locale,
language) from the proxy's exit‑IP geolocation. For an authenticated
per-context proxy, first register its credentials with the browser-level
`Target.setProxyCredentials` command, then call `new_context` with the exact
same `server` string and **no credentials**. This is the only supported
credential-delivery path.

> **Use the client library rather than hand-rolling this.** Playwright has no
> API shaped like the two-step handshake, and the natural call —
> `launch(proxy={server, username, password})` — does *not* perform it. That
> produces a browser whose exit IP was never resolved, so the persona's geo
> never binds and WebRTC masking is disabled. `chromeleon` does the
> handshake, serializes the register-then-create pair, sets the WebRTC policy,
> strips controller-side `PROXY_*` variables, and refuses launch-time
> credentials outright:
>
> ```python
> from playwright.sync_api import sync_playwright
> from chromeleon import launch, new_proxy_context
>
> with sync_playwright() as p:
>     browser = launch(p.chromium, executable_path=CHROMELEON)
>     context = new_proxy_context(browser, "http://user:pass@gateway:12321")
>     page = context.new_page()
> ```
>
> `new_proxy_context_async` is the asyncio twin. See
> `examples/per_context_proxy_example{,_sync}.py`.

### Unauthenticated proxy at launch

If every context uses the same unauthenticated upstream proxy, the ordinary
Chromium launch switch remains available:

```bash
chrome --proxy-server=http://host:port
```

Launch-time credentials are intentionally unsupported: credentials embedded in
`--proxy-server` and the retired `--proxy-auth` switch are rejected. For an
authenticated HTTP/HTTPS proxy, launch without a proxy, preregister with
`Target.setProxyCredentials`, and create a server-only context as shown above.
There is no process-wide credential fallback.

Passing the proxy at launch is also what lets Chromeleon resolve the exit IP for
WebRTC masking and geo/timezone coherence, and auto‑add
`--webrtc-ip-handling-policy=disable_non_proxied_udp`. If you already know the
exit IP, pass `--cr-exit-ip=<ip>` (or `CHROMELEON_EXIT_IP`) to skip the probe.

### Required per-context credential preregistration

Authenticated HTTP/HTTPS per-context proxies must register credentials before
creating a context. Chromeleon resolves the exit IP and generates the matching
persona before the `BrowserContext` is created:

```text
Target.setProxyCredentials {proxyServer, username, password}
Target.createBrowserContext {proxyServer}
```

Each matching `createBrowserContext` consumes one registration exactly once.
The command has no correlation token, so only one registration may be
outstanding for a given root connection and `proxyServer`. Callers sharing an
endpoint **must serialize each set-then-create pair**; a second registration is
rejected until the first is consumed.

Both commands must run on the same root CDP connection. A child session created
by `Target.attachToBrowserTarget` shares that root and may register before the
root creates the context; page-target sessions are not browser-level and cannot
call this command. A registration made on one independent DevTools WebSocket is
intentionally invisible to another. `proxyServer` matching is exact, including
scheme and spelling, and each root connection may hold at most 128 pending
registrations across distinct endpoints; the 129th call fails explicitly.
Pending entries live until they are consumed or that root connection closes.

Preregistered first-flight authentication uses Chromium's per-NetworkContext
HTTP auth cache and supports Basic authentication for HTTP and HTTPS proxies,
including HTTPS/WebSocket CONNECT. SOCKS username/password preregistration is
not supported. Proxy routing and the raw credential URL are retained for
NetworkService reconstruction while the creating browser-level Target handler
remains alive. Clients must keep that root CDP connection alive for the context
lifetime (or use `disposeOnDetach`); a context deliberately kept alive after
the owning Target session is torn down (for example, after root disconnect)
cannot be rehydrated by that handler.

Security note: the browser-side exit-IP probe currently invokes the trusted OS
`curl` with proxy credentials in its child-process argument vector. They are
redacted from Chromeleon logs, but a same-container process with permission to
inspect `/proc/<pid>/cmdline` can observe them during the short probe lifetime.
Run untrusted workloads in separate containers; moving probe credentials to a
protected config/file descriptor is tracked as follow-up hardening.

Do not put `username` or `password` in Playwright's `new_context(proxy=...)`,
and do not embed userinfo in `proxyServer`. A missing registration, an inexact
`proxyServer` match, invalid credentials, or failed exit-IP/persona resolution
rejects `createBrowserContext` before a browser context is allocated. There is
no passive `407` credential-capture fallback and no fail-open/fail-closed
navigation route.

### ⚠️ Read first: WebRTC leaks your real IP unless you set one flag

HTTP and SOCKS proxies **do not tunnel UDP**. When you attach a proxy, WebRTC's
ICE gathering still sends STUN over raw UDP from the host interface, and the
resulting `srflx` candidate exposes your **real public IP** — even though HTTP
traffic is proxied. Every time you use a proxy, launch with:

```
--webrtc-ip-handling-policy=disable_non_proxied_udp
```

This flag is **already the default** in the bundled `docker-entrypoint.sh`, so a
Dockerized browser is covered. If you launch the binary yourself, you must add
it yourself. **Verify** at `https://browserleaks.com/webrtc`: the "Public IP" row
must show the proxy exit, not your machine/datacenter IP.

### Per‑context proxy (Python, async)

```python
from playwright.async_api import async_playwright

async with async_playwright() as p:
    # REQUIRED whenever a proxy is used (see above).
    browser = await p.chromium.launch(
        executable_path="./chrome",
        args=["--webrtc-ip-handling-policy=disable_non_proxied_udp"],
    )
    proxy_server = "http://residential.example.net:12321"
    cdp = await browser.new_browser_cdp_session()
    try:
        await cdp.send("Target.setProxyCredentials", {
            "proxyServer": proxy_server,
            "username": "myuser",
            "password": "mypass_country-jp_session-abc123",
        })
    finally:
        await cdp.detach()

    # Reuse the exact server string; credentials do not go in new_context().
    context = await browser.new_context(proxy={"server": proxy_server})
    page = await context.new_page()
    # -> this context now reports Asia/Tokyo timezone, ja-JP locale, etc.
```

The same register-then-create pattern works with the sync API
(`new_browser_cdp_session`) and Node (`newBrowserCDPSession`), and after
`connect_over_cdp(...)` — as long as the server-side process was launched with
the WebRTC flag. Serialize every registration with its immediately following
context creation. Runnable versions:
`examples/per_context_proxy_example.py` and `…_sync.py`.

> **Notes.** A single browser can host many contexts, each on its own proxy with
> its own geo‑matched fingerprint. The geo match only appears if the exit IP
> resolves — if the locale stays `en-US`, proxy resolution likely failed.
> Country/session targeting in these examples is encoded in the proxy
> **password** (e.g. `…_country-jp_session-abc123`), a provider convention, not a
> Chromeleon API.

---

## 6. Automatic CAPTCHA solving (opt‑in)

Chromeleon ships with an embedded CAPTCHA auto‑solver. It is **OFF by default**
and enabled with the `--captcha-solver` launch flag. When on, it detects
supported challenges on every page load and works them transparently in the
background — your script doesn't need to do anything special. Solving runs at the
browser‑process level (not via CDP), so it never steals focus from or interleaves
input with your own automation.

**Scope & honesty:**

- **Supported challenge types.** A single `--captcha-solver` arms solvers for
  **reCAPTCHA v2 / v2 Enterprise** (audio transcription), **Cloudflare
  Turnstile**, **hCaptcha** checkbox, **DataDome** slider, and **PerimeterX**
  press‑and‑hold. It does **not** solve **reCAPTCHA v3** (score‑based — there is
  no challenge to interact with) or **image‑grid** challenges.
- **Best‑effort.** Success varies by challenge type and IP reputation; reCAPTCHA
  audio is the most battle‑tested path. The reCAPTCHA solver retries up to 5
  times (~22–30 s typical) and can still give up
  (`reason: "max_attempts_reached"`). A clean/residential proxy materially
  improves success — Google (and other vendors) rate‑limit per IP.
- **The reCAPTCHA audio model ships with the binary.** The `whisper-cli` binary,
  a reCAPTCHA‑distilled model (`ggml-base.en-recaptcha.bin`, ~141 MB — Whisper
  base.en fine‑tuned on real reCAPTCHA audio, which the solver **prefers**), and
  the stock `ggml-small.en.bin` (466 MB, automatic fallback) are embedded and
  auto‑extracted to `~/.cache/clm/v<version>/captcha_models/` on first launch —
  no setup needed. Both `x86_64` and `aarch64` builds include them. (The
  Turnstile / hCaptcha / DataDome / PerimeterX solvers are interaction‑based and
  need no model.)

### Enabling it

The client's `launch` takes a first‑class `captcha=True` (it appends
`--captcha-solver` for you; the models are embedded, so nothing else is needed):

```python
from chromeleon import launch

browser = launch(p.chromium, "./chrome", captcha=True, headless=True)
```

or pass the flag yourself — `chromium.launch(..., args=["--captcha-solver"])`.
`captcha_model_path="<dir>"` is a dev/self‑host override and implies `captcha`.
A clean/residential proxy is recommended; attach it per context with
`new_proxy_context`.

### Observing the solve (optional)

The solver emits lifecycle events on a custom **`Chromeleon`** CDP domain
(visible only to the DevTools session, invisible to the page). `enable_captcha`,
`disable_captcha`, and the `CAPTCHA_*` event constants save you the string
literals:

```python
from chromeleon import enable_captcha, CAPTCHA_SOLVED, CAPTCHA_FAILED

cdp = await page.context.new_cdp_session(page)
await enable_captcha(cdp)

solved = asyncio.Future()
cdp.on(CAPTCHA_SOLVED, lambda p: solved.set_result(p))
cdp.on(CAPTCHA_FAILED, lambda p: solved.set_exception(Exception(p["reason"])))

await page.goto("https://example.com/login")
result = await solved
print(f"Solved in {result['timeMs']:.0f}ms, {result['attempts']} attempt(s)")
```

Events: `captchaDetected`, `captchaSolving` (`method` — `"audio"`, `"checkbox"`,
`"slider"`, or `"press-hold"`), `captchaSolved` (`attempts`, `timeMs`),
`captchaFailed` (`reason`). For reCAPTCHA you can also just poll for the response
token — `textarea[name=g-recaptcha-response]` becomes non‑empty when the solve
lands.

**`solver_eval` — read inside a challenge.** For custom flows that need to read
state a captcha renders behind a **closed** shadow root (which normal
`page.evaluate` can't pierce), `solver_eval(cdp, expression, frame_url_contains="")`
runs JS in the solver's isolated world; the string result arrives as a
`Chromeleon.solverEvalResult` event (pass a URL substring to target a
cross‑origin subframe):

```python
from chromeleon import solver_eval, SOLVER_EVAL_RESULT

cdp.on(SOLVER_EVAL_RESULT, lambda p: print("eval:", p["result"]))
await solver_eval(cdp, "document.querySelector('#challenge')?.shadowRoot?.innerHTML ?? ''")
```

Full examples: `examples/captcha_solver_example.py` and `…_sync.py`; details in
`docs/CAPTCHA_SOLVER.md`.

> `--captcha-model-path=<dir>` overrides the *directory* searched, not the model
> (it prefers `ggml-base.en-recaptcha.bin`, falling back to `ggml-small.en.bin`,
> alongside `whisper-cli`). You normally don't need it — the models are embedded.

---

## 7. Extension emulation & translation (opt‑in)

All controls in this section are **off / empty by default** — Chromeleon emulates
no extensions and translates nothing until you turn them on. Flag names verified
against the shipped binary.

### 7.1 Extension emulation

The emulation *category* is on by default, but it emulates **nothing** until you
name specific extension slugs. Point `extensions_list` at the slugs you want.

| Knob | Form | Values | Default | Effect |
|---|---|---|---|---|
| `SPOOFING_CONFIG` key `extensions` | env / JSON | `true` \| `false` | `true` | Master on/off for the category. |
| `SPOOFING_CONFIG` key `extensions_list` | env / JSON | array of slug strings | `[]` (not auto‑filled) | **Which** extensions are emulated this session. |
| `--spoofing=<json>` | flag | inline JSON | unset | Inline form; launcher re‑exports it to `SPOOFING_CONFIG` for renderers. |
| `--spoofing-config=<path>` | flag | path to JSON file | unset | File form. |
| `--no-extension-emulation` | flag | presence | absent | Dedicated kill switch. |
| `--stealth-extension=<id>[,<id>…]` | flag | comma‑sep 32‑char IDs | empty | **Hides** the given real/emulated extension IDs from page detection. |

**Valid slugs (22):** `grammarly`, `metamask`, `phantom`, `coinbase-wallet`,
`lastpass`, `1password`, `bitwarden`, `honey`, `dark-reader`, `google-translate`,
`adobe-acrobat`, `react-devtools`, `languagetool`, `tampermonkey`,
`pinterest-save`, `cisco-webex`, `zoom`, `save-to-pocket`,
`capital-one-shopping`, `trust-wallet`, `redux-devtools`, `vue-devtools`.

```bash
# Emulate three extensions:
export SPOOFING_CONFIG='{"default":true,"extensions":true,"extensions_list":["metamask","grammarly","dark-reader"]}'
#   ...or inline:  chromeleon --spoofing='{"extensions":true,"extensions_list":["metamask"]}'
# Off:            chromeleon --no-extension-emulation
# Hide one ID:    chromeleon --stealth-extension=nkbihfbeogaeaoehlefnkodbefgpgknn
```

> **Default is 0 emulated.** Nothing auto‑populates `extensions_list` in the
> shipped binary — you must supply it. Dynamic‑URL slugs (`bitwarden`,
> `pinterest-save`) emulate with a detectable static‑id 200; prefer the others.

### 7.2 In‑image (photo / OCR) translation

Runs OCR (ScreenAI) on text **inside `<img>` images**, translates it, and paints
a translated overlay back over the image. Not DOM/page‑text translation (§7.3).

| Knob | Form | Values | Default | Effect |
|---|---|---|---|---|
| `--enable-features=StealthTranslate` | feature | literal | disabled | **Master on‑switch** (required). |
| `--disable-features=ImageTranslate` | feature | — | follows umbrella | Turn off just the image path. |
| `--stealth-translate-to=<bcp47>` | flag | e.g. `es`, `fr`, `ja` | UI locale | Target language (no‑op without the master switch). |
| `--stealth-translate-backend=<b>` | flag | `google` \| `ondevice` | `google` | `google`=cloud/all langs; `ondevice`=bundled packs, English source only. |
| `CHROMELEON_SCREENAI_DIR=<dir>` | env | path to OCR resources | unset | **Required** (dev provisioning) — no OCR ⇒ no‑op. |

```bash
CHROMELEON_SCREENAI_DIR=/opt/chromeleon/screen_ai \
  chromeleon --enable-features=StealthTranslate --stealth-translate-to=es https://example.com
```

> Only decoded `<img>` ≥ 64×64 px (not CSS backgrounds / `<canvas>` / `<video>`).
> `google` egresses via the **system** network context, bypassing the per‑page
> proxy. Overlay is paint‑only, reset per navigation.

### 7.3 Dual‑tree page‑text translation

Translates **on‑page (DOM) text** by substituting glyphs at their original
positions during paint — DOM, layout, and script‑observable style stay
bit‑identical. No launcher flag; driven by the feature gate + a renderer CDP
domain.

| Knob | Form | Values | Default | Effect |
|---|---|---|---|---|
| `--enable-features=DualTreeTranslate` | feature | presence | off | Master gate. |
| `…DualTreeTranslate:map/<hex>` | feature param | hex JSON `{"src":"dst"}` | `""` | Static translation map (no‑CDP path). |
| `…DualTreeTranslate:target/<script>` | feature param | `latin`/`han`/`arabic`/`cyrillic`/`devanagari`/`hangul` | `latin` | Stub/demo cipher (no real translation). |
| `DualTree.setDocumentTranslationMap` | CDP | `{frameId, entries:[{original,translated}]}` | — | Production dynamic path; precedence over `map/`. |
| `DualTree.clearDocumentTranslationMap` | CDP | `{frameId}` | — | Clear + repaint to original. |

```bash
# Enable the gate, then push a map over CDP:
chromeleon --enable-features=DualTreeTranslate --remote-debugging-port=9222
#   CDP: DualTree.setDocumentTranslationMap {frameId, entries:[{original:"Hello",translated:"Bonjour"}]}
```

> With the gate on but **no map**, it enters stub‑cipher mode (garbles text into
> the target script) — not real translation; feed it a `map/` or a CDP map.
> This is separate from §7.2 — `--stealth-translate-*` do **not** enable it.

### 7.4 Other adjustable flags

| Flag | Values | Default | Effect |
|---|---|---|---|
| `--fingerprint-platform=<os>` | `Windows`/`macOS`/`Linux` | — | Alias for `--fingerprint-os`. |
| `--fingerprint-locale=<bcp47>` / `--fingerprint-timezone=<iana>` | tag / IANA | auto | CLI forms of the geo env vars; presence suppresses IP geolocation. |
| `--no-ip-geolocation` | presence | off | Skip exit‑IP geo/timezone autodetect. |
| `--fingerprint-data=<path>` | path | bundled | Override the persona/network data file. |
| `--allow-passkey-ui` | presence | off | Opt back into the native WebAuthn/passkey prompt (default auto‑cancels it). |
| `--scrollbar-overlay=<true\|1>` | `true`/`1` | persona | Force overlay‑scrollbar spoofing (license‑gated). |

> **Not shipped:** there are no `humanClick`/`humanMove`/`humanType` CDP
> commands. Input humanization (Markov key‑timing, humanized mouse paths) is
> transparent and always‑on over the standard CDP `Input` domain — no toggle.

---

## 8. Flag & environment reference

Every entry below was verified against the v149 source. A few flags
(`--proxy-server`, `--headless`, `--remote-debugging-port`, `--ozone-platform`)
are **stock Chromium** switches that Chromeleon reacts to; the rest are
Chromeleon‑specific.

### Fingerprint / persona

| Flag / env | What it does | Default |
|---|---|---|
| `--fingerprint-os=<os>` | Spoofed OS persona (`Windows`/`macOS`/`Linux`). Mismatch vs a delivered config aborts launch. | auto‑sampled |
| `--fingerprint-seed=<uint64>` | Deterministic persona (same seed ⇒ same fingerprint). | random/launch |
| `--fingerprint-browser=<brand>` | Browser brand persona (`Chrome`, `Edge`, `Opera`). | sampled |
| `--no-fingerprint` / `NO_FINGERPRINT=1` | Disable ALL spoofing (beats a delivered config). | off (spoofing on) |
| `--spoofing=<json>` | Per‑category enable/disable, e.g. `'{"default": false}'`. | all on |
| `--dump-fingerprint-config` | Print the generated persona JSON and exit. | off |
| `CHROMELEON_FORCE_REGEN=1` | Discard an inherited `CONFIG` and regenerate from `--fingerprint-*`. | unset |

### Delivering a full persona (CONFIG JSON)

| Flag / env | What it does | Default |
|---|---|---|
| `CONFIG=<json>` (env) | Full persona JSON, used verbatim (authoritative). Keep < ~120 KB on Linux. | unset |
| `--cr-cfg-file=<path>` | Load persona JSON from a file (preferred for large/macOS configs). | none |
| `--cr-cfg=<json>` | Inline persona JSON (small configs only). | none |
| `CONFIG_VAR_NAME=<name>` (env) | Read the config from a differently‑named env var. | `CONFIG` |

### Proxy / exit‑IP / WebRTC

| Flag / env | What it does | Default |
|---|---|---|
| `--proxy-server=<url>` | *(stock)* Unauthenticated launch-wide proxy. Credential-bearing values are rejected; use `Target.setProxyCredentials` plus a server-only context for authenticated HTTP/HTTPS proxies. Chromeleon resolves the exit IP → geo/timezone and auto-adds the WebRTC flag. | direct |
| `--webrtc-ip-handling-policy=disable_non_proxied_udp` | *(stock)* **Set this whenever you use any proxy** — prevents the real‑IP WebRTC leak. | — |
| `CHROMELEON_EXIT_IP=<ip>` / `--cr-exit-ip=<ip>` | Supply the exit IP directly; skips network probes. | probe |

### Geo / timezone / locale

| Flag / env | What it does | Default |
|---|---|---|
| `CHROMELEON_TARGET_TZ=<tz>` | Pin the timezone, skip the geo cascade. | auto from exit IP |
| `CHROMELEON_TARGET_LOCALE=<locale>` / `CHROMELEON_TARGET_COUNTRY=<cc>` | Steer persona selection. | auto |
| `CHROMELEON_NO_GEO_AUTODETECT=1` | On a direct (no‑proxy) launch, skip the own‑IP geo lookup (instant startup). | lookup runs |

### CAPTCHA / local network / display

| Flag / env | What it does | Default |
|---|---|---|
| `--captcha-solver` | Enable the embedded CAPTCHA solver (reCAPTCHA v2/Enterprise audio + Turnstile / hCaptcha / DataDome / PerimeterX; not v3 or image‑grid). | disabled |
| `--captcha-model-path=<dir>` | Override the model directory (not the model size). | embedded |
| `--chromeleon-allow-local-network` / `CHROMELEON_ALLOW_LOCAL_NETWORK=1` | Disable the default‑deny LAN/loopback block. | block on |
| `--ozone-platform=vnc` *(stock)* + `--vnc-port=<n>` / `--vnc-web-port=<n>` / `--vnc-password=<pw>` | Built‑in VNC backend (default port `5900`, web `5800`, no password). | platform default |
| `--headless` *(stock)* / `--remote-debugging-port=<n>` *(stock)* | Headless mode / CDP endpoint for drivers. | off |

### Advanced / diagnostic

| Flag / env | What it does | Default |
|---|---|---|
| `--cr-license-check` | Resolve the per‑org identity, print the verdict (and on refusal the reason + remedy), exit. No network, no browser start. | — |
| `--license-key=<id>` / `_CLK=<id>` (env) | License token override (normally embedded per‑customer). | embedded |
| `--no-crash-reporting` | Disable crash upload to the license server. | reporting on |
| `--canvas-seed=<int64>` / `--no-readpixels-noise` | Deterministic / disabled canvas readback noise. | derived / on |
| `--font-allowlist=<csv>` | Restrict the spoofed font set. | persona set |

---

## 9. Local‑network policy & troubleshooting

### Default‑deny on local network

Chromeleon **blocks cross‑origin requests to loopback and LAN addresses by
default** (an anti‑fingerprinting measure that defeats `127.0.0.1:3389` /
`192.168.x.x` WebRTC port probes). `192.168.1.1` is allowlisted so you look like
a typical home router. If your own automation legitimately needs to reach
loopback/LAN, opt in with `--chromeleon-allow-local-network` (or
`CHROMELEON_ALLOW_LOCAL_NETWORK=1`). Full policy:
[docs/LOCAL_NETWORK_ACCESS.md](docs/LOCAL_NETWORK_ACCESS.md) (ships in the
tarball's `docs/`).

### "License check failed" in a container

Licensing is possession‑based: the per‑org `bundle/org.dat` shipped in your
tarball **is** the license, and it is read locally — no network call is made, so
proxies, egress rules and firewalls are never the cause. The browser accepts it
only when all of the following hold, and every failure looks identical from the
outside:

- it is a **regular file** (symlinks are followed, so Kubernetes ConfigMap /
  Secret projections work);
- it is owned by **the uid the browser runs as, or by root** — `root:root` is
  the robust choice, since it is accepted no matter which uid the container
  ends up running as;
- it is **not group‑ or other‑writable** (`0644` is right, `0664` is refused);
- every directory above it is **traversable** by that uid.

Ask the binary rather than guessing:

```bash
/opt/chromeleon/chrome --cr-license-check
```

It prints which source the identity came from, the path it consulted, the
running euid, and — when refused — the reason and the exact remedy. The same
detail lands in the support bundle written next to the failure
(`<cache>/cr/diag/<support-code>.txt`) as `org_source` / `org_reject`.

The two that bite in practice: an image built as one uid but run as another
(`chown 0:0 /opt/chromeleon/bundle/org.dat`), and an install directory that the
runtime user cannot traverse (`chmod -R a+rX /opt/chromeleon`).

### Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| `License check failed` / `kInvalid` in the diag | Run `--cr-license-check`; see the section above. Almost always org.dat ownership or directory traversal, never the network. |
| CDP client can't attach from another host | Add `--remote-allow-origins='*'` (Docker sets it). In Docker, publish `-p 9222:9222`. |
| `browserleaks.com/webrtc` shows your real IP | The WebRTC flag is missing in the launch path. Add `--webrtc-ip-handling-policy=disable_non_proxied_udp`. |
| Per-context proxy creation says credentials are missing or mismatched | Send `Target.setProxyCredentials` on a browser CDP session immediately before `new_context`, reuse the exact `proxyServer` spelling, and pass only `{"server": proxy_server}` to Playwright. |
| Per-context proxy context creation fails during resolution | Verify the proxy credentials and egress. Chromeleon resolves the exit IP and persona before allocating the context; there is no passive `407` fallback. |
| Child processes fail to spawn (`E2BIG`) on a macOS persona | `CONFIG` exceeds the ~120 KB env limit — deliver it via `--cr-cfg-file=<path>`. |
| Spoofed OS doesn't change | A delivered `CONFIG` / `--cr-cfg-file` is authoritative — add `CHROMELEON_FORCE_REGEN=1` to let `--fingerprint-os` override it. |
| CAPTCHA solver does nothing | It's opt‑in — launch with `--captcha-solver`. It handles reCAPTCHA v2/Enterprise, Turnstile, hCaptcha, DataDome, PerimeterX (not reCAPTCHA v3 or image‑grid), and a clean IP helps a lot. |
| Library load error launching `./chrome.bin` | Launch via the `./chrome` wrapper instead — it sets `LD_LIBRARY_PATH=./lib`. |

### More

- **Runnable examples:** `examples/` (Playwright/CDP, per‑context proxies,
  CAPTCHA solver, VNC).
- **CAPTCHA details:** `docs/CAPTCHA_SOLVER.md`.
- **Local‑network policy:** `docs/LOCAL_NETWORK_ACCESS.md`.
