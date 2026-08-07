# chromeleon-clients

Client libraries for driving the [Chromeleon](https://chromeleon.dev) browser.
Chromeleon itself is a licensed product; these drive it, they do not contain it.
This repository is deliberately **separate from the browser build**: the clients
are version-agnostic and ship on their own cadence to their own registries.

```
chromeleon-clients/
  HANDSHAKE.md       # the ONE spec every client implements — source of truth
  python/            # → PyPI  pip install chromeleon   (sync + async)
  node/              # → npm   npm i chromeleon         (async)
  .github/workflows/ # per-package publish (PyPI + npm Trusted Publishing)
```

| language | package | status |
|---|---|---|
| [Python](python/) | `pip install chromeleon` | available |
| [Node / TypeScript](node/) | `npm i chromeleon` | planned |
| Java, C#, Go, Rust | — | see [the docs](https://chromeleon.dev/docs) for the raw CDP handshake |

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
this one. A contract test there pins them against these clients' constants, so
renaming either fails the browser build rather than silently breaking proxies in
a customer's session.

## Installing

```
pip install chromeleon          # python/
npm install chromeleon          # node/
```

Both packages implement the same handshake and are held to it by
`HANDSHAKE.md`. The Node client is fully async, because every driver it wraps
is; the Python one ships both a sync and an async entry point
(`new_proxy_context` / `new_proxy_context_async`).

## Adding a language

1. Implement `HANDSHAKE.md` as a thin adapter over the driver's CDP send.
2. Add a publish workflow for its registry.

There is nothing to codegen — this is a stateful CDP handshake, not a REST or
gRPC surface — so each client is a small native implementation held to the
shared spec.

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

npm differs from PyPI in one way that matters: there is no "pending publisher",
so a trusted publisher can only be attached to a package that already exists.
The first publish of a new name is therefore manual and token-authenticated;
afterwards, attach this workflow at npmjs.com -> the package -> Settings ->
Trusted Publisher, and no credential is needed again.

Release with a `node-v<version>` tag. As with Python, the build refuses to
publish when the tag and `package.json` disagree — npm versions are immutable
and cannot be reused.
