# Audit: v18 Benchmark Readiness and Tool-Surface Sequencing -- September 2026

Date: 2026-09-22  
Base: 9b05733 (post-#1599/#1602)  
Methodology under review: [../benchmarks/v18/methodology.md](../benchmarks/v18/methodology.md) (PR #1563, merged)  
Evidence: live harness verification (pi 0.85.1, `pi -p --mode json`, glm-5.3-flash via zai: `usage.input/output/cacheRead` + computed `usage.cost` per message confirmed), prior failed executions (v17 postmortem, v12), in-house experiment corpus (dotfiles `experiments/`, brave-narrow-bench, exp3/exp4 model comparisons), blog ablation data (mcp-tool-docs.md), and the [2026-09-19 full audit](2026-09-19-full-audit.md)

## See Also

- [2026-09-19-full-audit.md](2026-09-19-full-audit.md) -- lean audit for subagent workloads (F1-F10; keep-7-tools verdict, exec_command traffic dominance)
- [../benchmarks/v18/methodology.md](../benchmarks/v18/methodology.md) -- superseded-by-nothing; the design under audit here (PR #1563)
- [../benchmarks/v17/](../benchmarks/v17/) -- discarded execution; postmortem reused as defect checklist
- [2026-08-29-kg-pull-ablation-benchmark-design.md](2026-08-29-kg-pull-ablation-benchmark-design.md) -- ablation-arm design precedent

## Question

Is the merged v18 methodology (PR #1563) ready to execute? Build the harness or use a pre-baked one? And what tool-surface changes should land before, during, or after the benchmark, given the goal of a simple, fast, agent-optimized aptu-coder?

## Answer (short)

1. **NO-GO to run the sealed ladder today.** The methodology is sound (seal-and-reveal, run ladder, pre-registered gate, hard budget, `no-target-read` scorer category all correctly kill the v17/v12 failure modes), but one harness defect blocks the MCP arm as written, and all execution artifacts are missing.
2. **Build, do not buy.** No existing harness drives pi headless with an MCP arm, per-session spend kills, and pi-JSONL scoring (lm-eval = static model evals; SWE-bench = container test execution). Hand-roll with stdlib/scipy stats. Est. 6-8 engineer-days.
3. **Sequence surface changes after the sealed run.** Config freezes at sealed-run start; trims should also be informed by v18 results. The 2026-09-19 audit's keep-7-tools verdict (F8) stands: the trim path is a client-profile arm configuration, not tool removal. The dominant validated token sink remains exec_command output shape (F1, #1578), not the tools list.

## Summary Table

| # | Finding | Verdict | Recommendation | Priority |
|---|---------|---------|----------------|----------|
| R1 | Methodology mandates `--no-extensions` in both arms, but `--mcp-config` is an extension-registered flag (pi-mcp-adapter): the MCP arm cannot wire as written | CONFIRMED | Register the adapter via explicit `--extension <path>` (honored despite `--no-extensions`), or revise the flag set to allow exactly one extension in the MCP arm -- [#1603](https://github.com/clouatre-labs/aptu-coder/issues/1603) | HIGH (blocking) |
| R2 | `~/.pi/agent/mcp.json` registers aptu-coder with `directTools: true`; a bare native arm would leak MCP tools | CONFIRMED | Dedicated `PI_CODING_AGENT_DIR` per arm with arm-specific `mcp.json` (shadow-dir pattern proven in brave-narrow-bench) -- [#1603](https://github.com/clouatre-labs/aptu-coder/issues/1603) | HIGH |
| R3 | `--no-session` suppresses the transcripts the scorer needs | CONFIRMED | Runner uses `--session-dir` -- [#1603](https://github.com/clouatre-labs/aptu-coder/issues/1603) | HIGH |
| R4 | All five execution artifacts missing (fixture generator, oracle, runner, seal/label-map, categorical scorer + stats) | CONFIRMED | Hand-rolled build; v17 prompts + postmortem reusable; brave-narrow-bench repro loop as runner skeleton -- [#1604](https://github.com/clouatre-labs/aptu-coder/issues/1604) | HIGH |
| R5 | Cost-metering premise | VERIFIED (live) | No action; record rates in the manifest on run date | none |
| R6 | Analysis-only profile (4 analyze tools only) as a client-profile arm; edit/exec tools duplicate agent built-ins in analysis workloads | CONFIRMED | Build after the sealed run; est. 35-45% tools-list + instructions reduction per pi narrowing precedent (~30% session tokens, brave-narrow-bench) -- [#1605](https://github.com/clouatre-labs/aptu-coder/issues/1605) | MEDIUM (post-benchmark) |
| R7 | New description/param trims beyond f6bfe21 | REJECTED for now | The 2026-09-19 audit found the parameter surface converged (post-#1542-#1544, f6bfe21) and structural collapses unhelpful (exp2 NO-GO); remaining wins are output-side (F1/F5, #1578/#1580). Do not re-open the description lever without new evidence | none |
| R8 | `no-target-read` as a scorer category (both arms) is a direct, correct fix for the v17 contamination failure | CONFIRMED (design strength) | No action | none |

## Evidence Detail

### Harness verification (live, 2026-09-22)

`pi -p --mode json` on glm-5.3-flash (zai) emits per-message `usage.input`, `usage.output`, `usage.cacheRead`, and a computed `usage.cost`. The runner's metering-and-kill design is implementable as specified. `--mcp-config` confirmed extension-registered via `pi --help`; explicit `--extension` paths are honored under `--no-extensions` (resolution option 1 for R1).

### Prior-failure transfer

- v17: MCP server cwd never equaled the fixture root; all MCP-arm scores measured prior knowledge (9/9 with zero target reads). v18 transfers the cwd lesson and adds `fabricated-anchor`/`no-target-read` scorer categories. Correct.
- v12: no archived transcripts, best-subset aggregation. v18 mandates session-JSONL archival via `--session-dir` and frozen config. Correct, provided R3 is honored in the runner.

### Build-vs-buy

lm-eval and SWE-bench-style runners were evaluated: neither supports pi headless mode, an MCP arm, per-session USD spend kills, or pi-JSONL parsing. The fixture, oracle, and scorer are project-specific (fixture paths, `fabricated-anchor` resolution). Hand-rolling is the correct call; the experiment repos' `scorer.py` pattern supplies the unit-tested-scorer discipline.

### Tool-surface sequencing (with the 2026-09-19 audit)

Telemetry: `exec_command` is 72% of calls, top-3 (exec + both edits) 91.9%, analyze family ~8%. Weighted by traffic, the biggest remaining lever is exec output shape (F1/#1578), already in flight -- not the tools list or descriptions. The brave-narrow-bench (10 tools to 1: ~30% session tokens) and the blog ablation (75% description cut: -12.4/-13.8% cold-call input, accuracy null) support R6 as the next structural lever, applied as a client profile so the full 7-tool surface stays available (F8 verdict preserved). R7 is explicitly rejected: f6bfe21 landed the description trim; exp2 showed enum consolidation of response shaping does not help agents; re-trimming without new evidence contradicts the 2026-09-19 convergence finding.

### Run-order decision

1. R1-R3 fixes (#1603).
2. Harness artifacts (#1604): fixture + oracle first (zero API cost, permanent reuse).
3. Run the ladder (planned ~USD 0.90, ceiling USD 5).
4. R6 profile (#1605) informed by v18 results; F1/F5 items (#1578/#1580) proceed independently on the output side.

## References

- v18 methodology: `docs/benchmarks/v18/methodology.md` (PR #1563)
- v17 postmortem: `docs/benchmarks/v17/`
- dotfiles experiments: `~/git/dotfiles/experiments/` (README tables; brave-narrow-bench repro script)
- Blog ablation: clouatre.ca, "How Much Tool Documentation Do AI Agents Actually Need?" (Zenodo DOI 10.5281/zenodo.22844431)
- Telemetry window: 2026-09-14 (58,776 calls), full-history 82,504 calls, per the 2026-09-19 full audit
