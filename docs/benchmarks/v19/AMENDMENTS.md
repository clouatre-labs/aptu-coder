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

## Carry-over ratifications (pre-stage-3, recorded at stage-2)

- Wait deadline 120s → 300s via `SESSION_WAIT_TIMEOUT_S` environment
  override; the frozen v18 default is NOT edited.
- Explicit per-session turn cap, fail-closed, recorded per session.
- Track A caller prompts reworded to request `file:line` anchors.
