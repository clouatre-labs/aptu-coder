# v19 benchmark results — sealed stage (#1681)

Stage: tasks-fanin-hop2 sealed run (2026-09-29) + A8 repair re-run. Pool: 8 Track A fanin/hop-2 tasks × 2 arms (native, mcp) = 16 sealed sessions; 4 repair sessions. Provider zai / glm-5.3-flash; binary aptu-coder 0.37.0; snapshot django `dd6f6b1` (tarball SHA256-verified against the pinned manifest). Scorer: post-A6, deterministic (snapshot-verified anchors); Jev adjudication dropped per A2.

Spends: valid sealed run $0.2283 + invalidated first attempt $0.1482 + A8 repair $0.0881 + A9b verify-repair $0.0672 = **$0.5318** (session budget cap $2.00).

## Framing: cost at iso-quality, not raw means

Per A1 the primary comparison is not "which arm scores higher" but "what does each arm cost to reach a given quality floor". With n=8 the means are descriptive only:

| arm | mean F1 (as-sealed) | mean F1 (repair-adj.) | completed (as-sealed → repair-adj.) | tokens (sealed, in/out/cache-read) | aptu tool calls | cost (sealed) |
|---|---:|---:|---|---:|---:|---:|
| mcp | 0.4321 | 0.4482 | 5/8 → 6/8 | 220k / 71k / 891k | 43 | $0.0952 |
| native | 0.3057 | 0.4051 | 7/8 → 8/8 | 200k / 108k / 1.63M | 0 | $0.1331 |

Costs to reach "correct" verdicts (F1 ≥ 0.75, snapshot-verified): the mcp arm produced 2 correct sessions (skipIfDBFeature 0.965 at $0.0091; register_lookup 0.811 at $0.0121). The native arm produced 0 correct sessions at cap; its best repaired cell (include, 0.795) carries a fabricated-anchor verdict and costs $0.0114 in the repair run alone plus the $0.0007 invalidated original. Directionally, high-quality outcomes in this pool were mcp-arm-only, but n=2 correct cells is far below any evidentiary threshold.

## Completion rates are a first-class outcome

As-sealed: mcp 5/8, native 7/8 — but 4 of the 5 non-completions were provider `error` sessions (harness defects), not model behaviour.

After A8 repair: mcp 6/8, native 8/8. The two residual mcp non-completions (chain: A7 turn cap 40; timezone: 900s wait deadline, hit in both the sealed run and the repair re-run) are genuine arm behaviour: timezone/mcp is structurally slow under the mcp arm.

The 900s wait deadline (A7b) and the per-session turn cap (A7) both bind on real sessions, so cap placement materially shapes completion rates. Cap sensitivity is unmeasured here.

## Per-task detail

See `tasks-fanin-hop2/sealed/summary.json` (as-sealed), `tasks-fanin-hop2/sealed/repairs/` (before/after, arm means, repair sessions), and `crossover.csv`.

## Methodology findings: two harness defects found and fixed

1. **Wait-deadline kill invisibility (A7b).** The first sealed attempt ($0.1482) was invalidated: the 300s wait deadline silently truncated sessions with `stopReason: toolUse` and no defect record. Fixed by recording `wait-timeout-killed:<s>` fail-closed and re-running at `SESSION_WAIT_TIMEOUT_S=900`. Cost of the defect: one full run's spend, metered but discarded.
2. **Provider-error sessions conflated with model outcomes.** 4 sessions ended on provider-side `error` and scored near 0 through no fault of the arm. Fixed by the A8 repair re-run with an explicit replacement policy (replace only where the repair completed). Without it, the mcp arm's completion rate and F1 were materially under-counted (repair-adjusted native F1 moved 0.306 → 0.405).

## Limitations (explicit)

- **n=8 per arm.** Mean-F1 confidence intervals overlap broadly at this sample size; no pairwise per-task test is meaningful. The F3 activation gate passed only vacuously (7 scorable sessions < min sample 8, per A4b).
- **Single provider, single model, single snapshot, single tier.** All conclusions are scoped to zai/glm-5.3-flash on django `dd6f6b1` fanin/hop-2 Track A.
- **Cap sensitivity unmeasured.** Turn cap 40 and 900s deadline are ratified values, not tuned ones; two sessions sit at exactly one cap.
- **Anchor fabrication is the dominant failure mode** in both arms (5 fabricated-anchor verdicts across 20 scored sessions) — this is a scorer-verifier finding about cited `path:line` anchors, independent of arm.

## A9b verify-repair pass

After the A9a word-boundary anchor verifier landed (#1709), the four completed sealed cells carrying `fabricated-anchor` verdicts (chain/native, include/mcp, qualname/native, qualname/mcp) were re-run once each with a corrective prefix restating the anchor rule (`tasks-fanin-hop2/a9b-verify-repair/`). Replacement policy per A9: a cell is replaced only where the repair completed and re-scored with zero fabricated anchors. Exactly one cell qualified — qualname/native (F1 0.4681 fabricated → 0.4000 partial); include/mcp and qualname/mcp still cite fabricated anchors under the stricter verifier and chain/native hit the 900s wait deadline, so all three stay as-sealed. Repair-adjusted means: native 0.2972 (7/8), mcp 0.4321 (5/8). Repair spend **$0.0672**; binary aptu-coder 0.38.0; snapshot tarball SHA256 re-verified against the pinned manifest before spend. See `tasks-fanin-hop2/a9b-verify-repair/README.md` for the recorded caveats (including the 5-vs-4 fabricated-cell counting discrepancy between this line and the sealed summary).

## Verdict

**Directional evidence only, not a causal claim.** On this sealed pool, the mcp arm produced the only high-quality (snapshot-verified correct) sessions and did so cheaply, while the native arm reached higher mean F1 only after repairing a harness-defect session. Completion is now 8/8 vs 6/8 in native's favour. The sample is too small, and cap- and provider-sensitivity too unmeasured, to claim MCP beats native. The durable outputs of this stage are the process repairs (A6 scorer verification, A7b kill visibility, A8 replacement policy) and a sealed, reproducible record for future stages to extend.

## Sealed-stage GO/NO-GO (A10/A11, 2026-09-30)

**GO** for a sealed N-run on fanin/hop-2 Track A, n=23 pairs per arm,
under the A11 pre-registered decision rule. Basis: the tier is the
needle-moving condition (grep-hostile, activation gate passed, both
arms complete), the existing 8-pair evidence is directionally
pro-mcp but underpowered (paired diff mean 0.052, SD 0.338, CI
[−0.231, 0.334]), and the run is affordable (~$0.82 stage cap).

**Track C: closed as a control result** per A10 — hop-1 lookups sit
below the tool's applicability floor (0/2 activation in the pilot);
no sealed Track C cells; no redesign spend. The #1681 criterion
"Track A vs Track C reported separately, no pooling" is met by
Track C's recorded null.

**Claim gate.** The N-run supports the iso-quality claim only if the
repair-adjusted 95% CI on the paired F1 diff excludes 0 in mcp's
favour with mean diff ≥ 0.1; otherwise the paper re-scopes to
activation + cost structure + methodology (pilot option iii). The
existing sealed 8-pair run remains in the record as the pilot for
this N-run; no retroactive re-scoring.
