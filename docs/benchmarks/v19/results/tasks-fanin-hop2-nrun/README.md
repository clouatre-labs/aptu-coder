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

---

# A11b re-probe — HALTED again before spend (2026-10-03)

Status: **halted before any LLM spend.** The A11b widened window
(hop-2 caller-file set in `[10, 60]`, rg floor >= 20,000 bytes,
unambiguous-single-definition invariant, descending hop-1 caller-file
count ordering) was executed exactly as ratified; the resulting pool is
**12 viable symbols, fewer than the ratified n=23**. Per the binding
halt rule ("halt before spend if the widened pool yields fewer than 23
viable symbols"), the sealed stage was not started.

## Zero-spend verifications performed

- `aptu-coder` binary: the installed binary had drifted to 0.37.0; it
  was rebuilt from workspace HEAD `117f110` via
  `cargo install --path crates/aptu-coder --profile release` and now
  reports 0.39.0, matching the workspace `Cargo.toml` and the
  `pilot.py` constant.
- Harness tests: 173 passed (`PYTHONPATH=scripts .venv-v19/bin/python
  -m pytest scripts/tests/ -q`).
- Snapshot: tarball re-fetched and SHA256-verified
  (`9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`).
  The prior sessions cwd (`/tmp/v19-wiring/django`) is missing 7,087
  files from the tarball; a fresh extraction with zero content diffs
  against the verified tarball is the snapshot of record
  (`/tmp/v19-nrun/django-dd6f6b1...`).

## Finding 1: pool still exhausted (12 < 23)

The widened window admits 4 new symbols over the frozen 8:

`function` (hop1=7, hop2=10), `handler` (hop1=5, hop2=10),
`formset_factory` (hop1=4, hop2=15), `back` (hop1=1, hop2=13)

for a full viable pool of 12:

`register_lookup` (43), `skipIfDBFeature` (41), `include` (33),
`chain` (11), `view_func` (7), `function` (7), `qualname` (6),
`handler` (5), `formset_factory` (4), `dec` (2), `timezone` (1),
`back` (1) — hop-1 caller-file counts in parens; hop-2 counts 43, 42,
33, 56, 36, 10, 22, 10, 15, 23, 20, 13 respectively. 3,806 candidates
were fail-closed excluded (full ledger in the probe artifact).

## Finding 2: A11b's "frozen 8 remain the head" is falsified

Selection orders by descending hop-1 caller-file count. The newly
admitted symbols interleave ahead of frozen members: `function`
(hop1=7) slots ahead of `qualname` (hop1=6); `handler` (5) and
`formset_factory` (4) follow. Only the first 5 frozen symbols
(`register_lookup`, `skipIfDBFeature`, `include`, `chain`,
`view_func`) retain their positions. The only drifting gating-neutral
datum remains `rg_output_bytes` (absolute snapshot path prefix;
F2 justification metadata only).

## Artifacts (zero-spend)

- `selections-a11b-widened-pool-probe.json` — full widened-window pool
  (12 selections, 3,806 fail-closed exclusions), probe metadata, and
  the `frozen8_head_intact: false` flag.

## Decision requested on #1681

The A11b relaxation is insufficient: no n=23 set exists on this
snapshot under any window relaxation that preserves the rg floor,
since at most 12 symbols on the entire snapshot pass
rg-output >= 20,000 + unambiguity with a nonempty in-window hop-2 set.
Options: (a) re-derive n <= 12 and amend A11's sample size (post-hoc
power must then be re-evaluated at the pilot SD), (b) lower the rg
floor as a new amendment (this dilutes the grep-hostile property A11b
explicitly declined to dilute and needs fresh justification), or
(c) ratify the sealed 8-pair result as final. No spend has occurred
against the $0.95 stage allowance.

---

# A11c stratified N-run — HALTED before spend (2026-10-03)

Status: **halted before any LLM spend.** All zero-spend step-1
verifications were executed per the ratified execution order; one
failed: the Django 6.1.1 tarball SHA256 recomputed from a fresh fetch
does not match the full digest string in the run order. The order's
binding rule is "recompute and record; halt on mismatch," so neither
stratum was started.

## Zero-spend verifications performed

- `aptu-coder` binary: 0.39.0 (matches workspace `Cargo.toml` 0.39.0;
  no rebuild required).
- Harness tests: 173 passed
  (`PYTHONPATH=scripts .venv-v19/bin/python -m pytest scripts/tests/ -q`).
- Stratum 1 tarball (dd6f6b1): re-fetched, SHA256
  `9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`
  — matches the pinned manifest. A fresh extraction is byte-identical
  (`diff -rq`, exit 0, no output) to the extraction of record at
  `/tmp/v19-nrun/django-dd6f6b1531984823e3dc56740dfa93f3ceb09357`.
- Stratum 2 tarball (6.1.1): re-fetched twice from
  `https://github.com/django/django/archive/refs/tags/6.1.1.tar.gz`;
  the two fetches are byte-identical (`cmp` clean), so GitHub's tag
  archive digest is deterministic and the artifact is stable.
  Recomputed SHA256:
  `32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77604cdc48b84007d73`.

## The digest mismatch (halt cause)

Run-order expected:
`32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77704cdc48b84007d73`
Recomputed (twice, byte-stable):
`32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77604cdc48b84007d73`

The strings differ at exactly one hex nibble (`...e3b5b777|04...` vs
`...e3b5b776|04...`). The ratified A11c amendment text pins only the
prefix `32e24244...`, which the recomputed digest matches in full;
the full 64-hex-digit string appears nowhere in the repository. The
recomputed digest is the verified artifact digest; the run-order
string is consistent with a single-character transcription error in
the order itself. Resolution requires the order's owner to re-issue
the expected digest (or ratify the recomputed digest by amendment);
per the halt rule the run did not proceed on its own interpretation.

## 6.1.1 selection re-probe (zero-spend, evidence for the re-run)

The same selection probe
(`select_fanin_symbols`, window `[10,60]`, rg floor 20,000 bytes,
unambiguity, descending hop-1 order, pinned tree-sitter deps) was run
on a fresh extraction of the verified 6.1.1 tarball and **reproduces
exactly 8 viable symbols** in the ratified order with 3,777 fail-closed
exclusions:

`register_lookup` (hop1=43, hop2=43), `include` (32, 32),
`chain` (12, 57), `function` (7, 10), `formset_factory` (4, 15),
`dec` (2, 23), `timezone` (1, 20), `back` (1, 22).

This matches the A11c probe pool symbol-for-symbol and order-for-order.

## Spend

$0.00. No sessions were run against either stratum; the $0.71 stage
cap is untouched. No artifacts beyond this halt record were emitted;
no selections/tasks/manifest files were written for either stratum
(the per-stratum artifact set is only valid once the digest question
above is resolved and the snapshot manifest pin is ratified against
the final digest string).
