# chromeleon-clients

Client libraries for driving the [Chromeleon](https://chromeleon.dev) browser.
Chromeleon itself is a licensed product; these drive it, they do not contain it.
This repository is deliberately **separate from the browser build**: the clients
are version-agnostic and ship on their own cadence to their own registries.

```
chromeleon-clients/
  HANDSHAKE.md       # the ONE spec every client implements — source of truth
  python/            # → PyPI      pip install chromeleon    (sync + async)
  node/              # → npm       npm i chromeleon          (async)
  rust/              # → crates.io cargo add chromeleon      (async + blocking)
  conformance/       # one corpus, every client, checked in one place
  .github/workflows/ # per-package publish (PyPI + npm + crates.io, all OIDC)
```

| language | package | status |
|---|---|---|
| [Python](python/) | `pip install chromeleon` | available |
| [Node / TypeScript](node/) | `npm i chromeleon` | available |
| [Rust](rust/) | `cargo add chromeleon` | available |
| Java, C#, Go | — | see [the docs](https://chromeleon.dev/docs) for the raw CDP handshake |

Each client lives in its own directory, versions on its own cadence, and
publishes to its own registry. Nothing is shared at build time; adding a
language means adding a directory and a CI job.

## What a client is for

Chromeleon attaches an authenticated proxy **per browser context**, through two
browser-level CDP commands that must be issued in order, on one connection, with
a byte-identical server string, and serialized against each other:

```
Target.setProxyCredentials  {proxyServer, username, password}
Target.createBrowserContext {proxyServer}
```

No driver has an API shaped like that, and the ways of getting it wrong are
quiet: credentials passed at launch leave the exit IP unresolved, which unbinds
the persona's geo and disables WebRTC masking; and the driver may not send the
server string you wrote — Playwright rewrites it, so the two commands disagree
and the registration is never consumed. A client exists so that is not the
caller's problem.

## The contract

[`HANDSHAKE.md`](HANDSHAKE.md) is the source of truth, and it carries a **spec
version**. When the handshake changes, bump the spec version and every client's
**major** together; patch and minor move independently.

The command name and the launch flag are owned by the browser repository, not by
this one. Contract tests there pin the browser's **own** literals — `tests/unit/
test_v150_devtools_compile_contract.py` asserts `experimental command
setProxyCredentials` is in the PDL and that the "must be called first" error text
survives — so a rename fails the browser build.

⚠️ Nothing compares those literals against **these clients'** constants: no test
in the browser repository reads this one. The two are kept in step by hand, which
is a gap worth closing, and until it is, a protocol rename would break customer
proxies quietly on this side.

## Installing

```
pip install chromeleon          # python/  — live on PyPI
npm install chromeleon          # node/    — live on npm
cargo add chromeleon            # rust/    — live on crates.io
```

All three implement the same handshake and are held to it by `HANDSHAKE.md` and
the shared corpus below. The Node client is fully async, because every driver it
wraps is; the Python one ships both a sync and an async entry point
(`new_proxy_context` / `new_proxy_context_async`); the Rust one is
driver-agnostic — it depends on no browser library — and offers both async and
blocking forms of the same lock.

## Holding the clients together

[`conformance/`](conformance/) is 94 proxy inputs with the answer every client
must give, plus a runner that checks all of them at once:

```
python3 conformance/check.py
```

The expected answers are not one implementation's opinion. Where Python and Node
agree, that is the expectation; every expected server string was also checked
against the driver's own normalizer, because the string a client registers has
to be a **fixed point** of it or the browser is handed something else. Where the
two disagree — 20 of the 94 — `conformance/RULINGS.md` decides and records the
losing behaviour beside it, so a known divergence shows up as `XFAIL` on every
run instead of quietly becoming the standard.

Current state: rust 91/91 of what its types can express, node 86/94, python
82/94. The Python and Node gaps are real bugs with severities listed in
`RULINGS.md`.

## Adding a language

1. Implement `HANDSHAKE.md` as a thin adapter over the driver's CDP send.
2. Add a publish workflow for its registry.

There is nothing to codegen — this is a stateful CDP handshake, not a REST or
gRPC surface — so each client is a small native implementation held to the
shared spec.

## Releasing the Rust client

`publish-rust.yml` publishes `rust/` to crates.io with Trusted Publishing, the
same tokenless OIDC mechanism as the other two.

crates.io follows npm's model rather than PyPI's: there is **no pending
publisher**, so the first publish of a name must be manual and token-authenticated.
That has been done — `chromeleon` 0.1.0 exists on crates.io — so the publisher can
now be attached at crates.io -> the crate -> Settings -> Trusted Publishing (owner
`TrueCrawl`, repository `chromeleon-clients`, workflow `publish-rust.yml`,
environment `crates-io`), after which no credential is needed again. The config
keys on the workflow **filename**.

⚠️ crates.io also refuses to publish at all from an account with **no verified
email address** — that failure looks like a token problem and is not one.

Release with a `rust-v<version>` tag. The build refuses to publish when the tag
and `Cargo.toml` disagree — a crates.io version number is permanent, and cannot
be reused even after a yank.

## Releasing the Python client

`publish-python.yml` uploads `python/` to PyPI through Trusted Publishing, so no
API token is stored in this repository or anywhere else — PyPI verifies a
short-lived OIDC token minted by GitHub for this exact repo and workflow file.

One-time setup, at https://pypi.org/manage/account/publishing/ — add a *pending*
publisher (project `chromeleon`, owner `TrueCrawl`, repository
`chromeleon-clients`, workflow `publish-python.yml`, environment `pypi`). The
"pending" kind is what allows the first upload to create the project; it becomes
an ordinary publisher once that upload lands.

Then run the workflow, or push a `python-v<version>` tag — the build refuses to
publish if the tag and the built version disagree, because a version number on
PyPI can never be reused, even after a release is yanked.

## Releasing the Node client

`publish-node.yml` publishes `node/` to npm with Trusted Publishing, the same
tokenless mechanism as the Python workflow.

npm differs from PyPI in one way that mattered once: there is no "pending
publisher", so a trusted publisher can only be attached to a package that
already exists. The first publish of a new name is therefore manual and
token-authenticated. That has been done — `chromeleon` exists on npm — so the
workflow can now be attached at npmjs.com -> the package -> Settings -> Trusted
Publisher, and no credential is needed again.

Release with a `node-v<version>` tag. As with Python, the build refuses to
publish when the tag and `package.json` disagree — npm versions are immutable
and cannot be reused.
