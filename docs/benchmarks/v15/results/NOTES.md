# v15 Benchmark Run Notes

## What is committed

128 files: 64 `-report.json` (agent self-report) and 64 `-harness.json` (timing and token metadata), one pair per run. Session transcripts and harness logs are not committed; they contained machine-identifying paths and are reproducible by re-running the harness.

## Known data gaps

### `raw_bytes` is 0 for all runs

The harness field was never populated (hardcoded to `0` in `bench-v15-run.sh`). It was intended to record network bytes transferred. No byte-transfer data is available in this dataset.

### `content_chars` is 0 for most Condition F runs

The harness derives `content_chars` from the agent's JSON report. Condition F agents populated this field only for T1 tasks (where file content was fully fetched and reported). For T1b--T4 directory-listing tasks, Condition F agents did not include `content_chars` in their output, so it defaults to `0`. Token-cost comparisons between conditions are therefore only valid for T1 and T1b.

### `latency_ms` is 0 for the 4 error sub-task harness files

The four error sub-tasks (`E-error-missing-token`, `E-error-not-found`, `F-error-missing-token`, `F-error-not-found`) failed harness JSON extraction (the agent-produced JSON omitted `target_id`, which the extraction routine required). Timing was recorded by the bash wrapper but was not propagated to the harness file when extraction failed. Latency for error sub-tasks is unavailable.

## E-T2 systematic `content_correct=false` (6/6 runs)

All six Condition E T2 runs report `content_correct=false`, `entry_count=58`, and `first_entry_seen` as a file extension (`.ac` or `.0`). This is a methodological artifact, not a tool failure.

**Cause.** The `remote_tree` tool operates in summary mode when called on a large repository root: it returns extension-count aggregates (e.g., 58 distinct extensions, first being `.ac`) rather than named directory entries. The T2 scoring criterion requires named entries (`README.md`, `meson.build`, `gtk/`, etc.), which are not present in extension-count output. The agent correctly identified the output as not matching the criterion and scored `false`.

**Contrast with T3.** Condition E T3 runs report `content_correct=true` with the same extension-count format (`entry_count=587`, `first_entry_seen=".0"`). The T3 scoring criterion asks for `.c` file names (`gtkwidget.c`, etc.); the agent inferred their presence from the `.c` extension count (272 files) and scored `true`. This inference is reasonable but represents a different self-scoring strategy than T2.

**Implication.** The E-condition `content_correct` scores for T2 and T3 are not directly comparable: T2 fails because named entries are absent from the summary; T3 passes because the agent inferred presence from extension counts. This asymmetry reflects an interaction between the `remote_tree` summary format and the per-target scoring criteria, not a difference in underlying tool capability. Analyses comparing T2 accuracy across conditions should note this limitation.

## Condition E T4 latency outlier

One E-T4 run (`E-T4-scored-4`) recorded `latency_ms=23553`, roughly double the mean for that group (mean 15,353 ms). No discard threshold was triggered (the harness threshold is 30,000 ms). The outlier is retained; analyses should report variance alongside means for T4.

## Harness compatibility notes (execution environment)

Three incompatibilities with goose 1.34.1 were resolved without modifying the harness script:

- `goose run --profile` was removed in v1.34.1. Resolved via a PATH-priority wrapper that translated `--profile <yaml>` to `--provider`, `--model`, and `--with-extension` arguments.
- `$TIMING_METHOD` was referenced in the harness heredoc but never assigned, triggering a `nounset` error under `set -euo pipefail`. Resolved by exporting `TIMING_METHOD=epochrealtime` before each invocation.
- Error sub-task agent output omitted `target_id`, which the harness extraction routine required. The four error report files have `target_id` injected as `"error"` and are marked with `"note": "harness_extraction_fixed_manually"` in their harness files.
