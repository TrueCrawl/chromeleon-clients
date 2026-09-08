# Releasing

Each client versions and ships on its own cadence. A release is a tag; the tag
triggers the matching workflow; the workflow publishes and then **asks the
registry whether the version actually arrived**.

| client | tag | workflow | registry | credential |
|---|---|---|---|---|
| python | `python-v<x.y.z>` | `publish-python.yml` | PyPI | `PYPI_API_TOKEN` in the `pypi` environment |
| node | `node-v<x.y.z>` | `publish-node.yml` | npm | `NPM_TOKEN` in the `npm` environment |
| rust | `rust-v<x.y.z>` | `publish-rust.yml` | crates.io | `CRATES_IO_TOKEN` in the `crates-io` environment |

## One-time: the credentials

All three publish with an API token held as a **GitHub environment secret**.
Trusted Publishing (OIDC) is supported as a fallback and takes over
automatically if you delete the token, but it is not the default — see
[Why token-first](#why-token-first).

```bash
# PyPI — https://pypi.org/manage/account/token/  (scope: project "chromeleon")
gh secret set PYPI_API_TOKEN  --repo TrueCrawl/chromeleon-clients --env pypi

# npm — https://www.npmjs.com/settings/<user>/tokens  (type: Automation)
gh secret set NPM_TOKEN       --repo TrueCrawl/chromeleon-clients --env npm

# crates.io — https://crates.io/settings/tokens  (scope: publish-update)
gh secret set CRATES_IO_TOKEN --repo TrueCrawl/chromeleon-clients --env crates-io
```

Each command prompts for the value; nothing is written to disk or shell history.
Verify without revealing them:

```bash
for e in pypi npm crates-io; do
  echo "$e: $(gh api repos/TrueCrawl/chromeleon-clients/environments/$e/secrets \
        --jq '[.secrets[].name]|join(", ")')"
done
```

## Cutting a release

1. Land the change on `main`. CI must be green.
2. Bump the version in the client you are releasing:
   `python/pyproject.toml`, `node/package.json`, or `rust/Cargo.toml`
   (+ `cargo check --locked` so `Cargo.lock` follows).
3. Commit the bump, then tag and push:

```bash
git tag -a python-v0.3.1 -m "python-v0.3.1"
git push origin python-v0.3.1
```

4. Watch it land. The workflow fails loudly if the version is not on the
   registry two minutes after publishing:

```bash
gh run list --repo TrueCrawl/chromeleon-clients --limit 5
```

Versions are **immutable** on all three registries — a number can never be
reused, even after a deletion — so a botched release is a new patch version, not
a re-push.

## Why token-first

Because OIDC silently did not work, twice, and nobody found out for a month.

`publish-python` and `publish-node` were originally OIDC-only. Both failed on
`python-v0.2.0` / `node-v0.2.0` (2026-08-11) and again on `v0.3.0`
(2026-09-08). 0.2.0 reached PyPI and npm by some other route while the
workflows stayed red. `publish-rust`, the one workflow using a stored token,
worked every time.

The two failures read like code errors and are not:

- **PyPI** answers with a *claim mismatch* — no Trusted Publisher is registered
  for this repository, workflow and environment.
- **npm** answers `E404 … you do not have permission`. npm can only attach a
  Trusted Publisher to a package that **already exists**, so the first release
  could never have used OIDC, and it was evidently never attached afterwards.

Tokens are boring and they work. If you would rather move to OIDC:

- PyPI → https://pypi.org/manage/project/chromeleon/settings/publishing/ —
  owner `TrueCrawl`, repository `chromeleon-clients`, workflow
  `publish-python.yml`, environment `pypi`
- npm → npmjs.com → `chromeleon` → Settings → Trusted Publisher — repository
  `TrueCrawl/chromeleon-clients`, workflow `publish-node.yml`

Then delete the corresponding secret; the workflow switches path on its own.
Both registries key their publisher config on the workflow **file name**, so
renaming one breaks publishing until the console is updated to match.

## If a release does not appear

The verification step tells you which of the two remedies you need. The tag is
already correct at that point, so once the credential is in place, re-run the
failed job — you do **not** need a new tag:

```bash
gh run rerun <run-id> --repo TrueCrawl/chromeleon-clients --failed
```
