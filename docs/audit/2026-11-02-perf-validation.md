# Audit: Perf-PR Validation with Criterion Baselines -- November 2026

Date: 2026-11-02
Base: 68a0419 (v0.41.1, pre-wave) vs 6e08b49 (post-wave)
Evidence: `cargo bench -p aptu-coder-core --bench analysis` run twice, once per revision, with `--save-baseline pre-wave` / `--baseline pre-wave`; 20 benchmark cases across 11 criterion groups, 10 samples each, local run

## See Also

- [2026-11-01-agent-optimization-audit.md](2026-11-01-agent-optimization-audit.md) -- source of the perf wave being validated (PRs #1783, #1786)
- [2026-08-23-performance-token-efficiency.md](2026-08-23-performance-token-efficiency.md) -- prior performance baseline

## Question

Did the merged performance PRs (#1783, #1786) measurably improve the analysis benches they targeted, without regressing the rest?

## Answer (short)

Mixed and mostly noise-dominated. Two targeted wins are clearly real: `analyze_module` (-8.1%, p < 0.05, improved) and `file_details` (-5.7%, improved). Graph-query paths also improved slightly (`render_subgraph_text` -3.0%, `blast_radius` -3.4%, `structural_graph/warm_hit` -3.9%). Several walk-heavy benches (`subtree_count_*`, `analyze_directory_depth/depth_1`, `call_graph_cache/cold_miss`) show small statistically-significant regressions of +2.5% to +7%, but the drift-free `subtree_count_*` control groups regressed too, which points to machine/environment noise and src drift rather than the PRs. Overall: the targeted wins landed; no credible regression attributable to the PRs.

## Summary Table

| Benchmark | Pre-wave mean | Current mean | Change | Verdict |
|---|---|---|---|---|
| overview/analyze_directory_src | 11.448 ms | 11.493 ms | -0.12% | No change (p 0.90) |
| file_details/analyze_file_lib_rs | 795.60 us | 747.10 us | -5.75% | Improved |
| symbol_focus/analyze_focused_src | 28.682 ms | 28.688 ms | +0.25% | No change (p 0.65) |
| subtree_count_overhead/baseline_walk_only | 3.8083 ms | 3.8265 ms | -0.00% | No change (p 1.00) |
| subtree_count_overhead/with_single_walk_and_count | 3.8091 ms | 3.9445 ms | +2.02% | No change (p 0.23) |
| subtree_count_overhead_500/baseline_walk_only | 5.2936 ms | 5.5332 ms | +3.72% | Regressed |
| subtree_count_overhead_500/with_single_walk_and_count | 5.4388 ms | 5.6940 ms | +7.10% | Regressed |
| subtree_count_overhead_1000/baseline_walk_only_1000 | 6.1556 ms | 6.3445 ms | +3.82% | Regressed |
| subtree_count_overhead_1000/with_single_walk_and_count_1000 | 6.6885 ms | 6.4817 ms | -3.51% | Improved |
| analyze_module/analyze_module_file_lib_rs | 456.27 us | 420.36 us | -8.10% | Improved |
| analyze_directory_depth/depth_1_repo_root | 1.4536 ms | 1.5197 ms | +4.78% | Regressed |
| analyze_directory_depth/depth_2_repo_root | 20.551 ms | 20.617 ms | +1.00% | No change (p 0.10) |
| call_graph_cache/cold_miss | 26.489 ms | 27.300 ms | +3.62% | Regressed |
| call_graph_cache/warm_hit | 808.32 ns | 806.61 ns | -0.34% | No change (p 0.31) |
| structural_graph/cold_miss | 683.04 us | 692.77 us | +0.92% | No change (noise threshold) |
| structural_graph/warm_hit | 31.217 ns | 30.188 ns | -3.91% | Improved |
| structural_graph/cache_miss_old_independent_rebuild | 669.95 us | 692.14 us | +3.30% | Regressed |
| structural_graph/cache_miss_collapsed_from_call_graph | 587.90 us | 599.01 us | +1.49% | No change (noise threshold) |
| structural_graph_query/render_subgraph_text | 10.125 us | 9.7483 us | -3.05% | Improved |
| structural_graph_query/blast_radius_bidirectional | 35.795 us | 34.420 us | -3.35% | Improved |

The plan anticipated 12 benchmark functions; the bench file defines 11 group functions covering 20 cases, all reported above. The `cache_paths` group listed in the plan does not exist in the bench file at either revision and was not run.

## Evidence Detail

Criterion `change` lines are the primary data (middle estimate of the three-point bound), with criterion's significance verdict per benchmark. Raw logs: `bench-pre-wave.log` and `bench-current.log` in the handoff directory for this session.

Key wins (statistically significant, direction consistent with PR intent):

- `analyze_module` -8.10% (456 us-class per-file parse path, PR #1783 target).
- `file_details` -5.75%.
- Graph query paths -3.0% to -3.9% (PR #1786 render/blast-radius paths).

Suspected noise, not regressions:

- `subtree_count_*` groups are drift-free controls by design (they exercise the walk, not the changed src). Their baseline-only cases regressed +3.7% to +3.8% with p < 0.05, which cannot be caused by the PRs and therefore calibrates the ambient noise floor of this comparison at roughly +/-5% for multi-millisecond walk benchmarks. Under that calibration, the `analyze_directory_depth/depth_1` (+4.78%) and `call_graph_cache/cold_miss` (+3.62%) regressions are not distinguishable from noise.
- `subtree_count_overhead_1000/with_single_walk_and_count_1000` "improved" -3.51% while its own baseline case regressed, confirming these walk benches are not reliable at this resolution.

## Methodology and Caveats

- Local ARM64 machine (macOS), benches run back to back; machine was otherwise quiet but no isolated CI runner, so single-run variance is uncontrolled.
- Shared `CARGO_TARGET_DIR` so the pre-wave baseline saved by criterion at 68a0419 is visible to the comparison run at 6e08b49; criterion baselines pin the reference estimates per benchmark case.
- Src-drift confound: the two revisions differ by roughly 720 lines of src changes plus ~56 lines of bench/harness changes, so per-benchmark comparisons measure the whole wave, not individual PRs.
- Coverage gaps: no `analyze_symbol` tool benchmark exists; the V5 lazy-init change is not isolated by any bench. `cache_paths` group named in the validation plan does not exist in the bench file.
- No `[profile.bench]` tuning exists; criterion defaults (10 samples, 3s warmup) apply, which limits resolution on the multi-millisecond walk benches as demonstrated by the drift-free controls.
