#!/usr/bin/env python3
"""Run the shared corpus through the Python client and print the results.

Emitter contract (see README.md): read `vectors.json` from argv[1], write
`{"client", "results": [{"id", "ok", ...}]}` to stdout. Nothing is judged here —
`check.py` does the comparing, so an emitter can never quietly grade itself.
"""
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "python"))

from chromeleon.core import parse_proxy  # noqa: E402


def run(case: dict) -> dict:
    value = case["input"]["value"]
    try:
        spec = parse_proxy(value)
    except Exception as exc:                                  # noqa: BLE001
        return {"id": case["id"], "ok": False, "error": f"{type(exc).__name__}: {exc}"}
    return {
        "id": case["id"],
        "ok": True,
        "server": spec.server,
        "username": spec.username,
        "password": spec.password,
        "authenticated": spec.authenticated,
    }


def main() -> int:
    corpus = json.loads(pathlib.Path(sys.argv[1]).read_text())
    json.dump(
        {"client": "python", "results": [run(c) for c in corpus["cases"]]},
        sys.stdout,
        ensure_ascii=False,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
