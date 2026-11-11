# KG Pull-Ablation Benchmark Results

> **Status: Executed to the pre-registered stop rule; verdict NOT-SUPPORTED for cutting; full-cost re-run required before any KG removal.**

Date: 2026-11-30
Related: [KG pull-ablation benchmark design](2026-08-29-kg-pull-ablation-benchmark-design.md), issue #1809

## Purpose

Execute the pre-registered KG pull-only ablation benchmark: measure whether
the pull-only graph surface (`analyze_symbol` + graph MCP resources, ~2,991
LOC across `resources.rs`, `structural.rs`, `store.rs`) produces
named-defect catches that a no-graph agent misses, as keep-or-cut evidence
for the graph surface.

## Execution status

The harness (scripts/kg-ablation/, standalone stdlib Python) is complete and
unit-tested (17 tests: proxy surface-hiding, passthrough arms, adjudicator
rubric, session matrix). The four fixture draft PRs were constructed,
validated, and opened as drafts (and closed and deleted after collection, per
the design):

| Fixture | PR | Files changed | Preflight `cargo check` |
| --- | --- | --- | --- |
| broken-caller | #1812 (closed) | `crates/aptu-coder/src/validation.rs` | 6 x E0308, all in `edit_overwrite.rs`, `edit_replace.rs`, `verify_anchors.rs` (outside the diff) |
| dead-code-path | #1813 (closed) | `crates/aptu-coder/src/tools/common.rs` | errors in `analyze_directory.rs`, `analyze_file.rs`, `analyze_symbol.rs` (outside the diff) |
| wrong-trait-impl | #1814 (closed) | `crates/aptu-coder-core/src/languages/rust.rs` | E0308 at `languages/mod.rs:87` handler registration (outside the diff) |
| clean-control | #1815 (closed) | `crates/aptu-coder/src/validation.rs` | clean; validation tests pass |

The pre-registered 24-session matrix was NOT executed. The benchmark halted
at its own pre-registered spend stop rule during the smoke run of the first
cell, before any matrix session started:

- Pre-registered per-run estimate: USD 0.0023 (sibling audits); cap USD 0.069
  for 24 runs plus 25% headroom.
- Actual per-run cost with the declared provider (zai / glm-5.3-flash):
  USD 0.0173-0.0221 per completed session, roughly 8x the estimate.
- Projected 24-run spend: approximately USD 0.41-0.53, well above the USD
  0.069 cap. The runner's per-run kill (USD 0.004) and cumulative cap checks
  fired as designed; continuing would have violated the pre-registered cap.

A harness defect also duplicated the smoke cell: the first runner invocation
hit the tool-level 300 s wait and a second invocation was started while the
first was still live, producing four completed sessions for one cell instead
of two. All four are reported below; none were discarded or re-run.

## Declared deviations

- **Provider/model:** the design pre-registered `mistral-small-2603`; no
  Mistral credential exists in this environment (only `ZAI_API_KEY`), so
  sessions ran with `zai/glm-5.3-flash` via the installed `pi` CLI.
- **No-graph arm mechanism:** the real stdio visibility proxy was built and
  unit-tested (tools/list filtering, clean JSON-RPC rejection of
  `resources/list`, `resources/templates/list`, graph `resources/read`, and
  `analyze_symbol` calls). The instruction-only fallback was not needed and
  was not used.
- **forced-graph preamble mechanism:** injected as an MCP logging
  notification after `initialize` plus the pinned prompt suffix, because a
  proxy cannot rewrite the agent system prompt.
- **Matrix completeness:** 1 of 24 cells ran (smoke of
  optional-graph / broken-caller), and it ran twice due to the harness
  duplication above.

## Partial results (1 cell, 4 sessions, not the pre-registered matrix)

Pinning: repository commit `bec6b10`, fixture PR #1812 (head `a117745`),
prompt pinned per arm in `config.py`, timeout 300 s, provider `zai`,
model `glm-5.3-flash`, cache state recorded per run (run 1 cold, run 2 warm;
the duplication makes the warm/cold split unreliable for the duplicated
sessions).

### Adjudication (pre-registered rubric, single reviewer)

| Fixture | Arm | Sessions | Catch | False positive | False negative | False reassurance |
| --- | --- | --- | --- | --- | --- | --- |
| broken-caller | optional-graph | 4 | 4 | 0 | 0 | 0 |

Evidence excerpt (session `62697859`, final message): "the parameter order
was flipped ... No caller is updated. Findings (all confirmed with
`cargo check` - 6 x E0308, lib does not compile) ... `edit_overwrite.rs:28`
... `verify_anchors.rs:85`". This names the defect symbol and the concrete
stale callers, satisfying the catch definition (not a generic hedge).

### Graph attribution

MCP proxy logs show zero `tools/call` exchanges in all four sessions: the
agent discovered the MCP surface (initialize, tools/list, resources/list all
logged) but performed its review entirely with bash/read tools, and the
catches were driven by the agent running `cargo check` on the PR head. No
`analyze_symbol` call or graph resource read occurred. These catches are
therefore **not attributable to the graph surface**, and the smoke cell does
not exercise the hypothesis the benchmark was designed to test.

### Per-session token and spend accounting (fail-closed from pi session JSONL)

| Session | Cell | Input tokens | Output tokens | Cost USD | Complete |
| --- | --- | --- | --- | --- | --- |
| 62697859 | optional-graph/broken-caller run 1 | 40075 | 15286 | 0.017976 | yes |
| 02a2655a | optional-graph/broken-caller run 1 (duplicate) | 54272 | 14571 | 0.022093 | yes |
| 2bdd13a1 | optional-graph/broken-caller run 2 | 30246 | 15346 | 0.017323 | yes |
| 3e4ccdb1 | optional-graph/broken-caller run 2 (duplicate) | 31121 | 18087 | 0.021192 | yes |
| Total | | | | **0.078584** | |

Actual spend: USD 0.0786, which exceeds the pre-registered USD 0.069 cap on
its own (one cell, doubled). Raw capture is preserved under
`.kg-ablation-smoke/` (untracked): transcripts, session JSONL, and per-session
MCP request/response logs.

## Verdict

**NOT-SUPPORTED.** The pre-registered decision rule ("retain the pull surface
only if Optional or Forced produces unique named-defect catches that No-graph
misses, with no unacceptable increase in false reassurance") cannot be
evaluated: no no-graph sessions ran, no forced-graph sessions ran, and the
only completed sessions produced catches via `cargo check` with zero graph
usage. Under the design's own rule, "if no graph arm catches anything
No-graph misses ... do not expand graph visibility; preserve current
pull-only compatibility pending a separately approved removal decision."

## Recommendation on the ~3,100 LOC cut

Do not cut the KG surface on this evidence. A full-cost pre-registered run is
required first: at observed per-run costs the 24-run matrix is approximately
USD 0.41-0.53 of model spend, roughly 6-8x the pre-registered cap. Because
the smoke suggests review quality on these fixtures is compile-error driven,
the re-run should also consider whether the fixture shapes admit defects
invisible to `cargo check` (type-compatible contract drift), or the benchmark
should accept that this fixture family cannot differentiate arms.

## Limitations

- **Halted execution:** 1 of 24 cells, duplicated; all aggregate claims carry
  minimal evidentiary weight.
- **Small N, single model:** the design's own limitations apply fully
  (directional only; no generalization beyond glm-5.3-flash).
- **Deviation:** provider/model differs from the pre-registered
  `mistral-small-2603`.
- **Compile-visible fixtures:** all three defective fixtures produce hard
  compile errors, which a bash-capable agent can catch without any graph;
  the fixture family as constructed cannot isolate graph value.
- **Natural pull behavior:** zero graph pulls in the optional arm is a single
  cell observation, but it is consistent with the injection-model audits that
  motivated this benchmark.
