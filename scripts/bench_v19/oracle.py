"""Ripgrep-based callers oracle for the v19 benchmark (rg-only for pilot).

Ground truth is the set of files referencing a symbol transitively at a
configurable hop depth; hop 1 is direct callers and hop k expands the
previous hop's caller set. tree-sitter extraction is deferred to
post-pilot; never shells out to aptu-coder.

Pilot limitation (rg-only): matching is textual. Definition sites,
comment lines, and doc-only files are excluded, but string-literal
mentions and multi-line doc comments cannot be excluded without a real
parser -- an accepted imprecision until the tree-sitter oracle lands.

Safety: the symbol is searched as a fixed string (``rg -F -e``) and
``--`` precedes positional arguments, so regex metacharacters or leading
dashes can never corrupt the match.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
from pathlib import Path

HOP_DEPTHS = (1, 2, 3)

# Source extensions eligible for oracle matches (docs/config excluded).
SOURCE_EXTENSIONS = (
    "py",
    "rs",
    "go",
    "ts",
    "tsx",
    "js",
    "java",
    "kt",
    "cs",
    "c",
    "cc",
    "cpp",
    "h",
    "hpp",
)

_COMMENT_MARKERS = ("#", "//", "/*", "*", "<!--")

_DEFINITION_RE = re.compile(r"^\s*(?:async\s+)?(?:def|fn|class|func|function)\s+(\w+)")


def _validate_symbol(symbol: str) -> str:
    """Reject empty/control-character symbols; metacharacters/dashes are fine."""
    if not symbol or not symbol.strip():
        raise ValueError("oracle: symbol must be a non-empty string")
    if any(ch in symbol for ch in "\n\r\t\x00"):
        raise ValueError("oracle: symbol must not contain control characters")
    return symbol


def _rg_matches(symbol: str, root: Path) -> dict[str, list[str]]:
    """source-file -> matching lines for symbol (fixed-string, rg -n)."""
    if shutil.which("rg") is None:
        raise RuntimeError("oracle: ripgrep (rg) is required for the pilot")
    argv = ["rg", "--no-heading", "-n", "-F"]
    for ext in SOURCE_EXTENSIONS:
        argv.extend(["-g", f"*.{ext}"])
    argv.extend(["-e", symbol, "--", str(root)])
    proc = subprocess.run(argv, capture_output=True, text=True, check=False)
    if proc.returncode not in (0, 1):
        raise RuntimeError(f"oracle: rg failed: {proc.stderr.strip()}")
    matches: dict[str, list[str]] = {}
    root_resolved = root.resolve()
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        path_part, _, rest = line.partition(":")
        _lineno, _, text = rest.partition(":")
        rel = Path(path_part).resolve().relative_to(root_resolved).as_posix()
        matches.setdefault(rel, []).append(text)
    return matches


def _is_definition(text: str, symbol: str) -> bool:
    match = _DEFINITION_RE.match(text)
    return match is not None and match.group(1) == symbol


def _caller_sites(matches: dict[str, list[str]], symbol: str) -> set[str]:
    """Files with >=1 non-definition, non-comment mention of symbol."""
    callers: set[str] = set()
    for rel, texts in matches.items():
        if any(_is_definition(t, symbol) for t in texts):
            continue  # definition site, not a caller
        if any(not t.lstrip().startswith(_COMMENT_MARKERS) for t in texts):
            callers.add(rel)
    return callers


def callers_oracle(snapshot_root: Path, symbol: str, hop_depth: int) -> list[str]:
    """Sorted caller file set for symbol at the given hop depth (deterministic)."""
    if hop_depth not in HOP_DEPTHS:
        raise ValueError(f"hop depth must be one of {HOP_DEPTHS}")
    _validate_symbol(symbol)
    current = _caller_sites(_rg_matches(symbol, snapshot_root), symbol)
    seen = set(current)
    for _ in range(hop_depth - 1):
        frontier = {
            stem for path in current if (stem := Path(path).stem) and stem != symbol
        } - seen
        if not frontier:
            break
        nxt: set[str] = set()
        for token in sorted(frontier):
            nxt |= _caller_sites(_rg_matches(token, snapshot_root), token)
        frontier = nxt - seen
        seen |= frontier
        current = seen
    return sorted(current)


def build_callers_oracle(snapshot_root: Path, symbol: str) -> list[dict]:
    """Oracle entries for all hop depths: id, track, symbol, hop, expected."""
    return [
        {
            "id": f"q-a-hop{hop}-{symbol}",
            "track": "A",
            "template": "callers",
            "symbol": symbol,
            "hop_depth": hop,
            "expected_files": callers_oracle(snapshot_root, symbol, hop),
        }
        for hop in HOP_DEPTHS
    ]


def write_oracle(entries: list[dict], out_path: Path) -> None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(entries, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
