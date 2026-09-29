# v19 amendments manifest

Every protocol change after the frozen v18/v19 baseline is recorded here,
one entry per ratified amendment, before any pilot spend relies on it.
Nothing in this file is applied silently; each entry names the decision
and the artifacts that implement it.

## A1 — 2025-09-26: Fair-design amendment ratified (F1–F5)

**Decision:** ratified on #1681 (maintainer, 2025-09-26). Rationale: the
study's goal is an academic causal claim about aptu-coder's tool value;
a fixture pre-filtered for rg-solvability cannot support that claim
(diagnosis: `docs/benchmarks/v19/FAIR-DESIGN.md`, recorded v18 evidence:
0/4 MCP activation, ~4x schema-token overhead, tasks filtered for
rg-vs-tree-sitter agreement).

Adopted changes (implementations land in `scripts/bench_v19/` before
stage 3):

- F1: headline metric becomes cost-at-iso-quality under explicit budget
  tiers (per-tier turn/context caps); F1 remains the quality metric.
- F2: new fan-in/hop-3 task tier where grep output floods context;
  existing 45 Django Track A tasks are retained as the rg-optimal
  control tier. Task generation no longer uses rg-agreement as an
  inclusion filter for the new tier.
- F3: enforced activation gate — tool arm must issue ≥1 aptu-coder tool
  call in ≥80% of scorable sessions; below that the stage HALTS
  fail-closed (same handling as a metering defect).
- F4: prompt parity — identical task text in both arms plus one
  arm-neutral availability sentence in the tool arm only, frozen here:
  "In addition to the default tools, repository-analysis tools for
  call graphs and symbol lookup are available in this session."
- F5: within-harness ablation unchanged; MCP schema overhead is counted
  against the tool arm in cost-per-solve.

## A2 — 2025-09-26: Anchor-adjudication layer DROPPED (option c on #1681)

**Decision:** dropped on #1681 (maintainer, 2025-09-26). Rationale: the
layer failed its #1686 freeze gate (overall agreement 0.733 < 0.90;
judge near-name confusion, five-role taxonomy gap — artifacts:
`docs/benchmarks/v19/results/calibration/`). The tree-sitter AST oracle
remains the sole ground truth (deterministic, auditable); anchor
resolution reuses v18 textual-resolution logic; non-oracle anchors are
precision-penalties exactly as pre-#1686. The failed calibration is
retained as a documented negative result on LLM verification oracles.
`scripts/bench_v19/adjudicate.py` is retained but inert (no pilot path
calls it); the `jev-1.13.0` pin and all judge answers remain verbatim in
the calibration artifacts.

## A3 — 2026-09-27: Simplified single-tier sealed design (v19.1)

**Decision:** taken on #1681 (maintainer, 2026-09-27), after the stage-3
pilot (`results/pilot-0361/`). Rationale: the pilot fixed the v18
activation confound (gate PASS 100% on fan-in Track A) but showed the
ladder itself miscalibrated on task difficulty — hop-3 fan-in is
completion-bounded (9/18 sessions ended `stopReason: toolUse` with no
final answer in **both** arms), hop-1 lookups are trivially rg-solvable
(Track C activation 0/2, fail-closed halt), and n=8 cannot support a
headline claim. A simple, reproducible, discriminatory design for the
paper:

- **Single sealed tier: hop-2 fan-in Track A.** Hop-1 and hop-3 cells
  and the entire Track C tier are dropped from the sealed design.
  Selection additionally gates on the hop-2 caller-file set
  (`FANIN_MIN_SEALED_CALLER_FILES = 40`), fail-closed recorded.
- **Fixed sealed N = 12 tasks x 2 arms = 24 sessions** (answers the
  methodology's open sealed-N question; projected spend well under the
  $2.00 sealed cap).
- **Headline metric:** per-arm F1 and cost-per-task (F1/F5 unchanged);
  discriminative filter unchanged for kept-set selection.
- **Reproducibility:** pinned snapshot + pinned tree-sitter versions +
  one-command regeneration (`bench_v19.generate_tasks`);
  `generate_fanin_tasks(hop_depth=3)` reproduces the stage-3 pilot
  verbatim; sealed sessions re-run kept tasks from scratch (fresh
  sessions, no pilot session reuse).

The stage-3 pilot is retained as the hop-3/hop-1 negative result
motivating this amendment.

## A4 — 2026-09-27: Bounded answer window + gate minimum sample (v19.1 fix)

**Decision:** taken on #1681 (maintainer, 2026-09-27), after the A3
hop-2 re-pilot (run `/tmp/v19-pilot/run-fanin-hop2`, $0.0845 spend).
The re-pilot showed the A3 tier was still invalid, for a different
reason: expected answer sets of 78–848 caller files are output-bounded,
not tool-bounded (native completed 0/5, mcp 1/5; 9/10 sessions ended
`stopReason: toolUse`). A task the model structurally cannot complete
is not a hard task — it is an invalid one (SWE-bench-style feasibility
filtering). Two changes:

- **A4a — bounded answer window.** Sealed-tier selection keeps symbols
  whose hop-2 caller-file set falls in `[20, 60]` AND whose
  `rg_output_bytes >= 20,000` (flooding floor; short common symbols
  produce huge raw rg output even at moderate caller counts, and the
  byte count is recorded per task as the flooding datum). The pilot-era
  hop-1 >= 40 fan-in floor is DROPPED for the sealed tier: the offline
  scan showed it forces hop-2 sets past 60 (pool of 0); the window plus
  flooding floor are the discrimination criteria. Fail-closed
  exclusions record which side of the window rejected them.
- **Sealed N is pool-bound: 8 tasks x 2 arms = 16 sessions**
  (supersedes A3's fixed N=12). The A4a window yields exactly 8 viable
  symbols on the pinned Django snapshot (offline, zero-spend scan):
  `chain` (56 callers, 66.9 KB rg), `register_lookup` (43, 52.6 KB),
  `skipIfDBFeature` (42, 21.4 KB), `view_func` (36, 24.3 KB),
  `include` (33, 221 KB), `dec` (23, 456 KB), `qualname` (22, 26 KB),
  `timezone` (20, 163 KB).
- **A4b — activation gate minimum sample.** The F3 gate (≥0.80
  activation, fail-closed, threshold unchanged) is evaluated only once
  ≥8 scorable tool-arm sessions exist; below that it records `pending`
  and passes vacuously. Rationale: the re-pilot halted at 3/4 = 0.75 —
  one session of granularity noise — and post-hoc threshold loosening
  after a fail is not acceptable; fixing the sample size is.

The A3 re-pilot artifacts are retained as the output-boundedness
negative result motivating this amendment.

## A5 — 2026-09-28: v19 pilots invalidated by analyze_symbol cache bug; tool-as-oracle becomes a permanent pre-sealed gate

**Decision:** taken on #1681 (maintainer, 2026-09-28), after a
byte-level verification that aptu-coder 0.36.1 (and main at
`4fcf77b`) poisoned every v19 tool-arm session.

- **The bug.** `analyze_symbol` call-graph mode cached results under a
  key omitting the queried symbol and the analysis mode
  (`CallGraphCacheKey` and the blake3 L2 disk key in
  `symbol_focused.rs`). The persistent L2 disk cache served the first
  symbol's graph to every later query for a different symbol on the
  same path, with only the FOCUS line substituted. Verified on the
  pinned Django snapshot: `chain`, `timezone`, `dec`,
  `get_version_tuple` returned byte-identical paginated callers;
  reproduced on a pristine snapshot copy. Fixed in the 0.37.0 line
  (issue #1691, PR #1692) with unit + end-to-end MCP regression tests.
- **Invalidation.** All v19 tool-vs-native sessions in which the tool
  arm made ≥1 `analyze_symbol` call are VOID as comparative evidence;
  the mcp-arm numbers are poisoned, the native-arm numbers stand.
  Prior pilot spends are retained only as negative-result artifacts.
- **F3 activation gate.** Per prior ratification (recorded here now):
  killed sessions with ≥1 aptu tool call count as active for the F3
  gate.
- **Permanent gate: tool-as-oracle pre-sealed validation.** Before any
  sealed-stage spend, the fixed binary is driven over stdio JSON-RPC
  from the pinned snapshot and its per-symbol caller data is compared
  to the research oracle. The A5 run (8 sealed symbols) confirms:
  all 8 symbols return distinct, deterministic graphs (cache bug
  gone); where tool and oracle semantics align exactly
  (`skipIfDBFeature`: unambiguous symbol, identifier calls only) the
  file-level agreement is **F1 = 1.000**; spot-checked gold anchors
  are genuine (`chain(` at `django/db/models/base.py:224`,
  `view_func(` at `django/utils/decorators.py:173`). Residual
  file-level F1 gaps on `chain`/`qualname`/`timezone` are attributable
  to documented semantic scope differences, not caching: the oracle
  counts identifier calls only (attribute calls like `timezone.now()`
  create no edge), is Python-only, and excludes test callers from hop
  joins, while the tool resolves across languages and separates test
  callers into their own attribution. The tool emits caller names
  without per-caller file attribution for production callers, which
  limits file-level scoring; this is noted as a tool-surface
  observation, not a correctness defect.
- **Harness fix (recorded for provenance).** `pilot.py --arms` was
  validated but not honored (the pair `native+mcp` was hardcoded), so
  the first A5 re-pilot attempt ran both arms before being halted;
  its partial artifacts under `run-fanin-hop2-a5` are discarded and
  the run restarted mcp-arm-only after the fix. No thresholds were
  changed.

## Carry-over ratifications (pre-stage-3, recorded at stage-2)

- Wait deadline 120s → 300s via `SESSION_WAIT_TIMEOUT_S` environment
  override; the frozen v18 default is NOT edited.
- Explicit per-session turn cap, fail-closed, recorded per session.
- Track A caller prompts reworded to request `file:line` anchors.
