#!/usr/bin/env python3
"""Oracle for the v18 benchmark: 40 ground-truth answers from the fixture.

Builds ground-truth answers by locating per-function V18ANS tags in the
frozen fixture. Uses ripgrep when available and falls back to pure-Python
re scanning with a loud warning; never imports or shells out to aptu-coder.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

N_QUESTIONS = 40
TAG_RE = re.compile(r"V18ANS-(\d{4})")


def _warn_no_rg() -> None:
    print(
        "oracle: WARNING ripgrep (rg) not found on PATH; "
        "falling back to python-re scanning",
        file=sys.stderr,
    )


def _rg_search(tag: str, fixture_root: Path) -> tuple[str, int] | None:
    """Search the fixture for a tag; returns (relative path, 1-based line)."""
    pat = re.escape(tag) + r":[A-Za-z]+-\d{4}"
    if shutil.which("rg") is None:
        _warn_no_rg()
        tag_re = re.compile(pat)
        for path in sorted(fixture_root.rglob("*")):
            if not path.is_file() or path.name == "manifest.json":
                continue
            rel = path.relative_to(fixture_root).as_posix()
            for lineno, line in enumerate(
                path.read_text(encoding="utf-8").splitlines(), 1
            ):
                if tag_re.search(line):
                    return rel, lineno
        return None
    proc = subprocess.run(
        ["rg", "--no-heading", "-n", "-g", "!manifest.json", pat,
         str(fixture_root)],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0 or not proc.stdout.strip():
        return None
    first = proc.stdout.splitlines()[0]
    abspath, lineno, _ = first.split(":", 2)
    rel = Path(abspath).resolve().relative_to(fixture_root.resolve()).as_posix()
    return rel, int(lineno)


def build_oracle(fixture_root: Path) -> list[dict]:
    """Build N_QUESTIONS ground-truth entries (id, prompt, answer, anchor)."""
    manifest = json.loads(
        (fixture_root / "manifest.json").read_text(encoding="utf-8")
    )
    oracle: list[dict] = []
    for entry in manifest["answers"]:
        if len(oracle) >= N_QUESTIONS:
            break
        found = _rg_search(entry["tag"], fixture_root)
        if found is None:
            raise RuntimeError(f"oracle: tag {entry['tag']} not found in fixture")
        rel, lineno = found
        if rel != entry["path"]:
            raise RuntimeError(
                f"oracle: anchor mismatch for {entry['tag']}: "
                f"{rel} != manifest path {entry['path']}"
            )
        oracle.append({
            "id": entry["id"],
            "prompt": (
                f"Find the string constant whose value starts with "
                f"{entry['tag']}: in this repository and report the exact "
                f"full string, plus the file and line where it is defined."
            ),
            "expected": entry["answer"],
            "anchor": f"{rel}:{lineno}",
        })
    if len(oracle) < N_QUESTIONS:
        raise RuntimeError(
            f"oracle: only {len(oracle)} questions built, need {N_QUESTIONS}"
        )
    return oracle


def write_oracle(fixture_root: Path, out_path: Path) -> list[dict]:
    oracle = build_oracle(fixture_root)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(oracle, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return oracle


def main() -> None:
    repo_root = Path(__file__).resolve().parents[2]
    fixture_root = repo_root / "docs/benchmarks/v18/fixture"
    out_path = repo_root / "docs/benchmarks/v18/oracle.json"
    oracle = write_oracle(fixture_root, out_path)
    print(f"wrote {len(oracle)} ground-truth answers to {out_path}")


if __name__ == "__main__":
    main()
