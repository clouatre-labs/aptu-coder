"""Tree-sitter callers oracle for the v19 benchmark (double extraction).

Ground truth is AST-based, per the v19 methodology's double-extraction
mandate and static-analysis oracle best practice: text matching cannot
produce call-graph ground truth. Definition and call sites are captured
with per-language tree-sitter queries, so comments, docstrings, and
string literals can never match (they are different AST node types).
Hop depth is expanded over real call edges via BFS on the file-level
call graph; filename stems are never used.

ripgrep remains ONLY as the independent cross-check arm: a caller file
set is also derived with a literal (``rg -F -e``, ``--``-separated)
search and a task is kept only when rg's file set equals the
tree-sitter set (the double-extraction agreement filter).

Supported languages (pilot): Python and Rust via pinned grammar
packages (see requirements.txt). Never shells out to aptu-coder.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
from collections import defaultdict
from pathlib import Path

import tree_sitter_python
import tree_sitter_rust
from tree_sitter import Language, Node, Parser, Query

HOP_DEPTHS = (1, 2, 3)

LANGUAGES: dict[str, Language] = {
    ".py": Language(tree_sitter_python.language()),
    ".rs": Language(tree_sitter_rust.language()),
}

# Per-language captures of definition names and (identifier) callees.
# Calls through attributes/methods are intentionally out of scope for
# the pilot (identifier callees only).
_DEFINITION_QUERIES: dict[str, str] = {
    ".py": (
        "(function_definition name: (identifier) @name)"
        "(class_definition name: (identifier) @name)"
    ),
    ".rs": (
        "(function_item name: (identifier) @name)"
        "(struct_item name: (type_identifier) @name)"
        "(enum_item name: (type_identifier) @name)"
    ),
}

_COMMENT_MARKERS = ("#", "//", "/*", "*", "<!--")

# Heuristic definition-line detector for the rg arm only (tree-sitter is
# authoritative; this keeps the textual cross-check comparable by
# excluding definition, comment, and doc-only lines).
_DEFINITION_RE = re.compile(r"^\s*(?:async\s+)?(?:def|fn|class|func|function)\s+(\w+)")

_CALL_QUERIES: dict[str, str] = {
    ".py": "(call function: (identifier) @callee)",
    ".rs": "(call_expression function: (identifier) @callee)",
}


def _validate_symbol(symbol: str) -> str:
    """Reject empty/control-character symbols; metacharacters/dashes are fine."""
    if not symbol or not symbol.strip():
        raise ValueError("oracle: symbol must be a non-empty string")
    if any(ch in symbol for ch in "\n\r\t\x00"):
        raise ValueError("oracle: symbol must not contain control characters")
    return symbol


def _query_for(table: dict[str, str], suffix: str) -> Query:
    return LANGUAGES[suffix].query(table[suffix])


def _captured_names(root: Node, query: Query) -> list[str]:
    # captures() returns {capture_name: [nodes]} in py-tree-sitter 0.23.
    return [
        node.text.decode("utf-8")
        for nodes in query.captures(root).values()
        for node in nodes
    ]


def _iter_source_files(root: Path) -> list[tuple[Path, str]]:
    """Deterministic (path, suffix) list for supported source files."""
    return sorted(
        (path, path.suffix)
        for path in root.rglob("*")
        if path.is_file() and path.suffix in LANGUAGES
    )


def _parse(path: Path, suffix: str):
    parser = Parser(LANGUAGES[suffix])
    return parser.parse(path.read_bytes())


def build_definition_index(snapshot_root: Path) -> dict[str, list[str]]:
    """symbol -> sorted defining-file list (tree-sitter definition names)."""
    index: dict[str, set[str]] = defaultdict(set)
    for path, suffix in _iter_source_files(snapshot_root):
        rel = path.relative_to(snapshot_root).as_posix()
        query = _query_for(_DEFINITION_QUERIES, suffix)
        tree = _parse(path, suffix)
        for name in _captured_names(tree.root_node, query):
            index[name].add(rel)
    return {name: sorted(files) for name, files in sorted(index.items())}


def build_call_edges(snapshot_root: Path) -> list[tuple[str, str]]:
    """Sorted (caller-file, callee-symbol) edges from tree-sitter call nodes."""
    edges: set[tuple[str, str]] = set()
    for path, suffix in _iter_source_files(snapshot_root):
        rel = path.relative_to(snapshot_root).as_posix()
        query = _query_for(_CALL_QUERIES, suffix)
        tree = _parse(path, suffix)
        for callee in _captured_names(tree.root_node, query):
            edges.add((rel, callee))
    return sorted(edges)


def direct_callers(call_edges: list[tuple[str, str]], symbol: str) -> set[str]:
    """Files containing a call node whose callee is exactly ``symbol``."""
    return {caller for caller, callee in call_edges if callee == symbol}


def callers_oracle(snapshot_root: Path, symbol: str, hop_depth: int) -> list[str]:
    """Sorted caller file set at the given hop depth via BFS on call edges.

    hop 1 is the direct callers; hop k expands over files that call
    symbols defined in the previous hop's caller files. The definition
    file itself is never a caller.
    """
    if hop_depth not in HOP_DEPTHS:
        raise ValueError(f"hop depth must be one of {HOP_DEPTHS}")
    _validate_symbol(symbol)
    index = build_definition_index(snapshot_root)
    edges = build_call_edges(snapshot_root)
    callers_of_symbol: dict[str, set[str]] = defaultdict(set)
    symbols_defined_in: dict[str, set[str]] = defaultdict(set)
    for caller, callee in edges:
        callers_of_symbol[callee].add(caller)
    for defined, files in index.items():
        for file in files:
            symbols_defined_in[file].add(defined)
    definition_files = set(index.get(symbol, ()))

    current = direct_callers(edges, symbol) - definition_files
    seen = set(current)
    for _ in range(hop_depth - 1):
        frontier: set[str] = set()
        for file in sorted(current):
            for defined in sorted(symbols_defined_in.get(file, ())):
                frontier |= callers_of_symbol.get(defined, set())
        frontier -= seen
        frontier -= definition_files
        seen |= frontier
        current = frontier
    return sorted(seen)


def _rg_matches(symbol: str, root: Path) -> dict[str, list[str]]:
    """source-file -> matching lines for symbol (literal rg -F -n)."""
    if shutil.which("rg") is None:
        raise RuntimeError("oracle: ripgrep (rg) is required for the cross-check")
    argv = ["rg", "--no-heading", "-n", "-F"]
    for suffix in sorted(LANGUAGES):
        argv.extend(["-g", f"*{suffix}"])
    argv.extend(["-e", symbol, "--", str(root)])
    proc = subprocess.run(argv, capture_output=True, text=True, check=False)
    if proc.returncode not in (0, 1):
        raise RuntimeError(f"oracle: rg cross-check failed: {proc.stderr.strip()}")
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


def rg_caller_files(snapshot_root: Path, symbol: str) -> set[str]:
    """Independent ripgrep arm: files with a non-definition, non-comment
    literal mention of symbol (definition sites and comments excluded so
    the arm is comparable to the tree-sitter caller set)."""
    _validate_symbol(symbol)
    callers: set[str] = set()
    for rel, texts in _rg_matches(symbol, snapshot_root).items():
        if any(
            _DEFINITION_RE.match(t) and _DEFINITION_RE.match(t).group(1) == symbol
            for t in texts
        ):
            continue
        if any(not t.lstrip().startswith(_COMMENT_MARKERS) for t in texts):
            callers.add(rel)
    return callers


def crosscheck_agrees(
    snapshot_root: Path, symbol: str, expected_files: list[str]
) -> bool:
    """True when the rg arm's file set equals the tree-sitter ground truth."""
    return rg_caller_files(snapshot_root, symbol) == set(expected_files)


def build_callers_oracle(snapshot_root: Path, symbol: str) -> list[dict]:
    """Oracle entries for all hop depths, each carrying the agreement flag.

    The rg-vs-tree-sitter agreement is computed at hop 1, where both
    arms derive a directly comparable flat caller set; it is attached to
    every hop of the symbol (a textual-mention mismatch flags impurity
    regardless of hop depth).
    """
    symbol = _validate_symbol(symbol)
    agrees = crosscheck_agrees(
        snapshot_root, symbol, callers_oracle(snapshot_root, symbol, 1)
    )
    entries = []
    for hop in HOP_DEPTHS:
        expected = callers_oracle(snapshot_root, symbol, hop)
        entries.append(
            {
                "id": f"q-a-hop{hop}-{symbol}",
                "track": "A",
                "template": "callers",
                "symbol": symbol,
                "hop_depth": hop,
                "expected_files": expected,
                "crosscheck_agrees": agrees,
            }
        )
    return entries


def build_lookup_oracle(snapshot_root: Path, symbol: str) -> dict:
    """Track C entry: defining file straight from the tree-sitter index."""
    symbol = _validate_symbol(symbol)
    index = build_definition_index(snapshot_root)
    return {
        "id": f"q-c-loc-{symbol}",
        "track": "C",
        "template": "lookup",
        "symbol": symbol,
        "hop_depth": None,
        "expected_files": index.get(symbol, []),
    }


def write_oracle(entries: list[dict], out_path: Path) -> None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(entries, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
