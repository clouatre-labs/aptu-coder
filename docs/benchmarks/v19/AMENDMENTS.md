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

## A6 — 2026-09-29: Deterministic anchor verification replaces gold-membership fabricated check (scorer repair)

**Decision:** ratified pre-sealed as a measurement-validity repair,
before any sealed-stage spend relies on the scorer. Human spot-check of
the two A5 re-pilot "fabricated-anchor" verdicts (skipIfDBFeature F1
0.941, register_lookup F1 0.811) found every sampled `file:line` anchor
genuine: file exists in the snapshot, line in range, symbol identifier
present at the cited line (verified directly against the pinned
snapshot). The old scorer computed `fabricated = |predicted files not
in gold|`, which conflated ordinary false positives (a real hop-1
caller outside the gold set's scope, e.g. the django/ framework files
for register_lookup, P=0.683 R=1.000) with fabrication and forced the
`fabricated-anchor` verdict whenever precision < 1.0 — making
`correct` nearly unreachable on fan-in tasks.

Adopted change (implemented in `scripts/bench_v19/score.py`, used by
`scripts/bench_v19/pilot.py`):

- `verify_anchors(text, symbol, snapshot)`: an anchor `path:line` is
  fabricated iff the file does not exist in the snapshot, the line
  number is out of range, or the symbol does not occur within ±2 lines
  of the cited line (calls may span lines). Gold-set membership is
  never consulted for fabrication; it affects only precision/recall/F1.
- Verdict semantics unchanged: `correct` = recall ≥ 0.9 AND zero
  fabricated anchors; `partial` = recall in (0, 0.9); `no-anchor` =
  empty answer set.
- Re-score of the two affected A5 sessions under the fixed scorer:
  0 fabricated anchors each; both verdicts `correct`. F1 values
  reproduce the recorded ones exactly (0.941, 0.811), confirming the
  F1 math was unaffected — only the verdict label was wrong.
- Frozen task set `tasks-track-a-hop2-a4.json` gains a `symbol` field
  per entry (values cross-checked against `selections-a4.json`);
  metadata only, `expected_files` and prompts untouched.

## A7 — 2026-09-29: Sealed-stage fanin turn cap 25 → 40

**Decision:** ratified pre-sealed (completion problem). 5/8 A5 re-pilot
sessions hit the fanin turn cap 25 mid-loop without a final answer
(`stopReason: toolUse`, `no-anchor`, F1 = 0) even after the A5 oracle
gate confirmed tool data was correct. Turn cap 25 measurably cannot
complete fanin/hop-2 sessions, so sealing at 25 would measure turn
capacity, not tool value. The fanin tier cap in
`scripts/bench_v19/runner_v19.py` `TIERS` is raised to 40 for the
sealed stage (control tier already 40). The cap remains fail-closed:
exceedance kills the session, records `turn-cap-exceeded:<n>`, and
still meters spend. Pilot results at cap 25 are retained as recorded;
no pilot numbers are retroactively recomputed.

### A7b — 2026-09-29: sealed-stage wait deadline 900s; wait-deadline kills made visible (first sealed attempt invalidated)

**Finding (gate-relevant defect).** The first sealed attempt
(`run-sealed-hop2`, 16 sessions, $0.1482 metered) was invalidated: the
binding constraint on fanin/hop-2 sessions was the 300s wall-clock wait
deadline, not the A7 turn cap 40. All 10 sessions ending with
`stopReason: toolUse` and no answer have within-session wall spans of
245–299s; the deadline kill path in
`runner_v19.run_session_with_turn_cap` fell through with `killed=False`
and no defect entry, making deadline kills indistinguishable from
sessions the model ended mid-loop. Under A5 gate discipline (a
metering-visibility defect halts spend), the attempt is discarded from
analysis; its spend remains metered for the record.

**Adopted changes (both implemented before the re-run):**

- `runner_v19.run_session_with_turn_cap` now records a
  `wait-timeout-killed:<s>` defect with `killed=True` on deadline
  kills, mirroring turn-cap handling (fail-closed visibility).
- Sealed-stage re-run sets `SESSION_WAIT_TIMEOUT_S=900` via the
  already-ratified environment-driven override (carry-over
  ratification, "Wait deadline 120s → 300s via SESSION_WAIT_TIMEOUT_S
  environment override"); the frozen v18 default is not edited. 900s
  covers the observed 2325s outlier only partially, but with deadline
  kills now visible as defects any residual truncation is detectable
  and reportable rather than silent.

### A8 — 2026-09-29: repair re-run of provider-error sealed sessions (replacement policy)

**Trigger.** 4 of the 16 sealed sessions ended `stopReason: "error"` (provider-side failures mid-loop, not model choice): chain/mcp, dec/mcp, timezone/mcp (native arm completed in each of these cells), and include/native (mcp arm completed in that cell). These are harness defects, not outcomes; leaving them in depresses arm completion rates and F1 for reasons unrelated to the treatment.

**Adopted changes:**

- Repair re-run of exactly those 4 (task, arm) cells using a filtered task file under `/tmp` (frozen task set not modified), same binary (0.37.0 — no Rust changes since the sealed run's `da0686d` build, so the A5 oracle gate re-run is not required), same snapshot (tarball sha256 verified against the pinned manifest), `SESSION_WAIT_TIMEOUT_S=900`, run root `/tmp/v19-pilot/run-sealed-repair/{mcp,native}`.
- Replacement policy: the original error sessions are **kept** in the sealed record verbatim; repair sessions are recorded under `sealed/repairs/` with per-session before/after. Arm means are reported both ways — as-sealed, and repair-adjusted (error sessions replaced by their repair counterparts where the repair completed; an error session whose repair also fails stays as-is).
- Repair spend is metered separately and reported alongside the sealed-stage spend in the PR/analysis record.
- No retroactive re-scoring of any kept session; the A6 scorer is unchanged.

## Carry-over ratifications (pre-stage-3, recorded at stage-2)

- Wait deadline 120s → 300s via `SESSION_WAIT_TIMEOUT_S` environment
  override; the frozen v18 default is NOT edited.
- Explicit per-session turn cap, fail-closed, recorded per session.
- Track A caller prompts reworded to request `file:line` anchors.
