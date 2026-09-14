# Audit: MCP Tool Cognitive Load and Parameter Surface -- September 2026

Date: 2026-09-14  
Version: v0.33.0  
Spec baseline: MCP 2026-07-28 (rmcp 3.3.0)  
Evidence: web spec research + codebase inventory + 18-day telemetry window (2026-08-26..09-14, 58,776 tool calls; guard re-check over full history: 82,504 calls) + Claude Code client logs (~/.claude/projects)  


## See Also

- [2026-09-13-code-quality.md](2026-09-13-code-quality.md) -- prior repo-wide quality audit
- [2026-09-11-param-description-experiments-audit.md](2026-09-11-param-description-experiments-audit.md) -- description-verbosity experiment (null result)
- [2026-08-01-mcp-spec-2026-07-28.md](2026-08-01-mcp-spec-2026-07-28.md) -- spec migration audit

## Question

Should we reduce the cognitive load of using aptu-coder -- fewer parameters and options (e.g., why a timeout on each request)? What gives the best ROI on performance, accuracy, and efficiency?

## Methodology

Three parallel research passes (web/spec, codebase, telemetry), each writing a JSON handoff, followed by an adversarial verification pass instructed to refute every claim (handoffs: `.git/coder-handoffs/audit-cogload-20260914/01a-01d`), followed by a second adversarial validation pass on each proposed recommendation (handoffs `02a`, `02b`). The first verification pass checked the *findings* (are the claims about code, telemetry, and spec true?); the second validation pass checked the *recommendations* (is the proposed change feasible, already implemented, or contradicted by data?). Verdicts below reflect the verification and validation passes. Four recommendations survived and were filed as issues; two were refuted during validation and are recorded as rejected with reasons.

## Answer (short)

Yes to parameter reduction, no to tool-count reduction. At 7 tools the server sits far below every measured tool-selection cliff (~20-46 tools across Speakeasy, Archestra, RAG-MCP evidence); the measured problems are concentrated in (a) a small number of protocol-duplicating or mode-fragmenting parameters and (b) `edit_replace` failure/retry churn, which telemetry and client logs show is the single largest accuracy sink. Removing per-request timeouts is spec-aligned; merging tools is not the lever.

## Summary Table

| # | Finding | Verdict | Validation | Issue |
|---|---------|---------|------------|-------|
| F1 | `timeout_secs` / `drain_timeout_secs` are protocol-level concerns exposed as tool params; MCP cancellation is wired but only logs (`lib.rs:873-881`, `exec_runtime.rs:72-73,163-168`) | CONFIRMED | PARTIAL: feasible but not trivial -- ~20 test references, dedicated `tests/exec_timeout.rs`, no cancellation registry exists; needs removal (no deprecation window -- alpha policy: remove outright, see Alpha policy below) | [#1542](https://github.com/clouatre-labs/aptu-coder/issues/1542) |
| F2 | `edit_replace` is the dominant error/retry sink: 15-19% error rate, ~1,936 client-log INVALID_PARAMS occurrences | CONFIRMED (PARTIAL on counts) | REFUTED as a fix: the proposed UX already exists (ambiguous error carries occurrence count + line numbers + guidance, `edit_replace.rs:303-322`; stale-hash error carries expected/actual hashes, `:393`; `EDIT_STALE_CONTEXT` circuit breaker, `:18-32`). Churn driver is `stale_content_hash` (118 events) vs `ambiguous` (4), and stale-hash guidance is already complete. No issue filed. | none (rejected) |
| F3 | `analyze_symbol` fragments one operation into 12 params with 3 mutually exclusive boolean mode flags that silently override `match_mode`/`follow_depth`/`impl_only` (`types.rs:198-255`, `analyze_symbol.rs:663,714`) | CONFIRMED | CONFIRMED: precedence verified; ~11 integration test call sites affected; ~11 integration test call sites affected; alpha policy: remove the legacy booleans outright (no deprecation window); removing silent overrides is a behavior regression for currently-valid requests, acceptable pre-1.0 | [#1543](https://github.com/clouatre-labs/aptu-coder/issues/1543) |
| F4 | `working_dir` is spec-sanctioned, not redundant: MCP 2026-07-28 deprecates roots (SEP-2577) and names tool parameters as the replacement | CONFIRMED | -- (do-not-remove conclusion stands) | none (no change) |
| F5 | Pagination: client-passed `page_size` is non-normative -- spec makes page size server-owned; cursor is the client-side surface | CONFIRMED (spec-side) | PARTIAL: cursor payload is `{mode, offset}` only, so dropping `page_size` is cursor-safe; but `has_more` does not exist and is not needed -- nullable `next_cursor` already encodes it | [#1544](https://github.com/clouatre-labs/aptu-coder/issues/1544) |
| F6 | Result-shaping booleans (`summary`, `fields`, `def_use`, `impl_only`) should collapse into a constrained `response_format`-style enum | MEDIUM | PARTIAL: `fields` is array-typed (`types.rs:162`), so a flat unit enum is lossy; only a payload-carrying enum works; must be experiment-gated like the 2026-09-11 audit | [#1545](https://github.com/clouatre-labs/aptu-coder/issues/1545) |
| F7 | Merge `analyze_module` into `analyze_file` | proposed as optional | REFUTED: metrics show `analyze_module` (171 calls) outnumbers `analyze_file` (156) and owns a dedicated L2 content-hash cache fast path (`analyze_module.rs:74`). Merging churns the hotter tool for zero simplification | none (rejected) |
| F8 | Metrics DO record per-param presence and `error_subtype` (contrary to one research pass reading an outdated schema); no observability blocker | REFUTED as gap | -- | none (no change) |

## Evidence Detail

### Usage distribution (18-day window, 58,776 calls)

- `exec_command` 72% of calls; top-3 (`exec_command`, `edit_replace`, `edit_overwrite`) = 91.9% (75,827/82,504 over full history). The `analyze_*` family is ~8%.
- Error rates: `edit_replace` 15.4-18.8% (142/873 windowed) -- dominated by `stale_content_hash` (118 events) rather than `ambiguous` (4); `analyze_symbol` 11 errors on 17 calls (small n, worth diagnosing); `exec_command` 0.9%.
- Latency: `analyze_*` p50 4-77 ms; `exec_command` p95 3,606 ms. Performance is not a cognitive-load problem; accuracy is.
- Truncation: 56 events, all `exec_command`, clustered at the ~30 KB cap; output p50 456 chars -- output sizing defaults are largely adequate.

### Spec alignment (MCP 2026-07-28)

- Timeouts belong to the request sender ("implementations SHOULD establish timeouts for all sent requests"; cancellation via `notifications/cancelled`; SEP-1539, still a proposal, explicitly treats requester-defined timeouts as problematic). A per-tool `timeout_secs` duplicates a client/transport concern and burns schema tokens every turn.
- Pagination is opaque-cursor with server-determined page size; clients MUST NOT assume fixed sizes. `page_size` as a client param is allowed but non-normative.
- No spec limit on tool or parameter count; ergonomics guidance (enums, defaults, pagination metadata, progress notifications for long ops) is docs/community level.
- Roots deprecated (SEP-2577): `working_dir` params are the sanctioned replacement. Do not remove.

### Tool-selection evidence

- Speakeasy: selection failure point ~46 tools; ~3x accuracy under ~30 tools. Archestra production traces: non-issue below ~20, cliff ~40. RAG-MCP (Gan & Sun 2025): 13.62% baseline accuracy at 100+ tools vs 43.13% with retrieval. All far above our 7. Parameter-count-specific accuracy curves: UNVERIFIED; guidance is qualitative (Anthropic, Nearform: constrained enums, defaults, flat schemas).

## Recommendations by ROI (validated)

Alpha policy: aptu-coder is pre-1.0 alpha software with no compatibility obligations. Breaking parameter-surface changes are made outright (remove the old surface in the same change) -- no deprecation windows, no dual-parameter parsing. [#1542](https://github.com/clouatre-labs/aptu-coder/issues/1542)-[#1545](https://github.com/clouatre-labs/aptu-coder/issues/1545) have been amended accordingly.

1. **Remove `timeout_secs`/`drain_timeout_secs` from `exec_command`; wire real cancellation.** -- [#1542](https://github.com/clouatre-labs/aptu-coder/issues/1542)
2. ~~**Attack `edit_replace` error churn.**~~ REJECTED at validation: the structured-error UX already exists; churn is stale-hash-driven with complete guidance. No action.
3. **Collapse `analyze_symbol` modes into one enum** -- [#1543](https://github.com/clouatre-labs/aptu-coder/issues/1543)
4. **Make pagination server-owned**: drop client `page_size`, keep opaque `cursor`; `next_cursor` already encodes "has more" -- [#1544](https://github.com/clouatre-labs/aptu-coder/issues/1544)
5. **Collapse result-shaping params into a payload-carrying `response_format` enum**, experiment-gated -- [#1545](https://github.com/clouatre-labs/aptu-coder/issues/1545)
6. ~~**Merge `analyze_module` into `analyze_file`.**~~ REJECTED at validation: `analyze_module` has higher usage and a dedicated L2 cache fast path. No action.

Do NOT: reduce tool count further (at 7 tools the server sits at 15-35% of the lowest measured selection cliff, ~20 tools -- no evidence of selection pressure), remove `working_dir` (spec-sanctioned post-[SEP-2577](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2577)), or touch description verbosity (prior experiment: null effect; real errors clustered in cursors and stale hashes).

## Impact (measured facts only)

- #1542 removes 2 of `exec_command`'s parameters (the protocol-duplicating timeouts) and converts log-only cancellation into process kill.
- #1543 replaces 3 interacting booleans with 1 defaulted enum on the heaviest tool (12 params).
- #1544 removes `page_size` from 3 tools (analyze_directory, analyze_file, analyze_symbol).
- #1545 replaces 4 result-shaping params with 1 shape, gated on experiment data (no pre-claimed benefit).
- Expected token/accuracy deltas are NOT claimed here; they must come from the #1545 experiment framework and post-rollout telemetry.

## Future Work (non-blocking)

- `stale_content_hash` churn (118 events) is the dominant `edit_replace` error driver with complete guidance already returned; the `EDIT_STALE_CONTEXT` circuit breaker (`edit_replace.rs:18-32`) does not currently count `stale_content_hash` toward its threshold. Whether it should is a separate, small decision.
- `analyze_symbol`'s 11/17 error rate (small n) deserves a diagnosis before #1543 lands, to avoid encoding a broken path into the new `mode` enum.

## Sources

- [MCP 2026-07-28 cancellation](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/cancellation), [SEP-1539](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1539), [MCP pagination](https://modelcontextprotocol.io/specification/2025-03-26/server/utilities/pagination), [MCP 2026-07-28 architecture](https://modelcontextprotocol.io/docs/2026-07-28/learn/architecture)
- [Anthropic: writing tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents)
- [Speakeasy: less is more](https://www.speakeasy.com/mcp/tool-design/less-is-more/), [Archestra: how many MCP tools](https://archestra.ai/blog/how-many-mcp-tools-too-many), [RAG-MCP (arXiv 2605.24660)](https://arxiv.org/html/2605.24660v1), [RunPod: designing MCP tools](https://www.runpod.io/blog/designing-mcp-tools), [Nearform MCP pitfalls](https://nearform.com/digital-community/implementing-model-context-protocol-mcp-tips-tricks-and-pitfalls/)
- Internal: `~/.local/share/aptu-coder/metrics-*.jsonl`, `~/.claude/projects`, `crates/aptu-coder/src/tools/`, `crates/aptu-coder-core/src/types.rs`, `docs/METRICS.md`
