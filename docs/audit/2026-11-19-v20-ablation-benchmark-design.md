# v20 Ablation Benchmark Design (Frozen)

> **Status: Design frozen before any session runs. Pre-registered; no post-hoc amendments.**

Date: 2026-11-19
Related: [KG pull-ablation results (v19 halting run)](2026-11-30-kg-pull-ablation-results.md), [prior design](2026-08-29-kg-pull-ablation-benchmark-design.md), issue #1824

## Purpose

Freeze and pre-register the v20 ablation benchmark: the first properly powered measurement of graph/structural value in this repository. It measures whether the in-tool structural tier (symbol and pattern modes) and the surviving in-tool graph mode produce named-defect catches that an rg-only arm misses, and serves as the keep/cut gate for both. The four documented confounds of the #1818 halting run are design inputs here, not footnotes: compile-visible fixtures, a budget sized from a stale estimate, harness duplication, and an undeclared provider.

## Declared deviation (up front)

**Provider/model:** all sessions run with `zai/glm-5.3-flash`. This is the declared deviation from the original pre-registered `mistral-small-2603`: no Mistral credential exists in this environment (only `ZAI_API_KEY`). It is recorded up front, before any result, rather than discovered post-run as in #1818. All findings are conditional on this model; no cross-model generalization is claimed.

## Arms

- **No-graph:** the structural tier is hidden and rejected via the existing stdio proxy (`scripts/kg-ablation/proxy.py`, unit-tested in #1809). The agent has bash, rg, and read only.
- **Optional:** the tier is exposed with a neutral availability note; natural pull is measured. Zero natural pulls in this arm is a primary adoption signal, not a footnote.
- **Forced:** a fixed structural workflow is required before review; this is the availability upper bound. Setup latency and tokens are included in NET cost.

All three arms run against the v20 fixture packs through the same proxy harness; the arms differ only in what the proxy exposes and what the pinned prompt requires.

## Task families

1. **Compile-invisible contract drift.** Type-compatible drift (same types, same arity, same names) invisible to `cargo check`. The defect is located outside the focal surface a reviewer would naturally inspect. Fixture pack: `family1-contract-drift`.
2. **Cross-file reference resolution.** A lexically collision-dense target where grep precision is low by construction; scored for precision against the exact caller/callee reference set resolvable only by the structural graph. Fixture pack: `family2-reference-resolution`.
3. **Pattern-mode call-shape queries.** Calls to any method named `reset` on types implementing trait `Node`, with same-name distractors (another trait's method and a free function). This gives #1823's pattern mode its own verdict; all prior measured evidence concerns symbol and reference resolution only. Fixture pack: `family3-pattern-mode`.
4. **Clean control.** No defect; any substantive defect report is a false positive. Fixture pack: `clean-control`.

## Fixtures and preflight

Fixtures are self-contained Rust crates under `scripts/kg-ablation/fixtures/` with a `ground-truth.json` per pack (pack id, defect symbols or reference/pattern ground truth, description). Defect ground truth lives only in `ground-truth.json` and `config.py`'s `DEFECT_SYMBOLS`, never in fixture source comments. Every pack must pass preflight (`scripts/kg-ablation/preflight.py`) before any session:

- compile-invisible: `cargo check` and `cargo test` pass on every pack, defective packs included;
- ground-truth schema valid and covered by `DEFECT_SYMBOLS`;
- grep-hostile: distinctive-literal defect-locating queries must fail to localize the defect (rg-based heuristic; dense lexical collisions and generic naming).

Preflight is fail-closed: a missing cargo binary or any failed check exits nonzero and no sessions run.

## Matrix and budget (re-registered from observed costs)

The prior cap (USD 0.069 for 24 runs) came from a stale USD 0.0023 per-session estimate and was blown about eightfold in the #1818 smoke run. Observed per-session cost with the declared provider was **USD 0.0173-0.0221 per session** (fail-closed accounting from session JSONL; four completed sessions, USD 0.0786 total).

Budget derivation: 24 cells x USD 0.0221 (observed upper bound) = USD 0.5304, plus roughly 20% operational headroom, rounded to **USD 0.64** as the pre-registered cumulative spend cap (`SPEND_CAP_USD = 0.64`). The per-run kill is re-registered at **USD 0.03**, just above the observed per-session maximum so a normal session is not killed mid-flight; 24 x 0.03 = USD 0.72 would itself breach the cap, so the cumulative cap remains the binding stop rule. The matrix is sized to 24 cells (3 arms x 4 fixture packs x 2 runs), which fits inside this cap at observed costs.

## Per-cell stop rules

- A cell whose session exceeds USD 0.03 NET is marked `per-run-cap-exceeded` and defective; the run is reported, never silently excluded.
- Cumulative spend above USD 0.64 halts the matrix immediately (`spend-cap-exceeded`).
- A session with a missing or malformed usage cost in the session JSONL is marked defective and `complete=false` (fail-closed accounting); the first incomplete run halts the matrix and is reported.
- A session hitting the 300 s timeout is killed, marked defective, and halts the matrix. **Only one runner invocation may be live per run root**: the runner takes a non-blocking `flock` on `<run_root>/.runner.lock` at startup and a second invocation exits nonzero. This makes the #1818 harness duplication defect structurally impossible.

## Activation gate

The pre-sealed activation gate (direct surfacing plus availability note, the F3-style gate from #1807 Lesson 2) is evaluated and recorded **before any F1 interpretation**. If the gate fails -- the tier was not actually reachable in the Optional/Forced arms -- the run measures wiring failure, not value, and no F1 (catch-rate) comparison may be interpreted. Graph attribution is cross-checked against the MCP proxy logs: a catch made without any tier call is attributed to the tools that made it.

## Method

Pin repository commit, fixture pack states (in-repo, so pinned by commit), prompts (per arm, in `config.py`), provider and model revision, timeout, max turns, and cache state. Capture full transcripts, MCP traffic, and per-session NET tokens and cost, fail-closed from session JSONL. Transcripts are adjudicated against named defects per the pre-registered rubric (`adjudicate.py`) before any aggregate inspection; ambiguous transcripts get `needs_review=true` with recorded evidence excerpts. Draft PRs are closed and branches deleted after collection.

## Pre-registered decision rule

Keep the structural tier only if Optional or Forced produces unique named-defect catches (or the precision win on family 2 is ≥ +0.15 F1 over the rg-only arm) that No-graph misses, with no unacceptable false-reassurance increase. Otherwise: removal issue under the #1807 playbook. Borderline results require a larger follow-up, not a verdict.

## Constraints

- No code changes to the tool surface in the benchmark PR (nothing under `crates/` is touched).
- Byte-stable output goldens and the rejected-trims list remain binding.
- Out of scope: cutting or expanding any surface; verdicts route to separate atomic PRs.
- Results are reported in `docs/audit/` with an explicit verdict per the pre-registered rule; raw results are committed and actual total spend reported.
