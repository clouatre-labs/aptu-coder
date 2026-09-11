# Parameter-Description Experiments Audit

Date: 2026-09-11
Related: `~/git/clouatre-labs/param-description-experiments` (external repo, not a submodule/dependency of aptu-coder)
Scope: no code changes made; findings only, pending approval.

## Context

`param-description-experiments` ran a controlled ablation asking whether moving per-parameter constraint detail out of an MCP tool's `description` string and into `inputSchema.properties[*].description` regresses parameter-filling accuracy. It targeted `aptu-coder`'s own `analyze_symbol` tool, comparing production text (post-PR `aptu-coder#593`) against a reconstructed pre-#593 baseline. This audit cross-references that repo's findings against (a) aptu-coder's current parameter-description implementation and existing style guidance, and (b) real-world error evidence from aptu-coder's metrics telemetry and session logs (goose, Claude Code).

Three read-only research passes fed this report: an inventory of the experiments repo, a baseline inventory of aptu-coder's current tool/parameter description patterns, and a log-mining pass over `~/.local/share/aptu-coder/metrics-*.jsonl`, `~/.claude/projects/`, and `~/.local/share/goose/sessions/`.

## F1: The experiment's result is narrow and already null

The experiment (single completed run, `exp1-analyze-symbol`, n=40 calls/cell/model, `claude-haiku-4-5-20251001` and `claude-sonnet-5`, Mann-Whitney U) found **no statistically significant parameter-filling regression** from the lean, current-production description style versus a richer, reconstructed pre-#593 style (haiku p=0.372, sonnet p=0.569), at two scoring resolutions. It found a **significant, consistent token-cost reduction** (haiku 12.4%, sonnet 13.8%, p<0.000001, r=1.0) for the lean style. The repo's own conclusion is scoped to this one tool, these two models, and this one historical PR — it does not generalize a recommendation beyond that.

This means the experiment does not surface a new finding actionable today: it retroactively validates a change (`#593`) aptu-coder already shipped, on the one tool it tested. It does not by itself justify extending the same trim to other tools without independent evidence for those tools.

## F2: aptu-coder already codifies the exact principle the experiment tested

`docs/MCP-BEST-PRACTICES.md` §4.1.1 ("Description Scope: Selection vs. Usage") already states the tool-description-vs-parameter-description split the experiment probes: tool description is a selection signal, parameter description is usage detail, and duplicating parameter detail in the tool description "burns tokens on every tool-listing request." §8 lists "Duplicating parameter detail in the tool description" as an anti-pattern. `docs/DESIGN-GUIDE.md` §3 separately prescribes "prescriptive over suggestive" descriptions for small-model routing. Both are enforced structurally by `crates/aptu-coder/tests/annotations.rs` (non-empty descriptions, all parameters described, complex parameters require examples) but not by any content-quality or length metric.

Git history confirms this is an active, ongoing practice rather than a one-time decision: repeated description-tightening commits exist through 2026-06 to present (`#1507`, `#1506`, `#1239`, `#753`, `#732`, `#729`, `#694`), each independent of the experiments repo. The experiment's headline finding is therefore consistent with, not novel relative to, aptu-coder's existing documented and practiced convention.

## F3: Real production errors do not match the failure mode the experiment tested

Cross-referencing `~/.local/share/aptu-coder/metrics-*.jsonl` (14,075 tool calls, 23 days) and 84 goose sessions with `working_dir` under aptu-coder (4,511 messages — the dominant real-usage channel; only 12 aptu-coder tool calls exist across all `~/.claude/projects/` transcripts on this machine) found **zero instances** of the failure mode the experiment was designed to detect: no unknown-field errors, no enum mismatches, no wrong-parameter-name substitutions (e.g. `depth` for `max_depth`) for any tool. Parameter-filling accuracy in the sense the experiment measured is not where aptu-coder's real errors occur.

Real errors cluster in three other patterns, none of which is "tool description too verbose vs. too lean":

- **Malformed/empty `cursor` values** (32 goose-session errors: `analyze_file`, `analyze_directory`, `analyze_symbol` — JSON-parse-EOF and base64-decode failures). Agents pass an empty or malformed cursor string rather than omitting the parameter.
- **Undocumented cross-field constraint** (15 errors: `"summary=true is incompatible with a pagination cursor"` on `analyze_file`/`analyze_directory`). Neither field's own description states this mutual exclusion; it surfaces only at call time.
- **`edit_replace` stale content hash** (187/1,454 calls, 12.9% — the single largest categorized error class in the metrics, corroborated by matching free-text messages in 3 goose sessions). This is a workflow/staleness issue (file changed since last read), not a description-wording issue — the `expected_content_hash` parameter behaves exactly as documented.

`exec_command`'s 126 `invalid_params` errors (1.2% error rate over 11,275 calls) have no subtype field in the metrics schema and no matching free-text sample was found in the 84 goose sessions beyond ordinary non-zero shell exit codes, so their root cause is not attributable from available evidence.

## F4: Metrics schema cannot itself diagnose parameter-description problems

`~/.local/share/aptu-coder/metrics-*.jsonl` records only `error_type` (`invalid_params`/`parse`/`unknown`) and a tool-specific `error_subtype` (currently populated only for `edit_replace`). It has no free-text error message field and no `is_paginated`-adjacent detail for cursor content. The cursor and summary/cursor findings in F3 were visible only via goose's session logs (which retain full free-text tool-response messages), not via the metrics telemetry aptu-coder emits about itself. The metrics schema, as currently defined, cannot answer future questions like this one without cross-referencing session transcripts.

## Recommendations

No code changes are proposed here; these are candidates for separate, explicitly-approved follow-up work.

### R1: No action from the experiment itself

The experiment's finding is already implemented (`#593`) and already reflected in aptu-coder's documented practice (F2). It does not identify a new tool or pattern to apply the same trim to; extending the trim to other tools (`analyze_module`, `analyze_file`, `edit_replace`, `exec_command` — currently "long" category per the baseline inventory) would need its own evidence, not an extrapolation from the `analyze_symbol`-only result.

### R2: Document the `summary` + `cursor` mutual exclusion in both fields' schema descriptions

F3's second bucket (15 real errors) is a genuine parameter-description content gap: the constraint exists in code/error message but not in either `summary` or `cursor`'s own `schemars` description on `analyze_file`/`analyze_directory`. This is the one finding in this audit that is a parameter-description fix in the same sense the experiment targeted.

### R3: Consider description or validation guidance for empty/malformed `cursor`

F3's largest bucket (32 errors) suggests agents don't know an empty-string or malformed cursor is invalid versus omitting the field entirely. Whether this is best addressed via description wording (state that cursor must be a valid opaque token or omitted) or input validation (reject/normalize empty string before the parse step) is an implementation choice for separate discussion.

### R4: Not a parameter-description issue — do not action under this audit

`edit_replace`'s stale-hash errors (F3, largest error class overall) and `exec_command`'s unattributed `invalid_params` (F3/F4) are workflow-staleness and telemetry-gap issues respectively, not parameter-description defects. Flagging for awareness only.

## Conclusion

The experiments repo empirically confirms a principle aptu-coder already documents and practices (F2), scoped to one already-shipped change on one tool (F1). It surfaces no evidence of the failure mode it was designed to detect anywhere in aptu-coder's real usage (F3). Real, evidenced parameter-related friction exists in a different place: an undocumented cross-parameter constraint (R2) and unclear cursor-omission semantics (R3), both narrower and more concrete than "verbose vs. lean" descriptions. No infrastructure, tooling, or non-MCP pattern from the experiments repo (Python/uv harness, Anthropic Batch API scoring pipeline, goose recipe) is applicable to aptu-coder, which remains strictly an MCP server.
