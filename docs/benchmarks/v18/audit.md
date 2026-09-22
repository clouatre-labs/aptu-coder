# v18 Readiness & Tool-Surface Audit (2026-09, pre-benchmark)

Status: audit complete. Verdict: **NO-GO to run the sealed ladder today**; one
blocking harness defect plus required artifacts must land first. This audit
reviews the merged methodology ([methodology.md](methodology.md), PR #1563)
against the live environment, prior failed executions (v17, v12), and the
in-house experiment corpus (dotfiles experiments, MCP tool-docs ablation).

## 1. Verdict summary

| Question | Answer |
| --- | --- |
| Ready to run benchmarks? | Not yet. One blocking flag conflict + artifacts missing. |
| Build or pre-baked harness? | **Hand-roll.** No existing harness drives pi headless with an MCP arm, per-session spend kills, and pi-JSONL scoring (lm-eval = static model evals; SWE-bench = container test execution). Stats via stdlib/scipy. |
| Methodology sound? | Yes. Seal-and-reveal, run ladder, pre-registered gate, hard budget, and the no-target-read scorer category correctly address the v17/v12 failure modes (prior-knowledge contamination, unverifiable MCP arm). |
| Cost premise valid? | Yes, verified live: `pi -p --mode json` (glm-5.3-flash, zai, pi 0.85.1) emits `usage.input/output/cacheRead` and computed `usage.cost` per message. |

## 2. Blocking defect (must fix before harness build)

**MCP-arm flag conflict.** The methodology mandates `--no-extensions` in both
arms, but `--mcp-config` is an extension-registered flag (pi-mcp-adapter). As
written, the MCP arm cannot wire aptu-coder. Resolution options:

1. Register the MCP adapter via an explicit `--extension <path>` (honored even
   with `--no-extensions`), or
2. Revise the methodology flag set to allow exactly one extension in the MCP
   arm, with the native arm unchanged.

Track in the harness issue.

## 3. Environment risks (verified)

- **Config leakage:** `~/.pi/agent/mcp.json` registers aptu-coder with
  `directTools: true`. The native arm requires a dedicated
  `PI_CODING_AGENT_DIR` session dir with its own (empty or native-only)
  `mcp.json`. The dotfiles `pi-brave-narrow-bench` shadow-agent-dir pattern is
  the proven isolation mechanism.
- **Session archival:** `--no-session` suppresses transcripts; the runner must
  use `--session-dir` so the scorer can read session JSONLs.
- **cwd lesson (v17):** launcher cwd = fixture root; MCP config carries no cwd
  override. Retain the pre-flight `analyze_directory` wiring gate.

## 4. Missing artifacts (all must exist before the ladder)

| Artifact | Effort | Reuse |
| --- | --- | --- |
| Fixture generator (50k+ LOC, multi-language, synthetic, frozen) | 1-2 d | param-description-experiments `_source` provenance pattern |
| Oracle (independent of the treatment; answers committed with fixture) | 1-2 d | ripgrep/tree-sitter derivation |
| Runner (stage ladder, budget caps, USD 0.25/session kill, `--session-dir`) | 1-2 d | brave-narrow-bench repro loop as skeleton |
| Seal/label-map blinding | 0.5 d | in-house experiment repos |
| Categorical scorer + stats (correct/incorrect/omitted/fabricated-anchor/no-target-read; sign test, Wilcoxon, bootstrap) | 1-2 d | scorer.py pattern from param-description-experiments |

Total ~6-8 engineer-days. v17 prompt templates and the v17 postmortem (as a
defect checklist) are directly reusable; v12's rubric approach is rejected by
design.

## 5. Tool-surface audit (independent of the benchmark, same goal)

Evidence base: the published MCP tool-docs ablation (75% description cut on
`analyze_symbol` → −12-14% cold-call input tokens, accuracy null at two tiers;
4-params→1-enum consolidation → additional −3.3-4.3%) and the pi tool-narrowing
benchmark (10 tools → 1 → ~30% session tokens saved). Description sizes today:
`edit_replace` ~810 chars (lib.rs), `exec_command` ~700, `analyze_module` ~590,
`analyze_file` ~490, `analyze_symbol` ~430 (already trimmed). Param docs in
`aptu-coder-core/src/types.rs` run 150-250 chars each.

Ranked opportunities:

1. **Analysis-only profile** (feature flag or client-profile): exposes only the
   4 analysis tools; est. 35-45% tools-list + instructions reduction. The
   edit/exec tools duplicate pi built-ins (write/edit/bash); their genuine
   differentiators (path validation, output filters, stdin/heredoc guards,
   typed ShellOutput) matter in full builds, not in the v18 analysis arm.
2. **Description diet** on `edit_replace`, `exec_command`, `analyze_module` +
   types.rs param docs: 5-8% per call; the published trim is the template.
3. **Param consolidation:** `match_mode` + `impl_only` → a single `mode` enum;
   `summary` → tri-state enum. `SymbolAnalysisMode` already proves the pattern.

Do not touch: opaque cursor protocol and server-owned page sizes,
edit_replace exact-once + batch atomicity, `validate_path` (security),
`exit_code` contract, SEP-2164 error-code negotiation.

## 6. Sequencing decision

Trim **after** the v18 sealed run, not before: the methodology freezes config
once the sealed run begins, and trimming mid-ladder would invalidate pairing.
Run order:

1. Fix the flag conflict + isolation plan (issue: harness).
2. Build fixture + oracle + runner + scorer.
3. Run the ladder (USD 0.90 planned, USD 5 ceiling).
4. Land the surface trims informed by v18 results + the ablation data, and
   re-benchmark if desired (fixture/oracle are zero-API-cost reuse).
