# tasks-fanin-hop2-nrun — HALTED before spend (2026-10-03)

Status: **halted before any LLM spend.** The A11 ratified design
(n=23 pairs per arm, same frozen A4 selection procedure) cannot be
constructed on the pinned Django snapshot (`dd6f6b1`, tarball sha256
`9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`,
re-verified against `bench_v19.snapshots.SNAPSHOT_MANIFEST` before this
probe).

## Finding: pool exhaustion

The A4a sealed-tier filters (hop-2 caller-file window `[20, 60]`,
`rg_output_bytes >= 20,000`, unambiguous single Python function
definition, hop-3 gate dropped per A4a, ordering by descending hop-1
caller-file count) yield **exactly 8 viable symbols on the entire
snapshot**. An exhaustive probe over every callee in the call-edge
index (3,810 fail-closed exclusions recorded in
`selections-a11.json.exhaustion-probe.json`) returns the same 8 symbols
as the frozen `selections-a4.json`, in the same order, with
byte-identical gating fields (hop-1 counts, hop-2 counts, hop-2
expected file sets):

`register_lookup` (43), `skipIfDBFeature` (41), `include` (33),
`chain` (56), `view_func` (36), `qualname` (22), `dec` (23),
`timezone` (20).

The A11 requirement that the 23-task set be a superset of the frozen
8 is therefore satisfiable only trivially (23 > 8 is unreachable);
n=23 requires widening the ratified A4a window or floor, which the
A11 text and this run's binding specification both forbid ("same
frozen selection procedure (A4)"). Per the spec's halt rule (spec
violation before spend), the sealed stage was not started.

## Zero-spend verification performed (per spec preamble)

- `aptu-coder` binary: 0.39.0 (matches workspace `Cargo.toml` at HEAD
  `12c2324`; the stale `pilot.py` `APTU_CODER_VERSION` constant was
  corrected 0.38.0 -> 0.39.0 per the A9b caveat-2 precedent).
- Snapshot tarball re-fetched and hash-verified against the pinned
  manifest. The prior sessions-cwd checkout (`/tmp/v19-wiring/django`)
  had zero content diffs against a fresh extraction of the verified
  tarball (only dotfiles missing from its original extraction); the
  fresh extraction is the sessions cwd of record.
- Harness tests: `PYTHONPATH=scripts .venv-v19/bin/python -m pytest
  scripts/tests/ -q` green.
- Selection reproduction: `select_fanin_symbols(snapshot,
  max_symbols=8, min_hop1=0, min_hop3=None)` reproduces
  `selections-a4.json` exactly on all gating fields. The only drifting
  datum is `rg_output_bytes` (ripgrep 15.2.0; byte totals include the
  absolute snapshot path prefix, which differed in the original run).
  This datum is F2 justification metadata only and gates nothing here.

## Artifacts (zero-spend)

- `selections-a11.json` — the exhaustive pool (8 selections; NOT the
  A11 n=23 set; retained as probe evidence only).
- `tasks-track-a-hop2-a11.json` — tasks for the 8-symbol pool,
  byte-identical to the frozen `tasks-track-a-hop2-a4.json` on every
  field (verified programmatically).
- `selections-a11.json.exhaustion-probe.json` — fail-closed exclusion
  ledger of the exhaustive probe.

## Decision requested on #1681 / PR #1732

Either (a) re-derive n<=8 and amend A11's sample size with the pool
exhaustion as the recorded reason, (b) ratify a widened window (e.g.
hop-2 set in `[61, N]` or a lower rg floor) as a new amendment with
the resulting pool documented, or (c) accept the sealed 8-pair result
as final. No spend has occurred against the $0.95 stage allowance.
