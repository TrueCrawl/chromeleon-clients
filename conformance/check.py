#!/usr/bin/env python3
"""Run every client that is present against the shared corpus and report drift.

    python3 conformance/check.py [--client python|node|rust]...

Each client has an emitter that only reports what it produced; the judging is
here, once, so no client can grade itself with the same bug it is being checked
for. A client is compared against `expect`, and:

  PASS   it matches
  XFAIL  it matches the wrong answer this corpus already RECORDED for it
         (`observed.<client>`) — a known, documented divergence, not drift
  FIXED  it used to be divergent and now matches: drop the `observed` entry
  FAIL   anything else — the client changed behaviour, or never agreed

Exit status is non-zero only for FAIL, so a known divergence does not block CI
while it also cannot hide: every XFAIL is printed.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
VECTORS = HERE / "vectors.json"

EMITTERS = {
    "python": {
        "cmd": [sys.executable, str(HERE / "emit_python.py"), str(VECTORS)],
        "present": (REPO / "python" / "chromeleon" / "core.py").exists(),
        "cwd": REPO,
    },
    "node": {
        "cmd": ["node", str(HERE / "emit_node.js"), str(VECTORS)],
        "present": (REPO / "node" / "src" / "core.js").exists(),
        "cwd": REPO,
    },
    "rust": {
        "cmd": ["cargo", "run", "--quiet", "--example", "conformance_emit", "--", str(VECTORS)],
        "present": (REPO / "rust" / "Cargo.toml").exists(),
        "cwd": REPO / "rust",
    },
}

VALUE_FIELDS = ("server", "username", "password", "authenticated")


def emit(client: str) -> dict[str, dict]:
    spec = EMITTERS[client]
    proc = subprocess.run(
        spec["cmd"], cwd=spec["cwd"], capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        raise SystemExit(
            f"{client} emitter failed ({proc.returncode}):\n{proc.stderr.strip()}"
        )
    # cargo prints build output on stderr, so stdout is the payload alone.
    payload = json.loads(proc.stdout)
    return {r["id"]: r for r in payload["results"]}


def as_outcome(result: dict, strict_kinds: bool) -> dict:
    """A client's answer in the corpus's own vocabulary."""
    if not result.get("ok", False):
        # Only the Rust client reports the corpus's error kinds; for the others
        # an error is an error, because their exception types are their own.
        return {"error": result.get("kind") if strict_kinds else "<error>"}
    return {field: result.get(field) for field in VALUE_FIELDS}


def compare(expected: dict, actual: dict, strict_kinds: bool) -> bool:
    if "error" in expected:
        if "error" not in actual:
            return False
        return not strict_kinds or expected["error"] == actual["error"]
    if "error" in actual:
        return False
    return all(expected.get(f) == actual.get(f) for f in VALUE_FIELDS)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client", action="append", choices=sorted(EMITTERS))
    args = parser.parse_args()

    corpus = json.loads(VECTORS.read_text())
    cases = {c["id"]: c for c in corpus["cases"]}
    wanted = args.client or [c for c, spec in EMITTERS.items() if spec["present"]]

    failures = 0
    for client in wanted:
        if not EMITTERS[client]["present"]:
            raise SystemExit(f"{client} client is not in this checkout")
        results = emit(client)
        strict = client == "rust"
        tally = {"PASS": 0, "XFAIL": 0, "FIXED": 0, "FAIL": 0, "SKIP": 0}
        notes: list[str] = []

        missing = set(cases) - set(results)
        if missing:
            raise SystemExit(f"{client} emitter skipped cases silently: {sorted(missing)}")

        for cid, case in cases.items():
            result = results[cid]
            if "skipped" in result:
                tally["SKIP"] += 1
                notes.append(f"  SKIP  {cid}  {result['skipped']}")
                continue
            actual = as_outcome(result, strict)
            if compare(case["expect"], actual, strict):
                recorded = case.get("observed", {}).get(client)
                if recorded is not None and not compare(case["expect"], recorded, False):
                    tally["FIXED"] += 1
                    notes.append(
                        f"  FIXED {cid}  {case['note']} — drop observed.{client} from the corpus"
                    )
                else:
                    tally["PASS"] += 1
                continue
            recorded = case.get("observed", {}).get(client)
            if recorded is not None and compare(recorded, actual, False):
                tally["XFAIL"] += 1
                notes.append(f"  XFAIL {cid}  {case['note']}  [{case['basis']}]")
            else:
                tally["FAIL"] += 1
                notes.append(
                    f"  FAIL  {cid}  {case['note']}\n"
                    f"          want {case['expect']}\n"
                    f"          got  {actual}"
                )

        failures += tally["FAIL"]
        summary = " ".join(f"{k}={v}" for k, v in tally.items() if v)
        print(f"{client}: {summary}")
        for note in notes:
            print(note)

    print()
    print("FAIL means a client no longer does what the corpus recorded — fix the client")
    print("or, if the corpus is wrong, change it deliberately and say why in RULINGS.md.")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
