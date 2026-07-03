# Audit: Performance and Token Efficiency, June 2026

Date: 2026-06-13

## Purpose

Point-in-time audit of `aptu-coder` against two operating goals:

1. **Faster execution**: reduce avoidable filesystem work, parsing work, subprocess latency, and cache misses.
2. **Lower token cost**: reduce duplicated response payloads, avoid unnecessary large command output, and keep tool guidance precise enough to steer agents toward low-cost calls.

The audit is intended to be the source material for GitHub issues. Each confirmed finding includes current evidence, impact, fix direction, and acceptance criteria.

Tracking issue: [#1047](https://github.com/clouatre-labs/aptu-coder/issues/1047)

## Scope

- Current branch base: `origin/main`, fetched on 2026-06-13.
- Current MCP surface: `analyze_directory`, `analyze_file`, `analyze_module`, `analyze_symbol`, `edit_overwrite`, `edit_replace`, `exec_command`.
- Current local metrics corpus from `scripts/mcp-metrics.py --format json`.
- Current repo source under `crates/aptu-coder` and `crates/aptu-coder-core`.
- Official external references for MCP and tool-use efficiency. Context7 and Brave Search tools were not available in this session.

## Methodology

- Refreshed repository metadata with `git fetch -p`.
- Checked open PRs with `gh pr list`.
- Read the current issue templates under `.github/ISSUE_TEMPLATE/`.
- Verified each finding against source with `rg`, targeted source reads, and local metrics.
- Separated current findings from historical audit items. The earlier code-quality audit remains a historical point-in-time document at [2026-06-code-quality.md](2026-06-code-quality.md).

## Metrics Snapshot

Command used:

```sh
UV_CACHE_DIR=/private/tmp/aptu-uv-cache uv run python scripts/mcp-metrics.py --format json
```

*Table 1: Local tool metrics used for prioritization.*

| Tool | Calls | p50 ms | p95 ms | p99 ms | p95 chars | Cache hit rate | Truncated |
|---|---:|---:|---:|---:|---:|---:|---:|
| `exec_command` | 56,589 | 95 | 3,862 | 32,699 | 7,851 | 0.0% | 62.56% |
| `edit_replace` | 6,153 | 2 | 5 | 7 | 165 | n/a | 0.0% |
| `edit_overwrite` | 1,458 | 1 | 3 | 5 | 132 | n/a | 0.0% |
| `analyze_file` | 1,312 | 3 | 457 | 520 | 7,536 | 69.61% | 0.0% |
| `analyze_directory` | 574 | 115 | 1,028 | 1,047 | 1,758 | 30.31% | 0.0% |
| `analyze_module` | 269 | 5 | 452 | 514 | 6,442 | 55.92% | 0.0% |
| `analyze_symbol` | 39 | 208 | 534 | 620 | 796 | 0.0% | 0.0% |

Observed cache impact:

- `analyze_directory`: median hit 20 ms, median miss 419 ms, estimated 69,513 ms saved.
- `analyze_file`: median hit 2 ms, median miss 118 ms, estimated 90,596 ms saved.
- `analyze_module`: median hit 2 ms, median miss 98 ms, estimated 13,220 ms saved.
- `analyze_symbol`: 19 cacheable calls, 0 hits.
- `exec_command`: 56,589 cacheable calls in metrics, 0 hits.

## Summary

*Table 2: Confirmed findings and issue mapping.*

| ID | Severity | Type | Finding | Issue | Status |
|---|---|---|---|---|---|
| F1 | High | Bug | `exec_command` cache behavior is documented but inactive | [#1039](https://github.com/clouatre-labs/aptu-coder/issues/1039) | Closed |
| F2 | High | Refactor | `analyze_module` does full file analysis instead of the existing module fast path | [#1046](https://github.com/clouatre-labs/aptu-coder/issues/1046) | Open |
| F3 | High | Refactor | Shallow `analyze_directory` still performs an unbounded tree walk | [#1044](https://github.com/clouatre-labs/aptu-coder/issues/1044) | Open |
| F4 | Medium | Refactor | Directory analysis reads eligible files twice | [#1041](https://github.com/clouatre-labs/aptu-coder/issues/1041) | Open |
| F5 | Medium | Feature | `analyze_symbol` has no effective reusable analysis cache | [#1040](https://github.com/clouatre-labs/aptu-coder/issues/1040) | Open |
| F6 | High | Feature | `analyze_file(fields=...)` limits text but not structured semantic payload | [#1045](https://github.com/clouatre-labs/aptu-coder/issues/1045) | Open |
| F7 | Medium | Feature | JSONL metrics cannot show which exec filters fired | [#1042](https://github.com/clouatre-labs/aptu-coder/issues/1042) | Open |
| F8 | Low | Refactor | Tool guidance needs a small token-efficiency pass, not a tool-count redesign | [#1043](https://github.com/clouatre-labs/aptu-coder/issues/1043) | Open |

## Findings

### F1: `exec_command` cache behavior is documented but inactive

**Issue type:** Bug

**Observed state:**

- `ExecCommandParams` exposes command execution parameters in [crates/aptu-coder/src/lib.rs](../../crates/aptu-coder/src/lib.rs).
- The handler computes a cache key at `lib.rs:3117`.
- The next comment states that execution caching is disabled and explicit `cache=true` is not implemented at `lib.rs:3125`.
- Metrics always report `cache_hit: Some(false)` for `exec_command` at `lib.rs:3260`.
- The README documents output filters and server behavior, while local metrics classify `exec_command` calls as cacheable with 0 hits.

**Evidence from metrics:**

- `exec_command` has 56,589 calls, more than every other tool combined.
- p95 latency is 3,862 ms.
- p99 latency is 32,699 ms.
- 62.56% of responses were truncated.
- Cache hit rate is 0.0%.

**Impact:**

- This is the largest observed latency opportunity.
- Documentation and runtime behavior diverge.
- Caching command output only reduces model tokens if clients avoid reinjecting repeated output or consume a cache indicator. It still reduces subprocess latency directly.

**Fix direction:**

- Choose one product behavior and make source, docs, and metrics agree:
  - Implement explicit `cache=true` for deterministic read-only commands, or
  - Remove advertised exec cache behavior and report exec as non-cacheable.
- If caching is implemented, key it on command, working directory, stdin content hash, relevant limits, and filter version.
- Do not default-cache arbitrary commands with side effects.
- Preserve `output_truncated`, exit code, timeout state, and filter behavior in cached responses.

**Acceptance criteria:**

- Tests cover `cache=true` hit and miss behavior, or docs and metrics no longer advertise exec caching.
- `cache_hit` and `cache_tier` are accurate for successful cached exec calls.
- Non-zero exit and timed-out command caching behavior is explicit and tested.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F2: `analyze_module` does full file analysis instead of the existing module fast path

**Issue type:** Refactor

**Observed state:**

- The MCP handler routes `analyze_module` through `handle_file_details_mode` at `lib.rs:2312`.
- It reconstructs `ModuleInfo` from `FileAnalysisOutput` at `lib.rs:2359`.
- Core already exposes `analyze_module_file` at [crates/aptu-coder-core/src/analyze.rs](../../crates/aptu-coder-core/src/analyze.rs).
- The parser has an `extract_module_info` fast path documented as functions and imports only at [crates/aptu-coder-core/src/parser.rs](../../crates/aptu-coder-core/src/parser.rs).
- That fast path skips calls, references, and impl-trait extraction.

**Evidence from metrics:**

- `analyze_module` p95 latency is 452 ms.
- `analyze_file` p95 latency is 457 ms.
- The near-equal p95 values match the source path, because both tools currently use full file analysis on misses.

**Impact:**

- `analyze_module` is positioned as the low-cost orientation tool, but cache misses still pay full parse cost.
- Token output is already smaller than `analyze_file`; the remaining problem is unnecessary CPU and wall time.

**Fix direction:**

- Route `analyze_module` misses through `analyze_module_file` or the parser module fast path.
- Add or reuse a module-specific cache keyed by file identity and content or mtime.
- Keep output schema and formatting unchanged.

**Acceptance criteria:**

- `analyze_module` results are byte-equivalent or intentionally documented as equivalent for existing test fixtures.
- Tests cover cache miss and cache hit behavior.
- Existing `analyze_module` integration tests pass.
- Metrics still report accurate `cache_hit` and `cache_tier`.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F3: Shallow `analyze_directory` still performs an unbounded tree walk

**Issue type:** Refactor

**Observed state:**

- `handle_overview_mode` calls `walk_directory(path, None)` at `lib.rs:545`.
- A nearby comment states that the walk is unbounded and depth filtering happens in memory at `lib.rs:544`.
- Cache keys are built from all entries before depth filtering at `lib.rs:560`.
- Disk cache hashing also iterates the full entry set at `lib.rs:576`.

**Evidence from metrics:**

- `analyze_directory` p95 latency is 1,028 ms.
- Cache hits are much cheaper than misses: median hit 20 ms, median miss 419 ms.

**Impact:**

- A shallow orientation request such as `max_depth=2` still pays the cost of walking and hashing deeper files.
- This cost is paid before cache lookup can succeed, because the cache key is derived from the full entry list.

**Fix direction:**

- Use bounded traversal when `max_depth` is set and exact subtree totals are not required.
- Preserve existing behavior where exact total counts are part of the response contract, or introduce an explicit option for exact recursive totals.
- Build bounded cache keys from the bounded entry set for bounded requests.

**Acceptance criteria:**

- A test proves `max_depth=2` does not require full recursive traversal for the fast path.
- Existing summary and pagination behavior remains correct.
- Cache keys distinguish bounded and unbounded directory analyses.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F4: Directory analysis reads eligible files twice

**Issue type:** Refactor

**Observed state:**

- `check_file_eligibility` reads file content to check readability at [crates/aptu-coder-core/src/analyze.rs:153](../../crates/aptu-coder-core/src/analyze.rs).
- `analyze_single_file` calls `check_file_eligibility` at `analyze.rs:209`.
- `analyze_single_file` then reads the same file again at `analyze.rs:215`.

**Impact:**

- Directory analysis does extra filesystem work for each eligible source file.
- This is a direct CPU and I/O reduction opportunity with no intended behavior change.

**Fix direction:**

- Split size/language eligibility from content loading, or return loaded source from the eligibility step.
- Preserve current handling for oversized, binary, unreadable, and unsupported files.

**Acceptance criteria:**

- Tests cover readable file, oversized file, and unreadable or binary file behavior.
- Existing directory analysis output remains unchanged for fixtures.
- `cargo test -p aptu-coder-core` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F5: `analyze_symbol` has no effective reusable analysis cache

**Issue type:** Feature

**Observed state:**

- `analyze_symbol` records `cache_tier` as `Miss` on success paths at `lib.rs:2176` and related metric emission paths.
- A unit test explicitly protects the `CacheTier::Miss` metric string for `analyze_symbol` at `lib.rs:4895`.
- Local metrics show 19 cacheable `analyze_symbol` calls and 0 hits.

**Evidence from metrics:**

- `analyze_symbol` p50 latency is 208 ms.
- `analyze_symbol` p95 latency is 534 ms.
- Output sizes are small, p95 796 chars, so the main opportunity is latency.

**Impact:**

- Repeated symbol lookups over the same directory cannot reuse parsed semantics or call graph state.
- The tool is already token-frugal, but not as fast as it can be for repeated navigation.

**Fix direction:**

- Add a reusable semantic graph or per-file semantic cache that `analyze_symbol` can query.
- Key cached state on root path, `git_ref`, depth, language set, AST recursion limit, file metadata, and any parameters that affect extraction.
- Reuse existing file and directory cache primitives where they fit.

**Acceptance criteria:**

- Repeated `analyze_symbol` calls over unchanged input produce a cache hit.
- Cache invalidates when relevant source files change.
- Tests cover at least one caller/callee lookup and one invalidation case.
- Metrics report accurate `cache_hit` and `cache_tier`.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F6: `analyze_file(fields=...)` limits text but not structured semantic payload

**Issue type:** Feature

**Observed state:**

- The `analyze_file` tool description recommends `fields=["functions","classes","imports"]` to limit output sections.
- Formatting receives `params.fields` at `lib.rs:1686`.
- The final structured response is built with `arc_output.semantic.clone()` at `lib.rs:1708`.
- The `structuredContent` is then serialized from the full `FileAnalysisOutput` at `lib.rs:1727`.

**External constraint:**

- The MCP 2025-11-25 tool spec allows both `structuredContent` and fallback `content`. When an `outputSchema` is defined, `structuredContent` must conform to it.

**Impact:**

- `fields` reduces text content, but does not reduce the semantic object if a client includes structured output in model context.
- This can erase part of the intended token savings for large files.

**Fix direction:**

- Apply field projection to `structuredContent` while preserving schema conformance, for example by returning empty arrays for omitted sections.
- Alternatively, add an explicit compact structured response mode with a matching output schema.
- Preserve current full structured output when no projection is requested.

**Acceptance criteria:**

- A test proves `fields=["functions"]` omits or empties classes and imports in structured output according to the chosen schema.
- Existing clients receive schema-conformant structured output.
- Text output behavior remains unchanged.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F7: JSONL metrics cannot show which exec filters fired

**Issue type:** Feature

**Observed state:**

- `ShellOutput` includes `filter_applied`.
- The README states that `filter_applied` appears in `structuredContent`.
- [docs/METRICS.md](../METRICS.md) states that `filter_applied` is not recorded in JSONL.

**Impact:**

- The metrics corpus can show that `exec_command` output is frequently truncated, but it cannot show which filters reduced output.
- Filter tuning would require guessing or adding temporary instrumentation.

**Fix direction:**

- Add `filter_applied: Option<String>` to JSONL metric events for `exec_command`.
- Record only the rule name, not command text or output content.
- Update observability docs and metrics tests.

**Acceptance criteria:**

- JSONL records include `filter_applied` when a filter fires.
- JSONL records omit or null the field when no filter fires.
- Existing metrics parsing remains backward compatible.
- Tests cover filtered and unfiltered exec commands.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

### F8: Tool guidance needs a small token-efficiency pass, not a tool-count redesign

**Issue type:** Refactor

**Observed state:**

- The current server exposes 7 tools.
- The tool descriptions are specific and include routing guidance, for example `analyze_file` tells users to prefer `analyze_module` for a lightweight function/import index.
- Prior benchmark notes in this repository record that promoting `analyze_module` as a low-cost orientation tool reduced unnecessary deep dives.

**External guidance:**

- Anthropic documents that tool definitions are part of input tokens and are billed.
- Anthropic also documents that detailed tool descriptions improve tool selection.
- Anthropic positions tool search for hundreds or thousands of tools, which is not the current `aptu-coder` case.

**Impact:**

- Tool search is not justified for the current 7-tool surface.
- Tool descriptions still deserve a focused pass to remove duplication while preserving routing guidance that benchmarks showed to be useful.

**Fix direction:**

- Keep the current small tool surface.
- Retain guidance that steers agents toward `analyze_directory`, then `analyze_module`, then `analyze_file` or `analyze_symbol`.
- Remove repeated wording that is already enforced by JSON schema or validation errors.
- Do not make descriptions terse enough to harm tool selection.

**Acceptance criteria:**

- Tool descriptions keep explicit routing rules for common choices.
- Duplicated parameter explanations are reduced where schema descriptions already cover them.
- Tests that inspect tool schemas continue to pass.
- `cargo test` passes.
- `cargo clippy -- -D warnings` passes.
- `cargo fmt --check` passes.

## Best Practices Applied

### MCP structured output

The MCP 2025-11-25 spec supports `structuredContent`, fallback `content`, and tool annotations. `aptu-coder` already returns structured output for successful tool calls. The token-efficiency issue is not the existence of structured output; it is that projected calls can still return full structured semantics.

Reference: <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>

### Tool descriptions and tool count

Anthropic's tool-use documentation says tool definitions are input tokens and are billed. It also says detailed descriptions improve tool use. The current `aptu-coder` surface is small, so the right action is a description quality pass, not a tool-search architecture.

References:

- <https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/implement-tool-use>
- <https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/overview>

### Cost controls

OpenAI cost guidance emphasizes minimizing tokens, caching repeated context, and controlling output size. The confirmed `aptu-coder` candidates map directly to those controls: reduce duplicated structured payloads, restore or remove stale cache behavior, improve cache reuse, and instrument filters before tuning them.

Reference: <https://developers.openai.com/api/docs/guides/cost-optimization>

## Non-Findings

- No evidence supports adding tool search for the current 7-tool server.
- No issue should be created from the historical dependency audit without revalidating the finding on current `origin/main`.
- No issue should ask for default caching of arbitrary `exec_command` calls. That would cache side-effectful commands unless additional safety constraints are implemented.
