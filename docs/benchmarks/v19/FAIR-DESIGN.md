# v19 design fix — how to measure aptu-coder's tool value fairly (not tool-selection failure)

Status: proposed protocol amendment for #1681. Grounded in recorded v18/v19 results and
published ablation methodology (citations inline).

## Diagnosis (facts from our own records)

The v18/v19 ladder, as wired, cannot produce a positive result for the MCP arm even if
the tools are excellent. Three structural defects, each recorded in our own artifacts:

1. **The task family is filtered to be ripgrep-optimal.** Task generation selected
   symbols as "unambiguous function definitions with 3–20 direct caller files where the
   double-extraction [rg vs tree-sitter] arms agree" (`docs/benchmarks/v19/results/tasks-django/README.md`).
   Every Track A prompt ("list caller files within N hops, report paths only") is solved
   by `rg` + one grep level. Our own gateway smoke states it: "the fixture task family is
   ripgrep-optimal and adversarial to structured analysis; with native bash available,
   gateway activation is tool-choice-dominated" (`docs/benchmarks/v18/results/gateway-smoke-0361/README.md`).
2. **Arms are asymmetric in a way that guarantees the confound.** The native arm is
   restricted to `read,bash`; the mcp arm keeps *all* default native tools (`read, bash,
   grep, find`) **plus** the four `aptu-coder_analyze_*` tools
   (`scripts/bench_v18/runner.py:78-99`). Nothing penalizes grep in the mcp arm; the
   only measured difference is a ~4x first-turn token overhead for unused schemas
   (2,246 vs 8,975 tokens, `wiring-smoke-0361/README.md`).
3. **Tool-silent prompts meet the documented "unknown-tools challenge".** No prompt ever
   mentions the analysis tools. Published MCP evaluations (MCP-Universe; MCP-Atlas,
   arXiv:2602.00933) document that models do not spontaneously use unfamiliar tools —
   so what v18 measured was *tool-selection failure*, not *tool value*. Recorded outcome:
   0/4 sessions activated MCP tools against an ≥80% activation target
   (`gateway-smoke-0361`, finding 3).

Conclusion: v18's null result is a property of the fixture design, not evidence about
aptu-coder. (Corroborating external evidence that such designs measure nothing about
toolkits: Verdent's SWE-bench report found scores "not particularly sensitive to agent
toolkit design".)

## Fix (five changes; each maps to a recorded defect)

**F1 — Change the dependent variable: cost-at-iso-quality under explicit budget tiers.**
On tasks the model can solve either way, accuracy is saturated and measures nothing
(BudgetBench, arXiv:2609.13149, formalizes budget-tiered sweeps as "controlled ablation
over strategy choice at fixed model"). Report, per arm: quality at each budget tier
(turn cap / active-context cap), and **$ and tokens per solved task**. Tool value = same
quality at lower cost, or higher quality under tight budgets. This replaces
"whoever scores higher F1 wins" — a metric grep-optimal tasks make meaningless.

**F2 — Add a task tier where grep is context-expensive.** Keep a small rg-optimal tier as
a sanity control, and add: (a) hop-3 transitive-caller tasks with large fan-in
(100+ caller files, where raw rg output floods context); (b) SWE-Explore-style
(arXiv:2606.07297) line-level citation scoring, which rewards precise structural
navigation over recall-by-grep. Task generation must stop filtering for
rg-vs-tree-sitter agreement as an inclusion criterion — that filter selects exactly the
tasks where structured tools cannot help.

**F3 — Activation gate, fail-closed, before any F1 interpretation.** Record tool-call
counts per arm (session JSONLs already contain them). If the tool arm's activation is
<80% of scorable sessions, the arm measured tool-choice, not tool value → stage halts
and reports, exactly like a metering defect. (This target already existed informally in
gateway-smoke-0361; make it enforced.)

**F4 — Prompt parity with one ratified sentence, not silence and not steering.** The
confound literature is split between tool-silent prompts (measures discovery) and
tool-nudging prompts (prompt bias). The middle position used by within-harness ablation
studies ("Code Isn't Memory", arXiv:2606.22417): identical task prompt in both arms, plus
one arm-neutral sentence in the tool arm only, stating that repository-analysis tools
beyond the native set are available. The sentence is a manifest-recorded, ratified
amendment — its text is frozen for the whole ladder so the delta it introduces is
constant and inspectable.

**F5 — Keep within-harness ablation, fix the asymmetry.** Same harness, same model, same
prompt, same caps (multi-model robustness checks deferred to the sealed stage —
capability masking is a documented threat, arXiv:2509.16941). The ablation axis is the
toolset only: native (`read,bash,grep,find`) vs native + aptu-coder analyze tools. Both
arms get identical budget enforcement; the mcp arm's schema overhead is *part of what
cost-at-iso-quality measures*, which is the honest accounting — the tool package must pay
for its own context rent.

## What this changes operationally

- Task generation: add fan-in/hop-3 tier; drop the rg-agreement inclusion filter for that
  tier (keep the control tier).
- Runner: turn/context caps become per-tier parameters; JSONL tool-call extraction added
  to the summary; activation gate wired like a cap (fail-closed halt).
- Scoring: add cost-per-solve and tokens-per-solve columns; F1 remains the quality
  metric; Track C unchanged (categorical).
- Pilot gate order: calibration gate → **activation gate** → discriminative filter
  (F1 gap ≥0.2 stays) → crossover curve over budget tiers.
- v18's frozen defaults are untouched; all of the above ships as manifest-recorded v19
  amendments, re-calibrated where the judge layer touches them.

## References

- MCPMark: https://arxiv.org/abs/2509.24002 (programmatic verification, turns/tool-calls reported)
- MCP-Atlas / unknown-tools challenge: https://arxiv.org/html/2602.00933v1
- BudgetBench: https://arxiv.org/html/2609.13149 (budget-tiered strategy ablation)
- Code Isn't Memory (code-graph index vs grep, within-harness ablation): https://arxiv.org/html/2606.22417v1
- SWE-Explore (exploration scored separately): https://arxiv.org/html/2606.07297v1
- Saving SWE-Bench (task saturation/leakage): https://arxiv.org/abs/2510.08996
- Verdent SWE-bench Verified report (toolkit-insensitivity of saturated suites): https://www.verdent.ai/blog/swe-bench-verified-technical-report
- SWE-Bench Pro (capability masking across models): https://arxiv.org/abs/2509.16941
