# v19 task set — fan-in tier (A1/F2, generation only, pre-pilot)

Generated 2025-09-27 (draft task set, not sealed). No pi sessions were
run; this is the Stage-0 generation artifact for the F2 fan-in tier
only. The existing 45 Django Track A tasks remain the rg-optimal
control tier and are untouched.

## Provenance

- Repo HEAD: `bench/v19-calibration-0361` (see git log for exact sha)
- aptu-coder: 0.36.1
- Snapshot: Django `dd6f6b1531984823e3dc56740dfa93f3ceb09357`
- Tarball SHA256:
  `9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`
- Extracted snapshot tree used for generation:
  `/tmp/v19-wiring/django`

## Generation commands

```
PYTHONPATH=scripts .venv-v19/bin/python   # Python 3.12, pinned grammars per scripts/bench_v19/requirements.txt
```

Selection: `generate_tasks.select_fanin_symbols(root)` — unambiguous
Python function definitions (tree-sitter function-definition index via
`oracle.build_function_definition_index`) with >= 40 distinct hop-1
caller files (`oracle.build_call_edges`), top 10 by hop-1 caller-file
count, kept only when the hop-3 transitive caller set
(`oracle.callers_oracle_from`, same BFS semantics as
`oracle.build_callers_oracle`) has >= 60 files. Per A1/F2, the
3–20-caller filter is relaxed and rg-vs-tree-sitter agreement is NOT
an inclusion criterion for this tier. Each task records
`rg_output_bytes`, the byte size of `rg -n '<symbol>' -g '*.py'` over
the snapshot (`oracle.rg_output_bytes`) — the F2 premise justification
datum: a textual enumeration floods a normal context window.

Tasks: `generate_fanin_tasks` (tier `fanin`, hop_depth 3, amended
Track A callers wording with file:line anchors) and
`generate_fanin_track_c_tasks` (Track C lookups; the oracle supports
the single-definition lookup invariant and all selected symbols are
unambiguous, so Track C IS generated for this tier, capped at 15).

## Contents

- `tasks-track-a.json` — 8 fan-in Track A callers tasks (hop 3).
- `tasks-track-c.json` — 8 fan-in Track C lookup tasks.
- `oracle-entries.json` — 16 entries (8 Track A hop-3 + 8 Track C).
- `exclusions.json` — 38 fail-closed exclusion records (builtins and
  class constructors with high fan-in but no single unambiguous
  function definition in the snapshot; two real function definitions
  below the hop-3 threshold, listed below).

## Per-symbol table

| symbol | hop-1 caller files | hop-3 caller files | rg_output_bytes |
|---|---|---|---|
| super | 467 | 1009 | 305625 |
| type | 131 | 967 | 862664 |
| skipUnlessDBFeature | 102 | 104 | 100888 |
| mark_safe | 72 | 916 | 36258 |
| F | 68 | 120 | 4370842 |
| dict | 54 | 543 | 263259 |
| modify_settings | 45 | 187 | 19069 |
| _lazy_re_compile | 45 | 812 | 15614 |

rg_output_bytes: min 15614, median ≈ 182073 (mean ≈ 747k). Every task's
textual enumeration is at least ~15 KB and up to ~4.4 MB.

## Exclusions

- All selected symbols have hop-1 caller files >= 40; no exception.
- `register_lookup` (43 hop-1) and `skipIfDBFeature` (41 hop-1) were
  dropped because their hop-3 caller sets (43 and 42 files) fall below
  the F2 context-flooding threshold of 60; the tier therefore carries 8
  symbols, not 10 — no symbol was padded below threshold.
- Builtin/library callees (`len` 428, `str` 380, `isinstance` 345, ...)
  and class constructors (`HttpResponse`, `Value`, `Q`, ...) were
  excluded fail-closed: not a single unambiguous function definition in
  the snapshot. Full list in `exclusions.json`.
- Zero ambiguous-symbol exclusions among picked symbols.

## Sanity verdict

VIABLE: JSON parses; every `expected_files` set in
`tasks-track-a.json` reproduces exactly from the tree-sitter oracle
(`callers_oracle_from` hop 3); Track C `expected_files` are the
single defining files; all 8 tasks satisfy hop-1 >= 40 and hop-3 >= 60.
