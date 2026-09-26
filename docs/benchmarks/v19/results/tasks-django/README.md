# v19 task set — Django snapshot (generation only, pre-pilot)

Generated 2025-09-26 (draft task set, not sealed). No pi sessions were
run; this is the Stage-0 generation + viability gate artifact only.

## Provenance

- Repo HEAD: `ab12d45` (origin/main, includes PR #1683 snapshot-fetch fix)
- aptu-coder: 0.36.1
- Snapshot: Django `dd6f6b1531984823e3dc56740dfa93f3ceb09357`
- Tarball SHA256: `9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`
- Verified with `PYTHONPATH=scripts .venv-v19/bin/python -m bench_v19.snapshots --fetch --dest-dir /tmp/v19-snapshots` (fail-closed verify passed)

## Generation commands

Oracle built via `oracle.build_callers_oracle` / `oracle.build_lookup_oracle`
over the extracted snapshot tree
(`/tmp/v19-snapshots/src/django-dd6f6b1531984823e3dc56740dfa93f3ceb09357`);
tasks generated via `generate_tasks.generate_tasks` (Track A) and
`generate_tasks.generate_track_c_tasks` (Track C), run with
`.venv-v19/bin/python` (Python 3.12, pinned tree-sitter grammars per
`scripts/bench_v19/requirements.txt`).

## Contents

- `oracle-entries.json` — 60 entries: 45 Track A (15 symbols × hop 1/2/3)
  + 15 Track C lookups. All 15 Track A symbols pass the rg-vs-tree-sitter
  agreement filter at hop 1; zero ambiguous-symbol exclusions.
- `tasks-track-a.json` — 45 Track A callers tasks (kept set).
- `tasks-track-c.json` — 15 Track C lookup tasks (the 15 oracle symbols;
  full single-answer pool from the definition index is 26,650).
- `exclusions.json` — empty (no ambiguous symbols among picked set).

## Viability verdict

VIABLE: 45 Track A + 15 Track C = 60 generated tasks, comfortably above
the ≥10 discriminative-candidate gate. Symbol selection deliberately
restricted to unambiguous function definitions with 3–20 direct caller
files where the double-extraction arms agree; earlier candidate sweeps
(class-heavy or builtin-adjacent symbols) disagreed at hop 1 and were
dropped before final generation (survivor rate ≈ 15/60 tested symbols,
consistent with the methodology's expectation that the agreement filter
is the main rejection mechanism).

Next: Stage-3 pilot applies the discriminative-power filter
(`filter_pilot_tasks`) to this draft set.
