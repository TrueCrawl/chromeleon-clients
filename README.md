# chromeleon-clients

Client libraries for driving the [Chromeleon](https://chromeleon.dev) browser.
Chromeleon itself is a licensed product; these drive it, they do not contain it.

| language | package | status |
|---|---|---|
| [Python](python/) | `pip install chromeleon` | available |
| Node / TypeScript | `npm i chromeleon` | planned |
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
the persona's geo and disables WebRTC masking. A client exists so that is not
the caller's problem.

## The contract

The command name and the launch flag are owned by the browser repository, not by
this one. A contract test there pins them against these clients' constants, so
renaming either fails the browser build rather than silently breaking proxies in
a customer's session.

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
