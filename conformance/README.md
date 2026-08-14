# Conformance

`HANDSHAKE.md` says what every client must do. This is where that stops being a
document and becomes a check.

```
conformance/
  vectors.json     94 proxy inputs and the answer every client must give
  RULINGS.md       why each contested case is decided the way it is
  check.py         runs every client present in the checkout, reports drift
  emit_python.py   \  emitters: run the corpus, print what the client produced,
  emit_node.js      > judge nothing. The comparing happens once, in check.py, so
  (rust)           /  no client can grade itself with the bug it is checked for.
```

The Rust emitter is `cargo run --example conformance_emit`.

## Run it

```sh
python3 conformance/check.py                 # every client in this checkout
python3 conformance/check.py --client rust   # just one
```

## What it covers

Invariants **1** (byte-identical, normalized `proxyServer`) and **2**
(credentials decoded, exactly once, only into the registration). Those are the
two that fail *silently* — an unnormalized server is never consumed by
`createBrowserContext` and the context browses direct; a credential decoded
wrong authenticates as somebody else. Both look like a flaky proxy.

It does not cover invariant 3 (serialization) or the launch flags: those need a
browser, and each client tests them in its own suite.

## Where the expected answers come from

Not from one implementation, and not from an opinion:

- Where the Python and Node clients **agree**, that agreed answer is the
  expectation — two independent implementations, and for the server string a
  third check below.
- Every expected server string was fed through the driver's own normalizer
  (`playwright-core 1.59.1 normalizeProxySettings`) — recorded per case as
  `playwright_normalizes_input_to`. The registered string must be a **fixed
  point** of that function or the two commands disagree.
- Where the two clients **disagree** (20 cases), `RULINGS.md` decides, and the
  losing behaviour stays in the case as `observed.<client>` so the disagreement
  is documented rather than flattened.

## Statuses

| | |
|---|---|
| `PASS` | matches the expectation |
| `XFAIL` | matches the wrong answer already recorded for that client — a known, documented divergence |
| `FIXED` | used to be divergent and now matches: delete its `observed` entry |
| `FAIL` | anything else — behaviour changed, or never agreed |

Only `FAIL` is fatal. An `XFAIL` cannot hide, because every one of them is
printed with its ruling every run.

Current state: **rust 91/91** (3 cases its types cannot express), **node 86/94**,
**python 82/94**. The Python and Node divergences are real bugs — `RULINGS.md`
grades 13 of the 20 contested cases *high*. The worst are Python's whole-string
`@` search (it can emit a server pointing at the wrong host, with the real host
in the username), Python's IDNA2003 host encoding (`faß` → `fass`, a different
registrable domain), and Node's unnormalized `ProxySpec` passthrough (the
registration is then never consumed, silently).

## Adding a case

Add it to `vectors.json` with a `note` saying what it is for and a `basis` of
`agreed` (both older clients produce it) or `ruling:<who>` with an entry in
`RULINGS.md`. Then run `check.py` and put any client that disagrees into
`observed`, or fix the client.
