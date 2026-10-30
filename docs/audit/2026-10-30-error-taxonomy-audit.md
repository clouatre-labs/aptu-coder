# Audit: Error Taxonomy and Telemetry Coverage -- October 2026

Date: 2026-10-30
Base: cb96f5b (v0.40.3)
Evidence: October 2026 telemetry (concatenated `~/.local/share/aptu-coder/metrics-2026-10-*.jsonl`, 46,342 events) + code evidence (symbol_focused.rs:158-172, call_graph.rs:96, edit.rs:435-520, metrics.rs:470-499, analyze_symbol.rs golden-list pattern, analyze_file.rs:180-320, analyze_module.rs:44-62) + MCP 2026-07-28 JSON-RPC error norms

## See Also

- [2026-09-19-full-audit.md](2026-09-19-full-audit.md) -- house format baseline; F9 covers MCP 2026-07-28 conformance posture
- [2026-08-01-mcp-spec-2026-07-28.md](../2026-08-01-mcp-spec-2026-07-28.md) -- spec adoption audit (`-32602` routing per SEP-2164)
- [2026-07-05-usage-observability-code-size.md](2026-07-05-usage-observability-code-size.md) -- observability telemetry design

## Question

Do the server's JSON-RPC error codes and `error_subtype` telemetry fields give an accurate, actionable picture of client-input failures? October telemetry shows `analyze_symbol` at an 86% error rate while `exec_command` errors are cleanly classified -- is the analyze family misreporting internal errors as input errors, or input errors as internal ones, and can telemetry distinguish them?

## Answer (short)

No, not for the analyze and edit surfaces. Three confirmed findings:

1. **Client-input analysis failures are emitted as INTERNAL_ERROR (-32603).** `GraphError::SymbolNotFound` and related lookup failures map to the internal-error code in `symbol_focused.rs:158-172`, and the `GraphError` hint field is discarded. Per MCP 2026-07-28 norms, a bad symbol name from the caller is an input error (`-32602`), not a server fault. This also explains the headline number: `analyze_symbol` shows 547 errors vs 91 ok (86% error rate). 271 of those errors are `analysis_failed` events (49.5%); code evidence establishes that symbol-not-found is routed through the same emit site, but telemetry cannot quantify how many of the 547 are caller-input versus server faults -- the classification gap #1766 closes; the remaining 272 `handler_internal` events may include caller-input failures (e.g. symbol-not-found) as well as genuine server faults, and the split between them cannot be determined from current telemetry -- which is precisely why #1766's fix matters.
2. **`analyze_file` and `analyze_module` invalid_params errors carry no `error_subtype`.** All 146 `analyze_file` and all 62 `analyze_module` invalid_params events have `error_subtype=null`, collapsing three distinct classes each (directory path, summary/cursor conflict, size limit). `analyze_symbol` already demonstrates the fix: a golden-list subtype taxonomy exists there.
3. **`edit_replace` batch_validation_failed collapses four internal classes.** 119 batch-validation events merge empty-old_text, not_found, ambiguous, and overlapping failures into one subtype. Model-visible text already carries recovery guidance, so this is telemetry-only.

The contrast case is `exec_command`: heredoc_error 54, timeout 47, cd_path_not_found 19, working_dir_not_found 6 -- subtype coverage that makes its error traffic immediately actionable. The analyze and edit surfaces should match it.

One non-finding: `schema_surface` accounts for 3,703 events, but these are server-start emissions (per-start, `metrics.rs:470-499`), not client traffic. Not spam; no change recommended.

## Summary Table

| # | Finding | Verdict | Recommendation | Priority |
|---|---------|---------|----------------|----------|
| T1 | Symbol-not-found and other client-input analysis failures map to INTERNAL_ERROR (-32603) instead of INVALID_PARAMS (-32602) (`symbol_focused.rs:158-172`, `crates/aptu-coder-core/src/graph/call_graph.rs:96`); the `GraphError` hint field is discarded. Drives the 86% `analyze_symbol` error rate (547 errors / 91 ok; analysis_failed 271 all at param_path_depth=0, handler_internal 272 clustered at depths 5-10, 4 events with kind=null) | CONFIRMED | Reclassify client-input analysis failures as INVALID_PARAMS with a stable `error_subtype`; preserve the `GraphError` hint in the payload; keep genuine panics and join failures internal. [#1766](https://github.com/clouatre-labs/aptu-coder/issues/1766) | HIGH |
| T2 | `analyze_file` and `analyze_module` invalid_params errors carry `error_subtype=null` across three distinct classes each (directory path, summary/cursor conflict, size limit) -- 146 + 62 telemetry-blind events, vs `analyze_symbol`'s existing golden-list subtype pattern | CONFIRMED | Add stable subtypes for the three classes on each tool. [#1767](https://github.com/clouatre-labs/aptu-coder/issues/1767), with T3 | MEDIUM |
| T3 | `edit_replace` batch_validation_failed (119 events) collapses 4 internal failure classes (empty old_text, not_found, ambiguous, overlapping); model-visible text already carries recovery guidance | CONFIRMED | Split telemetry subtypes only; no model-visible text change. [#1767](https://github.com/clouatre-labs/aptu-coder/issues/1767), with T2 | MEDIUM |
| -- | `schema_surface` 3,703 events look like traffic but are per-start emissions (`metrics.rs:470-499`) | CONFIRMED (not a defect) | **No change.** Server-start metadata, not client traffic; not spam | none |
| -- | `exec_command` subtype coverage is healthy and actionable (heredoc_error 54, timeout 47, cd_path_not_found 19, working_dir_not_found 6) | CONFIRMED (contrast case) | **No change.** This is the target shape for the analyze and edit surfaces | none |

## Evidence Detail

### Call distribution (October 2026, non-received events)

`exec_command` 14,885; `schema_surface` 3,703; `edit_replace` 2,976; `edit_overwrite` 1,720; `analyze_file` 650; `analyze_symbol` 638; `analyze_directory` 252; `analyze_module` 191; `verify_anchors` 70; `read_resource` 12. The analyze family is a small share of traffic, but `analyze_symbol`'s 86% error rate dominates its 638 calls -- the taxonomy problem is concentrated where errors are most frequent relative to volume.

### T1: error-code misclassification

`symbol_focused.rs:158-172` maps `GraphError::SymbolNotFound` to INTERNAL_ERROR (-32603). A symbol name that does not resolve is caller input, not a server fault; MCP 2026-07-28 routes input errors to -32602 (see the [2026-08-01-mcp-spec-2026-07-28.md](../2026-08-01-mcp-spec-2026-07-28.md) SEP-2164 posture, and full-audit F9). The telemetry signature: 271 `analysis_failed` events all carry `param_path_depth=0` (an empty or separator-free working path), while the 272 `handler_internal` events cluster at depths 5-10 (d5=74, d6=94, d7=15, d9=18, d10=19, plus d1-3=46). The field records path shape, not processing stage, so the audit makes no causal claim from depth alone. Four events carry kind=null and need handling in the fix so they are not silently untyped. The `GraphError` hint field, which tells the caller what to correct, is dropped on the floor at the mapping site.

### T2: null subtypes on analyze_file / analyze_module

`analyze_file.rs:180-320` and `analyze_module.rs:44-62` reject null-subtype cases (directory path supplied, summary/cursor conflict, size limit) with invalid_params but no `error_subtype`. October: 146 and 62 such events, all null. `analyze_symbol` already has the golden-list subtype pattern in place; the fix is to extend it to these two tools so the three classes are distinguishable in telemetry.

### T3: edit_replace batch validation collapse

`edit.rs:435-520` batch validation produces four distinguishable failure modes internally, but telemetry records all 119 October events as `batch_validation_failed`, alongside `not_found` 62, `ambiguous` 7, and 21 null-subtype events. The four classes are mutually exclusive `error_subtypes` (each event carries exactly one), so October holds 209 total `edit_replace` error events: 119 `batch_validation_failed`, 62 `not_found`, 7 `ambiguous`, and 21 null-subtype events. The proposed batch-internal split (empty old_text, batch-level not_found, batch-level ambiguous, overlapping edits) is distinct from the existing top-level `not_found` and `ambiguous` subtypes, which cover non-batch single-edit failures. Model-visible error text already includes recovery guidance (per the 2026-09-19 audit F10 verification), so this is purely a telemetry-granularity gap.

### Healthy contrast: exec_command

`exec_command` subtypes are populated and actionable: heredoc_error 54, timeout 47, cd_path_not_found 19, working_dir_not_found 6. Every class maps to a distinct caller correction. This is the shape the analyze and edit surfaces should reach.

## Recommendations by ROI

1. **Reclassify client-input analysis failures** (T1, HIGH, [#1766](https://github.com/clouatre-labs/aptu-coder/issues/1766)). Map symbol-not-found and equivalent caller-input analysis failures to INVALID_PARAMS (-32602) with a stable `error_subtype`; preserve the `GraphError` hint in the error payload; keep panics and join failures under INTERNAL_ERROR.
2. **Add subtypes to analyze_file / analyze_module invalid_params** (T2, MEDIUM). Three classes each, mirroring `analyze_symbol`'s golden-list pattern. [#1767](https://github.com/clouatre-labs/aptu-coder/issues/1767).
3. **Split edit_replace batch_validation_failed telemetry subtypes** (T3, MEDIUM). Telemetry only; no model-visible change. [#1767](https://github.com/clouatre-labs/aptu-coder/issues/1767).

Explicit do-NOTs: no change to `schema_surface` per-start emission (server-start metadata, not traffic); no change to `exec_command` error handling (already healthy); no model-visible text changes for T3.

## Impact (measured facts only)

- T1 corrects the error code on the analysis_failed population, 49.5% of errors, which contains the symbol-not-found class per code evidence (271 of `analyze_symbol`'s 547 October errors); the 272-event handler_internal cluster may also contain caller-input failures, so #1766's fix improves classification across both populations.
- T2 makes 208 events per month (146 + 62) classifiable instead of null.
- T3 makes 119 events per month attributable to one of four batch-internal classes (empty old_text, batch-level not_found, batch-level ambiguous, overlapping edits); it changes no caller-visible behavior.
- No accuracy or token claims are made; these are error-code and telemetry changes only.

## Sources

- Telemetry: `~/.local/share/aptu-coder/metrics-2026-10-*.jsonl` (46,342 events)
- Internal: `crates/aptu-coder/src/tools/symbol_focused.rs`, `crates/aptu-coder-core/src/graph/call_graph.rs`, `crates/aptu-coder-core/src/edit.rs`, `crates/aptu-coder/src/metrics.rs`, `crates/aptu-coder/src/tools/analyze_symbol.rs`, `crates/aptu-coder/src/tools/analyze_file.rs`, `crates/aptu-coder/src/tools/analyze_module.rs`
- Spec: [MCP 2026-07-28 tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
- Prior audits: [2026-09-19-full-audit.md](2026-09-19-full-audit.md), [2026-08-01-mcp-spec-2026-07-28.md](../2026-08-01-mcp-spec-2026-07-28.md)
