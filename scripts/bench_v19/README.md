# v19 crossover benchmark scaffold

Pilot-first scaffold for the v19 crossover benchmark per
`docs/benchmarks/v19/methodology.md`.

## Scope (pilot-first)

- Snapshot fetch and verify are separable: `snapshots.py fetch` uses the
  network, while verify (`--digests` mode) is pure-local and fail-closed
  for CI. The SHA256 manifest is frozen from day one; the pinned commits
  (Django `dd6f6b1...`, rust-clippy `f0c668a...`) are never re-pinned.
- Task generation (`generate_tasks.py`) covers the Track A callers
  template stratified by hop depth 1/2/3 plus the Track C lookup control.
  The hop-depth sweep is restricted to the callers template.
- The oracle (`oracle.py`) is ripgrep-only and
  double-extraction-agnostic. The tree-sitter-based oracle is deferred to
  post-pilot.
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
- tree-sitter oracle extraction.
- Sealed-stage Track A coverage beyond the callers template (requires a
  methodology amendment).

## Running

```sh
pytest scripts/tests/ -q   # fully offline
```

Snapshot fetching (maintainer-only, network required):

```sh
python -m bench_v19.snapshots --fetch --dest-dir <dir>
```
