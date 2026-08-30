# KG Pull-Only Ablation Benchmark Design

> **Status: Proposed design; not yet executed; telemetry prerequisite.**

Date: 2026-08-29  
Related: [KG consolidation readiness](2026-08-27-kg-consolidation-readiness.md), [KG benchmark v2 — cost and value](https://github.com/clouatre-labs/aptu/blob/main/docs/audit/2026-08-28-kg-benchmark-v2-roi.md), [telemetry issue #1491](https://github.com/clouatre-labs/aptu-coder/issues/1491)

## Purpose

Three aptu injection-model audits found no measured value in graph context. The aptu-coder
surface is different: its graph is pull-only, exposed through `analyze_symbol` and MCP graph
resources rather than injected into ordinary analysis. That pull model has not yet been
validated or measured at the agent level.

The adoption telemetry gap is tracked in [issue #1491](https://github.com/clouatre-labs/aptu-coder/issues/1491).
Until resource reads are recorded, a server log cannot distinguish an agent that did not use the
graph from one whose resource reads were simply unrecorded. This design therefore makes telemetry
a prerequisite for execution, while leaving the benchmark design itself ready for approval.

## Design

Use an external MCP-proxy ablation. The production server has no runtime configuration for hiding
individual tools or resources, so the proxy changes only the client-visible surface and does not
modify aptu-coder. Run equivalent agent sessions against the real stdio MCP server through the
proxy.

### Arms

- **No-graph:** Remove `analyze_symbol` from `tools/list` and reject `analyze_symbol`,
  `resources/list`, `resources/templates/list`, and graph `resources/read` requests. Give the
  agent no graph instruction. This is the real no-graph arm. An instruction-only prohibition is a
  weaker fallback and must be labeled as such; it does not make the advertised surface
  unavailable.
- **Optional-graph:** Expose the unchanged `analyze_symbol` and graph resource surface. Give the
  agent a neutral instruction that graph context is available on request. Do not inject graph
  output or make automatic calls. This measures the natural pull behavior.
- **Forced-graph:** Expose the graph surface and require a fixed graph workflow before review:
  call `analyze_symbol` on the named changed symbol(s), then read relevant graph resources
  (blast-radius or bidirectional where applicable), delivering all returned pages to the agent.
  The agent may still perform normal analysis. This arm is an availability upper bound, not
  natural pull behavior; include its setup latency and tokens in NET cost.

### Fixtures

Create four throwaway draft PRs against `clouatre-labs/aptu-coder`, never merge them, and close
the PRs and delete their branches after data collection. Use the following sibling shapes, with
the cross-file broken caller emphasized because it was 0/4 in the aptu audits.

| Fixture | Candidate construction | Graph value |
| --- | --- | --- |
| Broken caller | Change a public function signature in `crates/aptu-coder-core/src/validation.rs` or a small core helper, while leaving a caller in `crates/aptu-coder/src/tools/analyze_file.rs` or `tools/analyze_module.rs` using the old signature. Keep the caller outside the diff. | An incoming cross-file caller edge exposes a stale caller invisible to diff-only review. |
| Dead code path | Remove or rename a helper in `crates/aptu-coder/src/tools/common.rs` or a core utility while retaining a call in a different handler, such as `analyze_module.rs` or `analyze_file.rs`. Keep the surviving caller outside the changed-file diff. | An incoming edge exposes a dangling call; ensure no same-file reference makes it trivially visible. |
| Wrong trait impl | Change a method signature in a core language or analysis trait and leave a separate implementation in another language handler, such as `crates/aptu-coder-core/src/languages/mod.rs` plus `languages/rust.rs` or another existing implementation, with the wrong return/type contract. | Trait/implementation relationships and def-use can expose the mismatch. |
| Clean control | Make a similarly sized harmless refactor in the same crate/file classes, with no defect or cross-file contract break. | Negative control for false positives and false reassurance. |

Before opening any PR, validate each fixture against the current symbols and paths. Run
`cargo check`/`cargo test` as appropriate and have a human verify the named defect. Confirm that
the defect is real, that its relevant edge is outside the diff where specified, and that the clean
control is harmless. Do not reuse exact paths if they make the defect visible in the diff.

### Method and capture

Run **24 sessions**: 3 arms x 4 fixtures x 2 independent runs. Use `mistral-small-2603` for
cross-audit comparability, and record the exact provider/model revision. Pin the prompt,
temperature/seed where supported, repository commit, fixture PR, timeout, and maximum turns.
Capture the full agent transcript and every MCP request/response, including resource reads, from
equivalent repository and server states. Record cold/warm cache state explicitly and do not
silently exclude setup calls.

At the agent layer, account for **NET tokens**: agent prompt tokens, completion tokens, and tool-
result tokens/context attributable to the run. Include no-graph exploration cost and forced graph
outputs. Report per-run input, output, total tokens, and `cost_usd` when available, plus deltas by
fixture and arm. Server JSONL is supplementary operational data, not a substitute for agent-level
accounting.

Use the JSONL fields documented in [`docs/METRICS.md`](../METRICS.md): `tool`, `duration_ms`,
`output_chars`, `result`, `cache_hit`, `cache_tier`, `session_id`, `seq`, graph mode flags
(`match_mode`, `follow_depth`, `import_lookup`, `def_use`, `impl_only`), pagination/summary, and
`output_truncated`. Compare graph-call and resource-read counts from the harness, duration and
output size by arm, cache cold/warm behavior, truncation incidents, agent-side tokens and cost,
and named-defect outcomes.

The JSONL limits are material: it cannot provide model prompt/completion tokens, agent
exploration/search cost, semantic transcript scoring, or resource-read events under the current
schema. `output_chars` is returned text size, not model token count. Resource reads currently have
no tool enum entry or metrics emission, so telemetry must land before execution.

## Pre-registered scoring

Score transcripts against each fixture's named defect before inspecting aggregate arm results.
Two reviewers are preferable; with one reviewer, preserve the evidence excerpt and adjudication
notes. Score each run as binary for the named-defect catch and record the following outcomes:

- **Named-defect catch:** Identifies the concrete stale caller, dangling call, or trait contract
  mismatch; a generic “check callers” hedge is not a catch.
- **False positive:** Flags a non-defect on a defective fixture, or any defect on the clean control.
  Separate harmless style comments.
- **False negative:** The named defect exists and no comment or action identifies it.
- **False reassurance:** Explicitly claims that the relevant caller or implementation was updated,
  unused, compatible, or otherwise safe when that claim is false.

Report fixture-by-arm tables and counts of catch, false positive, false negative, and false
reassurance. Verdicts alone carry no signal: an approve/pass verdict is not a catch.

## Cost ceiling

Sibling per-run costs were approximately USD 0.0002-0.0023. The 24-run baseline ceiling is USD
0.0552 (24 x 0.0023), excluding harness and engineering cost. Reserve 25% operational headroom
and cap planned model spend at **USD 0.069**. Stop if actual spend exceeds the cap or if a run is
not transcript-complete; report actual total spend.

## Decision rule

Retain the pull surface only if Optional or Forced produces unique named-defect catches that
No-graph misses, with no unacceptable increase in false reassurance. Require telemetry and
fixture follow-up for borderline results. If no graph arm catches anything No-graph misses, or
if graph arms produce false reassurance on seeded defects, do not expand graph visibility;
preserve current pull-only compatibility pending a separately approved removal decision.

## Limitations

- **Small N:** 24 runs, four fixtures, and two stochastic runs per arm/fixture are directional;
  borderline results require a larger follow-up.
- **Single model:** `mistral-small-2603` does not establish generalization to other models or
  providers.
- **Synthetic fixtures:** Hand-written defects test targeted structural shapes, not their
  frequency or behavior in naturally occurring PRs.
- **Proxy-induced discoverability distortion:** Filtering and exposing MCP capabilities through a
  proxy can affect how the agent discovers them, even when server behavior is unchanged.
- **Forced-arm upper-bound bias:** Required graph calls overestimate natural pull behavior and
  are not evidence that agents would discover or choose the workflow unaided.

## Reproduction

The following is a harness sketch, not an implementation commitment. The proxy must capture MCP
traffic and preserve the arm-specific behavior while the runner records transcripts and agent-
layer token accounting.

```bash
# Preflight each throwaway fixture branch/PR before opening the benchmark run.
# Confirm the named defect, clean control, and cargo check/test result by human review.

for arm in no-graph optional-graph forced-graph; do
  for fixture in broken-caller dead-code-path wrong-trait-impl clean-control; do
    for run in 1 2; do
      run_agent \
        --model mistral-small-2603 \
        --arm "$arm" \
        --fixture-pr "$fixture" \
        --mcp-proxy ./harness/proxy \
        --capture transcript.jsonl,mcp.jsonl,metrics.jsonl \
        --record-net-tokens --record-cost
    done
  done
done

# Score transcripts using the pre-registered rubric before aggregating arm results.
# Close the four draft PRs and delete their throwaway branches after collection.
```

## Prerequisites

1. Land and verify [issue #1491](https://github.com/clouatre-labs/aptu-coder/issues/1491), which
   records MCP graph resource reads in JSONL/OpenTelemetry metrics without recording raw URIs or
   graph contents. This telemetry is required before runs so adoption is measurable; it is not a
   prerequisite for approving this design.
2. Complete fixture preflight validation against the current repository symbols and paths. Confirm
   each named defect with `cargo check`/`cargo test` and human inspection before opening draft PRs.
3. Pin the model/provider revision, repository commit, prompts, runner limits, proxy version, and
   cache-state procedure. Obtain permission for the visible GitHub actions, then close all draft
   PRs and delete branches after the benchmark.
