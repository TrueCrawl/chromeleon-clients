# Rulings

The 20 corpus cases where the Python and Node clients **disagree**, and why each
is decided the way it is. Measured 2026-08-12 by running both clients over
`vectors.json`; the server strings were additionally checked against
`playwright-core 1.59.1 normalizeProxySettings`, which is the function the
driver applies to whatever server you hand it.

**The tie-breaker is not opinion.** Invariant 1 says the normalized server is
"exactly WHATWG `URL(...).protocol + "//" + URL(...).host`", and that is
literally what the driver does:

```js
function normalizeProxySettings(proxy) {
  let { server } = proxy;
  let url;
  try { url = new URL(server); if (!url.host || !url.protocol) url = new URL("http://" + server); }
  catch (e) { url = new URL("http://" + server); }
  server = url.protocol + "//" + url.host;
```

So a client's `setProxyCredentials.proxyServer` must be a **fixed point** of
that function. Feeding every emitted server back through it: Node 0 failures,
Python 5 (`s57`, `s58`, `s65`, `s69`, `s70`) — in each, the browser is given a
different string than was registered, so the registration is never consumed.
Note the `catch` branch: it turns junk into the host `http`, silently, which is
why the Rust client raises where the driver would guess.

Credential handling has no driver oracle — credentials never reach the driver —
so those cases are decided against the spec text.

Score: **node right on 11, python right on 8, neither right on 1** (`s66`).

| # | input | python | node | correct | severity |
|---|---|---|---|---|---|
| s44 | `http://faß.example:8080` | `http://fass.example:8080` | `http://xn--fa-hia.example:8080` | **node** | high — wrong host |
| s45 | `http://σόλος.example:8080` | `http://xn--wxaikc6b.example:8080` | `http://xn--wxaijb9b.example:8080` | **node** | high — wrong host |
| s49 | `http://gw.example.com/?a=@b` | srv `http://b`, user `gw.example.com/?a=` | srv `http://gw.example.com` | **node** | high |
| s50 | `http://user:pass@gw.example.com:8080/p?x=@y` | srv `http://y`, pass `pass@gw…/p?x=` | srv `http://gw.example.com:8080`, pass `pass` | **node** | high |
| s57 | `""` | `http://` | throws | **node** | med |
| s58 | `"   "` | `http://` | throws | **node** | med |
| s63 | `http://user:p%GGss@gw…` | pass `p%GGss` | `URIError: URI malformed` | **python** | med |
| s65 | `//gw.example.com:8080` | `http://` | `http://gw.example.com:8080` | **node** | high |
| s66 | `http://user:p%FFss@gw…` | pass `p�ss` | `URIError: URI malformed` | *neither* | high |
| s68 | `http://user:pass%@gw…` | pass `pass%` | `URIError: URI malformed` | **python** | med |
| s69 | `http://ex%20ample.com:8080` | `http://ex ample.com:8080` | throws | **node** | high |
| s70 | `http://ex%2Fample.com:8080` | `http://ex/ample.com:8080` | throws | **node** | high |
| d02 | `{server:"http://user:pass@gw…"}` | user `user`, pass `pass` | `null`/`null` | **python** | high |
| d04 | `{server:"http://us%40er:p%40ss@gw…"}` | user `us@er`, pass `p@ss` | `null`/`null` | **python** | high |
| d11 | `{server:"http://user:pass@gw…", username:null, password:null}` | user `user`, pass `pass` | `null`/`null` | **python** | high |
| d14 | `{server:"socks5://user:pass@gw:1080"}` | user `user`, pass `pass` | `null`/`null` | **python** | low |
| d17 | `{server:null, username:"u", password:"p"}` | raises | `http://null`, authenticated | **python** | high |
| d18 | `{server:"http://gw.example.com/?x=@y", …null}` | srv `http://y` | srv `http://gw.example.com` | **node** | high |
| d20 | `{server:…, username:123, password:456}` | `123` (int) | `"123"` | **node** | med |
| d24 | `{server:"http://useronly@gw…"}` | user `useronly` | `null`/`null` | **python** | low |

## By root cause

**A. IDNA (`s44`, `s45`) — node.** Python's `str.encode("idna")` is IDNA2003:
`ß` → `ss`, Greek final sigma folds. WHATWG (and Chromium) use UTS-46
non-transitional. `fass.example` and `xn--fa-hia.example` are different
registrable domains, so this is not a spelling difference — the proxy connects
somewhere else. *Rust: the `url`/`idna` crates. Never hand-roll punycode.*

**B. credential split scope (`s49`, `s50`, `d18`) — node.** Python searches the
whole URL for the last `@`, so an `@` in a path or query wins and the host ends
up in the username field while the server points at the query fragment. *Rust:
slice the authority at the first `/?#`, then take the last `@` inside it.*

**C. host percent-decoding (`s69`, `s70`) — node.** Python decodes the host and
returns it without re-validating, so a space or `/` survives inside a hostname.
Benign cases agree (`%61` → `a` in both) because WHATWG decodes too — and then
re-validates. *Rust: decode, re-parse, reject forbidden host characters.*

**D. un-URL-able input (`s57`, `s58`, `s65`) — node.** Python's `urlsplit` on
`http:////gw…` puts the authority in the path and loses the host, yielding
`"http://"`. *Rust: parse with the `http://` fallback; an empty server is
`MissingServer`, not a repair.*

**E. credential percent-decoding (`s63`, `s68` — python; `s66` — neither).**
`decodeURIComponent` is RFC 3986-strict and throws on a stray `%`, which turns
the ordinary password `p%ssword` into a hard failure; WHATWG passes an invalid
escape through as text. But on `%FF` — a well-formed escape whose byte is not
UTF-8 — python substitutes U+FFFD, which is invariant 2's exact failure mode:
you authenticate with a secret that is not yours and it fails silently at the
proxy. *Rust: pass invalid escapes through; raise a named error on non-UTF-8.*

**F. object whose `server` embeds credentials (`d02`, `d04`, `d11`, `d14`,
`d24`) — python.** Python recurses into the URL when the object supplies
neither credential; node drops them and then refuses the handshake with "needs a
username and password". Node's version is fail-closed rather than a leak, but
the credentials were right there. Explicit credentials win over embedded ones in
both, and stay literal. *Rust: python's rule exactly.*

**G. `{server: null}` (`d17`) — python.** Node's `'server' in proxy` passes for
an explicit null, `String(null)` gives the host `null`, and it registers real
credentials against it. *Rust: `server` is a required `&str`; unreachable.*

**H. non-string credentials (`d20`) — node.** CDP wants strings. Python also
collapses a falsy credential (`0` → `""`) via `spec.username or ""`. *Rust:
`String`-typed; `None` serializes as `""`, a non-empty value never does.*

## Same behaviour, different spelling (not counted as divergence)

| input | python | node |
|---|---|---|
| `http://host:notaport` | `ValueError: Port could not be cast…` | `TypeError: Invalid URL` |
| `http://host:99999` | `ValueError: Port out of range` | `TypeError: Invalid URL` |
| `{"server": ""}` | `ValueError: requires a 'server' key` | `TypeError: Invalid URL` |

Both raising on a bad port is better than the driver, which returns
`"http://http"` for `http://host:notaport` without complaint. Python's messages
name the offending port; Node's say nothing. The corpus expects the Rust
client's own error *kinds* here (`invalid_server`, `missing_server`), and only
checks that the other two raised at all.

## Not in the corpus, but the worst of the lot

`parseProxy` in the Node client early-returns a `ProxySpec` untouched, and its
constructor does not normalize:

```
node   parseProxy(new ProxySpec("HTTP://Example.COM:80", "u", "p"))
       registers  "HTTP://Example.COM:80"   driver sends "http://example.com"   ✘ never consumed
python re-normalizes to "http://example.com"                                    ✔
```

Worse, a spec built from a credential URL keeps the userinfo inside `server`, so
Node would put the password in `proxyServer`. *Rust: `ProxySpec` is only
constructible through a normalizing constructor, so this state does not exist.*
