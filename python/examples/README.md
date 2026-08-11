# Chromeleon Examples & Test Suite

## Quick Start

```bash
# Install dependencies
pip install -r requirements.txt

# Install playwright browsers (one-time setup)
playwright install chromium
```

## Basic Usage with Playwright

```bash
# Basic usage
python playwright_example.py --chromeleon-path ./chrome

# Headless mode
python playwright_example.py --chromeleon-path ./chrome --headless

# Run fingerprint detection tests
python playwright_example.py --chromeleon-path ./chrome --headless --test
```

## Playwright Example Options

| Option | Description |
|--------|-------------|
| `--chromeleon-path PATH` | Path to chrome binary (required) |
| `--headless` | Run in headless mode |
| `--proxy URL` | Proxy server (e.g., `http://user:pass@host:port`) |
| `--fingerprint-os OS` | Spoof OS: `windows`, `linux`, or `macos` |
| `--test` | Run fingerprint detection tests on multiple sites |
| `--url URL` | Visit a specific URL |

## reCAPTCHA Auto-Solver

Chromeleon includes an embedded reCAPTCHA v2/Enterprise solver. Enable it with `--captcha-solver`.

```bash
# Async example with CDP event tracking:
python captcha_solver_example.py --chromeleon-path ./chrome --cdp-events

# Sync example (minimal):
python captcha_solver_example_sync.py --chromeleon-path ./chrome

# With proxy (recommended for clean IP):
python captcha_solver_example.py --chromeleon-path ./chrome --proxy http://user:pass@host:port
```

See [docs/CAPTCHA_SOLVER.md](../docs/CAPTCHA_SOLVER.md) for full documentation including CDP events, setup, and troubleshooting.

## Pytest Test Suite

The `tests/` directory contains the comprehensive pytest test suite for validating Chromeleon against various antibot and fingerprinting services.

### Running Tests

```bash
# Set the Chrome path environment variable
export CHROME_EXECUTABLE=/path/to/chromeleon/chrome

# Run all antibot tests
pytest tests/antibots/ --headless

# Run specific tests
pytest tests/antibots/fingerprintjs_test.py --headless
pytest tests/antibots/creepjs_test.py --headless

# Run with fingerprint OS spoofing
pytest tests/antibots/fingerprintjs_test.py --headless --fingerprint-os windows

# Run with proxy (requires PROXY_USER_PASS or FALLBACK_PROXY_USER_PASS env var)
pytest tests/antibots/fingerprintjs_test.py --headless --proxy

# Run captcha solver tests
pytest tests/antibots/captcha_solver_test.py --captcha-solver
pytest tests/antibots/captcha_solver_test.py --captcha-solver --proxy

# Show verbose fingerprint/proxy logging
pytest tests/antibots/ --headless --show-fp

# Run in parallel (faster)
pytest tests/antibots/ --headless -n auto

# Save screenshots on failure
pytest tests/antibots/ --headless --screenshot
```

### Pytest Options

| Option | Description |
|--------|-------------|
| `--headless` | Run browser in headless mode |
| `--fingerprint-os OS` | Target OS for fingerprint: `Windows`, `macOS`, `Linux` |
| `--fingerprint-seed N` | Use specific seed for deterministic fingerprints |
| `--proxy` | Enable proxy support (requires env vars) |
| `--strict-fingerprint` | **Strict mode**: Lock fingerprint across retries (see below) |
| `--validate-seeds` | **Validation mode**: Track seed pass/fail for fingerprint validation (see below) |
| `--show-fp` | Verbose logging for fingerprint/proxy details |
| `--screenshot` | Save screenshots on test failure |
| `--no-fingerprint` | Disable fingerprint spoofing |
| `--captcha-solver` | Enable reCAPTCHA auto-solver (audio-based, whisper.cpp) |
| `--captcha-model-path PATH` | Path to whisper models (default: `$CAPTCHA_MODEL_PATH`) |
| `--chromium-logging` | Enable Chromium internal logging |
| `-n auto` | Run tests in parallel (pytest-xdist) |

### Strict Fingerprint Mode

Strict mode ensures the **exact same fingerprint** is used across all retries:

```bash
# Run with strict mode - failures indicate real fingerprint issues
pytest tests/antibots/fingerprintjs_test.py --headless --proxy --strict-fingerprint
```

**What strict mode does:**
- Locks the fingerprint seed across all retries
- Caches the first proxy and reuses it (same timezone = same fingerprint)
- Disables "no-proxy fallback" behavior

**Why use strict mode:**
- Normal mode may mask fingerprint issues by retrying with different proxies/fingerprints
- Strict mode ensures failures are due to actual fingerprint detection, not network issues
- Use for debugging and validating fingerprint quality

### Fingerprint Generation Validation Mode

Validates that **multiple different fingerprints** all work, not just one lucky seed:

```bash
# Enable validation mode - tracks which seeds pass/fail
pytest tests/antibots/fingerprintjs_test.py --headless --proxy --validate-seeds

# Run multiple tests - each gets a different random seed
pytest tests/antibots/fingerprintjs_test.py tests/antibots/overpoweredjs_test.py \
  --headless --proxy --validate-seeds

# Use pytest-repeat to run same test 5 times with different seeds
pip install pytest-repeat
pytest tests/antibots/fingerprintjs_test.py --headless --proxy --validate-seeds --count=5
```

**What validation mode does:**
- Caches the first working proxy (same proxy for all tests)
- Generates a **different random seed** for each test
- Tracks which seeds passed/failed
- Reports seed validation summary at the end
- Disables reruns (each seed gets one fair attempt)
- **Fails if ANY seed fails** (all fingerprints must pass)

**Why use validation mode:**
- Ensures your fingerprint generation is robust, not dependent on one lucky seed
- Identifies if certain fingerprints are detected while others pass
- Helps debug fingerprint generation issues

**Example output (all pass):**
```
SEED VALIDATION SUMMARY
======================================================================
Seeds tested: 5
Seeds passed: 5
Seeds failed: 0

✅ Passed seeds:
   123456789
   987654321
   555555555
   111111111
   777777777

🎉 All 5 fingerprints passed validation!
RESULT: PASS
======================================================================
```

**Example output (some fail):**
```
SEED VALIDATION SUMMARY
======================================================================
Seeds tested: 5
Seeds passed: 4
Seeds failed: 1

✅ Passed seeds:
   123456789
   987654321
   555555555
   111111111

❌ Failed seeds:
   999999999

⚠️  1/5 fingerprints failed - investigate these seeds
RESULT: FAIL
======================================================================
```

### Mode Comparison

| Behavior | Normal Mode | Strict Mode | Validation Mode |
|----------|-------------|-------------|-----------------|
| Fingerprint seed | Random each attempt | Same across retries | **Different each test** |
| Proxy | New each retry | Same proxy reused | **Same proxy for all** |
| Reruns on failure | Yes | Yes (same fingerprint) | **No** (fair test) |
| Use case | Production testing | Debug specific fingerprint | **Validate generation** |

### Test Categories

**Antibot Tests** (`tests/antibots/`):
- `fingerprintjs_test.py` - FingerprintJS Pro detection
- `creepjs_test.py` - CreepJS fingerprint consistency
- `browserscan_test.py` - BrowserScan bot detection
- `pixelscan_test.py` - PixelScan fingerprint analysis
- `cloudflare_test.py` - Cloudflare Turnstile
- `datadome_test.py` - DataDome protection
- `kasada_test.py` - Kasada bot detection
- `akamai_test.py` - Akamai Bot Manager
- `perimeterx_test.py` - PerimeterX/HUMAN
- `incapsula_test.py` - Imperva/Incapsula
- And more...

**Fingerprint Tests** (`tests/fp/`):
- `canvas_test.py` - Canvas fingerprinting
- `webgl_test.py` - WebGL fingerprinting
- `audio_test.py` - Audio fingerprinting
- `navigator_test.py` - Navigator properties
- `screen_window_test.py` - Screen/window dimensions
- `useragentdata_test.py` - User-Agent Client Hints
- And more...

### Environment Variables

```bash
# Chrome binary path (alternative to setting it per-test)
export CHROME_EXECUTABLE=/path/to/chromeleon/chrome

# Proxy configuration (for --proxy flag)
export PROXY_USER_PASS=username:password
export FALLBACK_PROXY_USER_PASS=username:password
export PROXY_COUNTRY=us

# Fingerprint configuration
export FINGERPRINT_OS=Windows  # or macOS, Linux

# Run in headless mode by default
export HEADLESS=true
```

## WebRTC and Proxies — READ FIRST

**Any time you use a proxy with Chromeleon, launch with:**

```
--webrtc-ip-handling-policy=disable_non_proxied_udp
```

### Why

HTTP and SOCKS proxies in Chromium do not tunnel UDP. When you attach a proxy
via `new_context(proxy=...)` (per-context) — or even via `--proxy-server`
alone — WebRTC's ICE gathering still sends STUN over raw UDP from the host
network interface. The resulting `srflx` candidate exposes your real public
IP, regardless of the fact that HTTP traffic is routed through the proxy.

`disable_non_proxied_udp` suppresses UDP candidate gathering unless the UDP
path is itself proxied (which HTTP proxies can't do), so WebRTC either falls
back to TCP-proxied relays or gathers nothing. Either outcome is safe.

Every example in this directory includes the flag. The entrypoint script
(`docker-entrypoint.sh`) includes it too. If you're writing your own
launcher, include it yourself.

### How to verify

Load `https://browserleaks.com/webrtc` in the browser. If the "Public IP"
row shows your proxy exit, you're safe. If it shows your datacenter/machine
IP, the flag is missing somewhere in your launch path.

## Per-Context Proxy with Geo-Targeted Fingerprints

Each browser context can use a different proxy, and Chromeleon automatically generates
a matching fingerprint (timezone, locale, language) based on the proxy's exit IP location.

### Required Per-Context Proxy Credential Registration

`Target.setProxyCredentials` is the sole supported credential path for an
authenticated per-context proxy. Register on a browser-level CDP session, then
create the context with only the exact same server string. The registration is
single-use, so serialize every register-then-create pair.

#### Register the string Playwright SENDS, not the one you wrote

The browser matches the two `proxyServer` values byte-for-byte, and only one of
them is yours. Playwright rewrites `proxy.server` on its way to the browser —
`normalizeProxySettings()` in `browserContext.js` returns
`url.protocol + '//' + url.host`, which **drops the scheme's default port**,
lowercases the host, and strips any path or embedded userinfo:

| you write | Playwright sends |
|---|---|
| `http://gw.example.com:80` | `http://gw.example.com` |
| `https://gw.example.com:443` | `https://gw.example.com` |
| `http://GW.Example.com:8080` | `http://gw.example.com:8080` |
| `gw.example.com:3128` | `http://gw.example.com:3128` |

Register the raw string and `createBrowserContext` fails with *"Target.setProxyCredentials
must be called first with the exact proxyServer used by Target.createBrowserContext"* —
even though you did call it first. Normalize once and use the result for **both**
calls. The helpers below reproduce Playwright's rewrite — the Node one by calling
the same `URL` parser, the Python one by reimplementing its host rules
(percent-decode, IPv6 compression, IDN punycode). The client library ships both as
`normalizeServer()` / `normalize_server()`, and the runnable examples in this
directory use them.

To see what actually went on the wire, run with `DEBUG=pw:protocol` and read the
`proxyServer` in the `Target.createBrowserContext` line.

#### Omitting `proxy` from the context is silent

A context created without `proxy` is simply **not proxied**, and nothing raises:
the registration stays unconsumed, the context egresses directly, and the persona
is selected with no geo constraint — so its timezone, locale and languages come
from the pool rather than from your exit IP. A session presenting a country you
never asked for is the usual symptom. Check that `proxy` is on `newContext`, not
only on `setProxyCredentials`.

**Async (recommended):**
```python
import ipaddress
from urllib.parse import unquote, urlsplit
from playwright.async_api import async_playwright

_DEFAULT_PORTS = {"http": 80, "https": 443}

def normalize_proxy_server(server: str) -> str:
    """Canonicalise as Playwright will before it sends the string."""
    raw = str(server).strip()
    parts = urlsplit(raw)
    if not parts.scheme or not parts.netloc:      # scheme-less "host:port"
        parts = urlsplit("http://" + raw)
    scheme = (parts.scheme or "http").lower()
    port = parts.port                             # ValueError on a bad port
    host = unquote(parts.hostname or "")
    if ":" in host:                               # IPv6 — hostname drops the []
        host = f"[{ipaddress.IPv6Address(host).compressed}]"
    elif any(ord(c) > 127 for c in host):         # IDN label -> punycode
        host = host.encode("idna").decode("ascii")
    host = host.lower()
    if port is not None and port != _DEFAULT_PORTS.get(scheme):
        host = f"{host}:{port}"
    return f"{scheme}://{host}"

async with async_playwright() as p:
    # REQUIRED: disable_non_proxied_udp prevents WebRTC from leaking the real
    # IP when per-context proxies are used. See "WebRTC and Proxies" above.
    browser = await p.chromium.launch(
        executable_path="./chrome",
        args=["--webrtc-ip-handling-policy=disable_non_proxied_udp"],
    )

    proxy_server = normalize_proxy_server("http://residential.floxy.io:12321")
    cdp = await browser.new_browser_cdp_session()
    try:
        await cdp.send("Target.setProxyCredentials", {
            "proxyServer": proxy_server,
            "username": "myuser",
            "password": "mypass_country-jp_session-abc123",
        })
    finally:
        await cdp.detach()

    # Same string, and credentials NEVER go here.
    context = await browser.new_context(proxy={"server": proxy_server})
    page = await context.new_page()
    # -> This context now has Asia/Tokyo timezone, ja-JP locale, etc.
```

**Sync:**
```python
from playwright.sync_api import sync_playwright
# normalize_proxy_server() as defined in the async example above.

with sync_playwright() as p:
    browser = p.chromium.launch(
        executable_path="./chrome",
        args=["--webrtc-ip-handling-policy=disable_non_proxied_udp"],
    )

    proxy_server = normalize_proxy_server("http://residential.floxy.io:12321")
    cdp = browser.new_browser_cdp_session()
    try:
        cdp.send("Target.setProxyCredentials", {
            "proxyServer": proxy_server,
            "username": "myuser",
            "password": "mypass_country-jp_session-abc123",
        })
    finally:
        cdp.detach()

    context = browser.new_context(proxy={"server": proxy_server})
    page = context.new_page()
    # -> This context now has Asia/Tokyo timezone, ja-JP locale, etc.
```

**Node.js:**
```javascript
const { chromium } = require('playwright');

// Reproduces Playwright's normalizeProxySettings(): URL.host already lowercases
// the host and omits the scheme's default port, so this matches byte-for-byte.
function normalizeProxyServer(server) {
    const s = /^[a-z][a-z0-9+.-]*:\/\//i.test(server) ? server : `http://${server}`;
    const url = new URL(s);
    return `${url.protocol}//${url.host}`;
}

const browser = await chromium.launch({
    executablePath: './chrome',
    args: ['--webrtc-ip-handling-policy=disable_non_proxied_udp'],
});

const proxyServer = normalizeProxyServer('http://residential.floxy.io:12321');
const cdp = await browser.newBrowserCDPSession();
try {
    await cdp.send('Target.setProxyCredentials', {
        proxyServer,
        username: 'myuser',
        password: 'mypass_country-jp_session-abc123',
    });
} finally {
    await cdp.detach();
}

// Same string, and credentials NEVER go here.
const context = await browser.newContext({ proxy: { server: proxyServer } });
const page = await context.newPage();
// -> This context now has Asia/Tokyo timezone, ja-JP locale, etc.
```

### Running the Per-Context Proxy Examples

```bash
# Async version (default)
python per_context_proxy_example.py --chromeleon-path ./chrome \
    --proxy1 http://user:pass_country-us@residential.floxy.io:12321 \
    --proxy2 http://user:pass_country-jp@residential.floxy.io:12321 \
    --headless

# Sync version
python per_context_proxy_example_sync.py --chromeleon-path ./chrome \
    --proxy1 http://user:pass_country-us@residential.floxy.io:12321 \
    --proxy2 http://user:pass_country-jp@residential.floxy.io:12321 \
    --headless
```

## Examples with Fingerprint Spoofing

```bash
# Spoof as Windows
python playwright_example.py --chromeleon-path ./chrome --headless \
  --fingerprint-os windows

# Use a proxy
python playwright_example.py --chromeleon-path ./chrome --headless \
  --proxy http://user:pass@proxy.example.com:8080

# Test FingerprintJS detection
python playwright_example.py --chromeleon-path ./chrome --headless \
  --fingerprint-os windows --url https://demo.fingerprint.com/playground

# Run pytest with Windows fingerprint
pytest tests/antibots/fingerprintjs_test.py --headless --fingerprint-os Windows
```

## Docker & Remote CDP

Run Chromeleon in a Docker container and connect remotely via CDP. This is the
recommended production setup — launch once, connect many times, no browser
startup cost per session.

### Quick Start

```bash
# 1. Extract your Chromeleon download
tar xzf Chromeleon-*.tar.gz
cd chromeleon-linux

# 2. Build the Docker image (Dockerfile lives at the package root)
docker build -t chromeleon .

# 3. Run it
docker run -d --name chromeleon -p 9222:9222 \
  --restart unless-stopped \
  --memory 2g \
  chromeleon

# 4. Connect from your code (see below)
```

### Connecting via `connectOverCDP`

**Python (async):**
```python
from playwright.async_api import async_playwright

async with async_playwright() as p:
    browser = await p.chromium.connect_over_cdp("http://localhost:9222")

    context = await browser.new_context()
    page = await context.new_page()
    await page.goto("https://example.com")

    # Each new context gets a unique fingerprint automatically
    print(await page.evaluate("navigator.userAgent"))
```

**Python (sync):**
```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp("http://localhost:9222")

    context = browser.new_context()
    page = context.new_page()
    page.goto("https://example.com")
```

**Node.js:**
```javascript
const { chromium } = require('playwright');

const browser = await chromium.connectOverCDP('http://localhost:9222');
const context = await browser.newContext();
const page = await context.newPage();
await page.goto('https://example.com');
```

### CDP with Per-Context Proxy

You can set proxies at context creation time (not browser launch) when
connected over CDP. This avoids relaunching the browser for each proxy.

**Important:** the server-side Chromeleon process you're connecting to must
have been launched with `--webrtc-ip-handling-policy=disable_non_proxied_udp`
or your context-level proxies will leak WebRTC (see "WebRTC and Proxies"
at the top of this README). The bundled `docker-entrypoint.sh` already
includes the flag.

```python
from playwright.async_api import async_playwright

async with async_playwright() as p:
    browser = await p.chromium.connect_over_cdp("http://localhost:9222")

    # normalize_proxy_server() as defined under "Register the string Playwright
    # SENDS" above — required, not cosmetic, for a proxy on :80 or :443.
    proxy_server = normalize_proxy_server("http://residential.floxy.io:12321")
    cdp = await browser.new_browser_cdp_session()
    try:
        await cdp.send("Target.setProxyCredentials", {
            "proxyServer": proxy_server,
            "username": "myuser",
            "password": "mypass_country-jp_session-abc123",
        })
    finally:
        await cdp.detach()

    # Use the identical server value and omit credentials here.
    context = await browser.new_context(proxy={"server": proxy_server})
    page = await context.new_page()
    # -> timezone=Asia/Tokyo, locale=ja-JP, language=ja, etc.
```

See `connect_over_cdp_example.py` for a complete working example.

## Timezone & Locale Configuration

Chromeleon automatically sets timezone, locale, and language based on the
proxy's geographic location using GeoIP lookup. Here's how it works:

### Automatic (with proxy)

When you preregister credentials and set a proxy at context creation time,
Chromeleon:
1. Validates the registration and resolves the proxy's exit IP address
2. Looks up the geographic location via GeoIP database
3. Generates a fingerprint config with matching timezone, locale, and language
4. Applies it to `Intl.DateTimeFormat().resolvedOptions()`, `navigator.language`, etc.

This is fully automatic — no `TZ` environment variable needed.

### Manual timezone override

If you need to set timezone without a proxy, or override the auto-detected one:

```python
# Set timezone via Playwright context options
context = await browser.new_context(
    timezone_id="Asia/Tokyo",
    locale="ja-JP",
)
```

### Troubleshooting: Locale doesn't change

**Problem:** `Intl.DateTimeFormat().resolvedOptions().locale` shows a locale you
did not ask for — `en-US`, or any other country — even with a Japanese proxy.

**Causes & fixes:**

0. **The context has no proxy at all.** This one is silent: nothing raises, the
   registration is simply never consumed, and a context with no proxy selects its
   persona with no geo constraint — so its locale and timezone come from the
   persona pool rather than from your exit IP. Any country can come out. Check
   that `proxy={"server": ...}` is on `new_context`, not only on
   `Target.setProxyCredentials`, and confirm what the page itself sees:
   `page.goto("https://api.ipify.org?format=json")`. See
   [Omitting `proxy` from the context is silent](#omitting-proxy-from-the-context-is-silent).

1. **Proxy credentials not preregistered, or `proxyServer` did not match.** Send
   `Target.setProxyCredentials` immediately before `new_context`, and omit
   `username`/`password` from the Playwright proxy object. "The same server
   string" means the one Playwright *sends*, which is not necessarily the one you
   wrote — see [Register the string Playwright
   SENDS](#register-the-string-playwright-sends-not-the-one-you-wrote). Unlike
   cause 0, this one is loud: context creation fails before allocation when
   registration or exit-IP/persona resolution fails.

2. **Browser-level proxy vs context-level proxy — for fingerprint matching only.**
   A browser-level `--proxy-server` is supported only for an unauthenticated
   proxy shared by every context. Authenticated proxies must use the
   preregister-then-create sequence above, which also provides per-context
   geo-matching.

   This is a *locale/timezone* concern, not a WebRTC concern. Both patterns
   need `--webrtc-ip-handling-policy=disable_non_proxied_udp` in launch
   args to prevent WebRTC IP leak (see "WebRTC and Proxies" at the top of
   this README).

   ```python
   # For per-context geo-targeted fingerprints:
   browser = p.chromium.launch(
       executable_path="./chrome",
       args=["--webrtc-ip-handling-policy=disable_non_proxied_udp"],  # required
   )
   proxy_server = normalize_proxy_server("http://proxy.example:8080")
   cdp = browser.new_browser_cdp_session()
   cdp.send("Target.setProxyCredentials", {
       "proxyServer": proxy_server,
       "username": "user",
       "password": "pass",
   })
   cdp.detach()
   context = browser.new_context(proxy={"server": proxy_server})
   ```

3. **`TZ` environment variable.** Setting `TZ=Japan` changes the OS timezone
   but does NOT change JavaScript's locale. The locale is set by Chromeleon's
   fingerprint config, which requires the proxy resolution to work (see #1).

4. **Verify the fingerprint config is active.** Check the Chrome log output for:
   ```
   [FINGERPRINT_PLATFORM_PROPAGATE] Propagating platform=... to child process
   ```
   If you don't see this, the fingerprint config wasn't generated — usually
   because the proxy exit IP couldn't be resolved. Seeing the line does NOT rule
   out cause 0: an unproxied context still generates a config, just an
   unconstrained one.

## VNC Display Platform

Chromeleon includes an embedded VNC server as an alternative to headless mode.
Instead of Xvfb + x11vnc + websockify, the VNC server runs directly inside the
browser process with zero external dependencies.

### Usage

```bash
# Launch with VNC (replaces --headless)
./chrome --ozone-platform=vnc --vnc-port=5900

# Connect with any VNC viewer
vncviewer localhost:5900

# With Playwright (connect over CDP)
./chrome --ozone-platform=vnc --vnc-port=5900 --remote-debugging-port=9222
```

```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp("http://localhost:9222")
    page = browser.contexts[0].new_page()
    page.goto("https://example.com")
    # Page is visible in VNC viewer while Playwright controls it
```

### VNC Options

| Flag | Default | Description |
|------|---------|-------------|
| `--ozone-platform=vnc` | — | Enable VNC display platform |
| `--vnc-port=PORT` | 5900 | VNC server port (raw RFB + WebSocket) |
| `--vnc-password=PASS` | (none) | Optional VNC authentication password |

### Running the VNC Example

```bash
# Launch browser with VNC + control via Playwright
python vnc_example.py --chromeleon-path ./chrome

# With fingerprint spoofing
python vnc_example.py --chromeleon-path ./chrome --fingerprint-os Windows

# Custom VNC port
python vnc_example.py --chromeleon-path ./chrome --vnc-port 5901

# Visit a specific URL
python vnc_example.py --chromeleon-path ./chrome --url https://demo.fingerprint.com/playground

# Connect to an already-running Docker instance
python vnc_example.py --cdp-url http://localhost:9222
```

### Docker with VNC

```bash
# Expose both CDP and VNC ports
docker run -d --name chromeleon -p 9222:9222 -p 5900:5900 \
  chromeleon --ozone-platform=vnc --vnc-port=5900 --remote-debugging-port=9222

# Connect VNC viewer to localhost:5900 to see the browser
# Connect Playwright to http://localhost:9222 to control it
```

### VNC vs Headless

| | Headless | VNC |
|---|---|---|
| External dependencies | None | None |
| Visual debugging | Screenshots only | Live view via VNC viewer |
| Performance | Fastest | ~Same (SwiftShader in both) |
| Fingerprint behavior | Identical | Identical |
| Use case | Production, CI | Debugging, demos, monitoring |

All fingerprint spoofing features work identically on both platforms.

---

## Known Issues & Limitations

### `--fingerprint-os=Windows` inconsistencies

When using `--fingerprint-os=Windows` on a Linux host, some detection services
may show inconsistent or fluctuating results. This is because certain low-level
signals (GPU rendering, canvas bitmap, system fonts) are generated by the actual
Linux GPU/renderer and cannot be perfectly spoofed.

**What works well:**
- FingerprintJS Pro — passes with no tampering detected
- OverpoweredJS — botScore 2 ("Human") across all spoofed OSes
  (Windows=2, macOS=2, Linux=2). The score is fingerprint-driven, not an
  unfixable OS-mismatch tell; it dropped from the older 3-4 once the WebRTC
  `videoCodecs` capabilities were sourced per-persona from `RTCRtpReceiver`
  decode caps.
- Navigator properties (platform, userAgent, userAgentData)
- Screen dimensions, timezone, locale
- WebGL vendor/renderer strings
- Audio fingerprint

**What may be detected:**
- Kasada — may detect OS mismatch (canvas + WebGL rendering cross-correlation)
- Canvas bitmap hash — the actual pixel rendering differs from real Windows

**Recommendation:** For sites protected by Kasada, native Linux fingerprint
(no `--fingerprint-os`) with a residential proxy remains the most robust
option, since it avoids the host GPU/canvas cross-correlation entirely.

## Node.js Usage

```bash
# Install playwright
npm install playwright

# Run example
node playwright_example.js --chromeleon-path ./chrome --headless --test
```
