# v19 crossover benchmark scaffold

Pilot-first scaffold for the v19 crossover benchmark per
`docs/benchmarks/v19/methodology.md`.

## Scope (pilot-first)

- Snapshot fetch and verify are separable: `snapshots.py fetch` uses the
  network, while verify (`--digests` mode) is pure-local and fail-closed
  for CI. The SHA256 manifest is frozen from day one; the pinned commits
  (Django `dd6f6b1...`, rust-clippy `f0c668a...`) are never re-pinned.
- Task generation (`generate_tasks.py`) covers the Track A callers
  template stratified by hop depth 1/2/3 plus the Track C lookup
  control, which is now implemented: Track C tasks are generated
  directly from the tree-sitter definition index (single answer by
  construction, exempt from the F1-gap rule and from the rg agreement
  cross-check). The hop-depth sweep is restricted to the callers
  template.
- The oracle (`oracle.py`) is AST-based via py-tree-sitter with pinned
  Python and Rust grammars (see `requirements.txt`). Ground truth is
  the set of files containing call nodes whose callee resolves exactly
  to the target symbol against the definition index; hop depth expands
  over these real call edges via BFS on the file-level call graph.
  Comments, docstrings, and string literals are different AST node
  types and can never match. Filename stems are never used.
- Double extraction per the methodology: ripgrep is retained ONLY as
  the independent cross-check arm. A literal `rg -F -e` search derives
  the same file set, and a Track A task is kept only when rg's file set
  equals the tree-sitter set (agreement filter; comparable directly at
  hop 1, where both arms derive a flat caller set). Disagreement drops
  the task.
- Scoring (`score.py`) is a standalone F1 set-recall scorer. It imports
  only `expected_digest` and `write_scores` from `bench_v18.score`; the
  v18 categorical helpers (`categorize`, `score_session`, `ANSWER_RE`)
  are v18-answer-format coupled and deliberately not reused.
- The runner (`runner.py`) is a thin adapter importing bench_v18
  runner/blinding/stats via `sys.path`. The arms registry is shadowed in
  v19 and bench_v18 module-level globals are never mutated. The
  `mcp-gateway` arm (`directTools: false`, identical to the PR #1621
  spec) is the tax-control arm restricted to hop-1 Track C cells.

## Deferred to post-pilot

- Track B tasks.
- Sealed-stage Track A coverage beyond the callers template (requires a
  methodology amendment).

## Oracle dependencies

`scripts/bench_v19/requirements.txt` pins the tree-sitter runtime and
grammars with `==`. Pinning is required: oracle ground truth must be
reproducible across grammar releases. The dependency is justified by the
methodology's own double-extraction mandate, which text matching cannot
satisfy (static-analysis oracle best practice: call-graph ground truth
comes from AST extraction, not text matching). All pins support Python
3.9+.

Install with `pip install -r scripts/bench_v19/requirements.txt`.

## Running

```sh
pytest scripts/tests/ -q   # fully offline
```

Snapshot fetching (maintainer-only, network required):

```sh
python -m bench_v19.snapshots --fetch --dest-dir <dir>
```
