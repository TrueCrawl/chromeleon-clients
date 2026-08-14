# The Chromeleon per-context proxy handshake (spec v1)

This is the single source of truth every language client implements. When this
changes, bump the spec version and move every client's **major** together.

A shared corpus (see [`conformance/`](conformance/)) holds 94 proxy inputs and
the answer every client must give, and `conformance/check.py` runs all of them
against it. That covers invariants **1** and **2** below — the two that fail
silently. Invariant 3 and the launch flags need a browser, so each client
asserts those in its own suite.

## What it is

Chromeleon attaches an **authenticated** proxy to a single browser **context**
with two **browser-level** CDP commands, in order, on one connection:

```
Target.setProxyCredentials  { proxyServer, username, password }
Target.createBrowserContext { proxyServer }
```

Everything else a client does is convenience around this pair.

## Invariants (these are what clients exist to get right)

1. **Byte-identical server string.** `proxyServer` must be the *same string* in
   both commands. Only one of the two is yours to control: the driver
   (Playwright/Puppeteer) re-normalizes the server it sends to
   `createBrowserContext`. Normalize your `setProxyCredentials.proxyServer` the
   same way so they match:
   - prepend `http://` if no scheme,
   - `protocol + "//" + host` where `host` lowercases the hostname and **drops
     the scheme's default port** (`http`→80, `https`→443, `ws`→80, `wss`→443).
   This is exactly WHATWG `URL(...).protocol + "//" + URL(...).host`.

2. **Credentials only in the registration.** Username/password go in
   `setProxyCredentials`, **never** on the context / never on the launch
   `--proxy-server`. Credentials embedded in a URL are percent-encoded, so
   **decode** them before they go on the wire, or you authenticate with the
   wrong secret and it fails silently.

3. **Single-use, and serialized per `(connection, proxyServer)`.** The
   registration is consumed by the matching `createBrowserContext` — including
   on a failure, so a retry must preregister again. The connection here is the
   **root** CDP session: sessions attached to it share one slot, separate
   WebSockets do not.

   How a second registration for the same pair is treated depends on the
   browser: **up to v151.4 it is rejected** while one is outstanding; **from
   v151.5 the reply is deferred** until the slot frees, which serializes the
   pair browser-side. v151.5 also accepts an optional correlation token,
   `credentialsId`, paired with `proxyCredentialsId` on the context —
   registrations carrying one are independent and never queue.

   A client must serialize register→create itself regardless: it cannot know
   which binary it is talking to, and against v151.4 nothing else will.
   Different servers on the same connection may run concurrently.

4. **HTTP(S) proxies only** for preregistration. An unauthenticated proxy needs
   no handshake — pass it straight to the driver. A SOCKS or credential-less
   proxy must not go through this path.

## Launch, not part of the handshake but required

- **Never pass proxy credentials at launch.** The binary fails closed on an
  unresolved exit IP (unbound geo + disabled WebRTC mask) with a remediation
  banner. Per-context proxies are launched with **no** `--proxy-server`.
- **Merge the WebRTC policy flag.** A context-level proxy routes HTTP, but
  WebRTC's UDP socket is browser-level and would gather ICE over the real route.
  Launch with `--webrtc-ip-handling-policy=disable_non_proxied_udp` (this is
  `LAUNCH_ARGS`).
- **Strip the controller's `PROXY_*` env** from the browser process; those are
  the controller's, and inheriting them routes browser traffic somewhere you
  didn't choose.

## Reference clients

| Language | Package | Handshake entry point |
|---|---|---|
| Python | `python/` → PyPI `chromeleon` | `new_proxy_context` / `new_proxy_context_async`, `proxy_registration` |
| Node   | `node/` → npm `chromeleon` | `newProxyContext` (async), `withProxyRegistration` |
| Rust   | `rust/` → crates.io `chromeleon` | `new_proxy_context_raw` / `new_proxy_context_with`, `proxy_registration` (async + blocking) |
| _(Go, .NET…)_ | as demand appears | implements this spec |

Where the clients disagree about an input, `conformance/RULINGS.md` says which
one is right and why. The Rust client follows every ruling — it is the only one
that passes the whole corpus (91/91 of the cases its types can express) — which
is a statement about the corpus, not a proof that it implements this whole
document; invariants 3 and 4 and the launch rules are not corpus-testable.
