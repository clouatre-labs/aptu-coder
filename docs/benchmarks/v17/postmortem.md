# v17 Post-mortem: Investigation of the Discarded Benchmark Execution

Analysis of the artifacts preserved at commit `663467f` on `bench/v17-results`, conducted read-only. The v17 execution remains discarded as a measurement; this document treats the artifacts as observational data for diagnosis only.

## Scope limitation

The central fact of this post-mortem invalidates most of the original analysis questions. Every quantitative comparison between the MCP conditions (A, C) and the native conditions (B, D) in v17 — tokens, cost, latency, quality — compares two agents doing *different tasks*: the MCP agents never accessed the target repository at all, while the native agents did. Such comparisons measure nothing about MCP tools versus native tools and are not reported as findings below except where noted.

## Headline finding

**No MCP run in any condition ever read a single file of the Django checkout.** Across all six A/C sessions, every absolute attempt at the Django path failed with the exact error `path is outside the working directory`, and every relative attempt failed with `path not found`. The only successful `analyze_directory` calls listed files of the aptu-coder repository itself, for example `./CONTRIBUTING.md`, `./Cargo.lock`, and `./crates`. All A/C scores, including Sonnet's 9/9, were produced from the model's prior knowledge of Django's contrib.auth plus error recovery. The scores.json note attributing C-scored-1's failure to "model-path-choice variance" is incorrect: the path failure was universal. The only inter-run variation was post-failure behavior — Sonnet and C-scored-2 reconstructed plausible answers from memory, while C-scored-1 gave up and emitted an empty `auth_module_map`.

Root cause: the MCP config stub (`mcp-aptu-coder-only.json`, byte-identical to v12's) specifies no `cwd`, so the stdio server inherited the benchmark-runner repo root as its sandbox. `validate_path()` in `crates/aptu-coder/src/validation.rs` canonicalizes the server's `std::env::current_dir()` as the allowed root and rejects anything outside it. The prompt, meanwhile, substitutes `TARGET_REPO_PATH=/tmp/benchmark-repos/django` — outside that root by construction. A correctly behaving server *must* reject these calls; the harness pointed the agent at a path the server could never accept.

## Hypothesis verdicts

### H1: Payload bloat from analyze_directory results — refuted as stated

Tool-result sizes were measured by pairing `tool_use` blocks with `toolUseResult` entries by `tool_use_id`:

| Run | Tool calls | Total result bytes | Cumulative re-send bytes | Telemetry input tokens |
|---|---:|---:|---:|---:|
| A-pilot-1 | 4 | 6,121 | 12,160 | 146,277 |
| A-scored-1 | 4 | 7,060 | 7,157 | 163,683 |
| A-scored-2 | 6 | 9,613 | 30,675 | 268,283 |
| B-pilot-1 | 14 | 34,723 | 142,092 | 239,142 |
| B-scored-1 | 12 | 33,622 | 134,799 | 208,237 |
| B-scored-2 | 12 | 44,237 | 143,446 | 211,742 |
| C-pilot-1 | 5 | 3,777 | 7,358 | 208,279 |
| C-scored-1 | 7 | 8,072 | 36,107 | 347,858 |
| C-scored-2 | 7 | 3,067 | 9,216 | 290,287 |
| D-pilot-1 | 13 | 49,488 | 272,908 | 414,800 |
| D-scored-1 | 18 | 62,168 | 326,179 | 502,792 |
| D-scored-2 | 14 | 44,307 | 243,583 | 388,505 |

The MCP sessions' tool results are tiny — 3 to 10 KB per session, the largest single result being 6,927 bytes (a per-file index with `cache_tier`, `class_count`, `function_count`, `line_count`, and `path` fields). The recurring 46-byte results are the error string `Error: path is outside the working directory`. Native sessions carry 33 to 62 KB of results with 134 to 326 KB of cumulative re-send. Large `analyze_directory` payloads do not explain MCP token counts.

One observational finding does survive the scope limitation, because it is about session mechanics rather than task performance: MCP sessions began with roughly 35,000 to 40,000 input tokens on the very first API call, versus about 20,000 to 22,000 for native sessions. Per-turn input then grew by only a few hundred tokens across tool calls (A-scored-1: 40,021 → 40,021 → 40,266 → 40,456 → 42,940), confirming that tool results were not the cost driver; the static context was. This is measured on degenerate sessions but reflects fixed per-session cost, not task behavior.

Measured against the live server (homebrew aptu-coder 0.34.1, the build the runner invokes): `tools/list` returns 7 tools totaling 27,610 bytes (~6.9k tokens — `analyze_symbol` 7,153 B, `analyze_file` 6,616 B, `analyze_directory` 3,117 B, `analyze_module` 2,709 B, `exec_command` 3,595 B, `edit_replace` 3,225 B, `edit_overwrite` 1,195 B) plus roughly 223 tokens of server instructions. The server directly contributes about 7.1k tokens of static context, including ~2k tokens of edit/exec schemas that v17 conditions disallowed but the server ships unconditionally. The remainder of the ~15–18k first-turn MCP-vs-native delta is client-side (Claude Code's MCP integration), not aptu-coder payload.

**Correction of a draft claim**: an earlier draft stated that "static MCP context tripled from ~14k to ~40k tokens/turn." That was wrong — it compared a v12 *mean-per-turn* figure to a v17 *first-turn base*. What the data supports is that the **MCP-vs-native per-turn premium** grew roughly threefold while native per-turn context also roughly doubled, so much of the absolute growth is client-side and affects both arms:

| Mean input tokens per turn | v12 | v17 |
|---|---|---|
| Sonnet native (B) | 10,547 / 12,429 | ~16,018 |
| Sonnet MCP (A) | 17,424 / 16,722 (runs 3–4: 13,942 / 15,792) | 32,737 / 38,326 |
| Haiku native (D) | 26,888 / 24,254 | 26,463 / 25,900 |
| Haiku MCP (C) | 33,098–48,986 | 38,651 / 36,286 |

The v12 Sonnet MCP premium was ~4–7k tokens/turn; v17's was ~17–22k. Of the v17 premium, ~7.1k is directly attributable to server payload; attribution of the rest between server schema growth since v12 and client-side MCP context is unresolved (v12-era schema sizes were not archived). Note also that the runner set `DISABLE_PROMPT_CACHING=1`, so this static context was re-billed at full price every turn — a harness choice, applied to both arms, now removed (see below).

### H2: Sandbox/path failure — confirmed, and universal

Complete inventory of every aptu-coder tool call in all A/C sessions, with verbatim path arguments and results. The only aptu-coder tool ever invoked was `analyze_directory` (plus the CLI's `StructuredOutput` to finish):

| Session | `path` argument (verbatim) | Result |
|---|---|---|
| A-pilot-1 | `/tmp/benchmark-repos/django/django/contrib/auth` | `path is outside the working directory` |
| A-pilot-1 | `.` | success (aptu-coder repo) |
| A-pilot-1 | `/Users/hugues.clouatre/git` | `path is outside the working directory` |
| A-scored-1 | `/tmp/benchmark-repos/django/django/contrib/auth` | `path is outside the working directory` |
| A-scored-1 | `/tmp/benchmark-repos/django` | `path is outside the working directory` |
| A-scored-1 | `.` | success (aptu-coder repo) |
| A-scored-2 | `/tmp/benchmark-repos/django/django/contrib/auth` | `path is outside the working directory` |
| A-scored-2 | `.` | success (aptu-coder repo) |
| A-scored-2 | `/tmp/benchmark-repos` | `path is outside the working directory` |
| A-scored-2 | `/Users/hugues.clouatre` | `path is outside the working directory` |
| A-scored-2 | `./scripts` | success (aptu-coder repo) |
| C-pilot-1 | `/tmp/benchmark-repos/django/django/contrib/auth` | `path is outside the working directory` |
| C-pilot-1 | `django/contrib/auth` | `path not found` |
| C-pilot-1 | `.` | success (aptu-coder repo) |
| C-pilot-1 | `crates` | success (empty: `0 files (0 prod, 0 test), 0L, 0F, 0C (max_depth=1)`) |
| C-scored-1 | `/tmp/benchmark-repos/django/django/contrib/auth` | `path is outside the working directory` |
| C-scored-1 | `.` | success (aptu-coder repo) |
| C-scored-1 | `../django` | `path not found` |
| C-scored-1 | `./django` | `path not found` |
| C-scored-1 | `/benchmark-repos/django/django/contrib/auth` | `path not found` |
| C-scored-1 | `./crates` | success (aptu-coder repo) |
| C-scored-2 | `/tmp/benchmark-repos/django/django/contrib/auth` | `path is outside the working directory` |
| C-scored-2 | `./django/contrib/auth` | `path not found` |
| C-scored-2 | `django/contrib/auth` | `path not found` |
| C-scored-2 | `.` | success (aptu-coder repo) |
| C-scored-2 | `/django/contrib/auth` | `path not found` |
| C-scored-2 | `../django/django/contrib/auth` | `path not found` |

Classification:

1. **Harness/prompt defect (primary).** The prompt target is outside the server's working directory; a correctly behaving server must reject it.
2. **Server usability defect (contributing).** The error text `path is outside the working directory` never states what the working directory actually is. The structured `suggestedAction` (`provide a path within the current working directory`) exists but also omits the root, so every model burned calls guessing path forms (`/Users/hugues.clouatre`, `../django`, `/django/...`).
3. **Model variance (consequence only).** It explains 9/9 versus 6 versus 1 in condition C, not the failure itself.

### H3: Tool-selection collapse — confirmed as observation; cause is H2, so the server-side question is unresolved

`analyze_file`, `analyze_module`, and `analyze_symbol` were called zero times in all six A/C sessions. The condition prompts' recipes are internally coherent after the drift corrections: step 2's `analyze_file` on "2-3 key files identified above" depends on step 1's directory overview — which always failed. The recipe never advanced past step 1 in any run. Git history between v12 and v17 (`bfe0e96` tree output by default plus a 5K auto-summary threshold, `bd3229e` default `max_depth=3`, `e67c792`/`5800b2b` server-owned paging) plausibly made `analyze_directory` output richer, but this dataset cannot test whether that changes drill-down behavior, because no session ever saw a successful target-repo analysis.

### H4: v12 comparison integrity — partially confirmed

The "59%" claim lives in the README ("cutting token usage by up to 59%"; MCP Sonnet 112k tokens / $0.39 vs native 276k / $0.95). It is computed as median input tokens with the MCP side using only A runs 3–4 (the improved-build re-run) against the full n=2 native condition. Recomputed aggregation matrix (v12, A vs B):

| Aggregation | A | B | MCP reduction |
|---|---:|---:|---:|
| input, median, A runs 3–4 (README basis) | 111,966 | 276,291 | 59.5% |
| total tokens, median, all 4 A runs | 134,489 | 284,025 | 52.6% |
| input, mean, all 4 A runs | 128,633 | 276,291 | 53.4% |
| input, median, first 2 A runs | 145,300 | 276,291 | 47.4% |

The headline is inflated by roughly 7 points via best-subset selection, but the underlying v12 effect is robust at 47–59% under every aggregation. The v17 Sonnet collapse to −3% therefore survives conservative re-aggregation. However, given the H2 finding, v12's MCP arm validity is itself in question: v12 preserved no session transcripts (all `.log` files are zero bytes; A-scored-3 has none), so it cannot be verified that v12's MCP runs ever accessed Django either. If they did not, v12's "cheaper and equal quality" could be the same prior-knowledge artifact at one-third the per-turn overhead.

### H5: Wall-time / API-time — partially confirmed; accounting is benign

Across all 12 runs: `api_time` ≈ `wall_time` holds for all six A/C runs (ratio 1.006–1.027); A-scored-2's api 137,949 ms exceeding wall 137,508 ms is a 441 ms (0.32%) excess consistent with timer ordering, not a defect, and the same ratio pattern appears in every A/C run. Native runs B-scored-1, D-scored-1, and D-scored-2 show 25–39% of wall time outside api time (client-side Bash/Read execution), so native wall times overstate API cost. Large MCP tool results do not inflate latency (results ≤7 KB, per-turn input nearly flat); the uncached ~40k-token static context plausibly does — A runs averaged ~17–20 s/turn versus B's ~7–8.5 s/turn — but since the MCP sessions were degenerate, this is an observation about session mechanics, not a benchmark finding.

## Root-cause fix validation (2026-09-15, live test against aptu-coder 0.34.1)

Tested via direct MCP stdio (initialize / tools/list / tools/call):

1. **Reproduction** — server cwd = aptu-coder repo, `analyze_directory(path="/tmp/benchmark-repos/django/django/contrib/auth")` returns `isError: true`, text `path is outside the working directory`, `suggestedAction` = `provide a path within the current working directory`. The payload nowhere states the working directory.
2. **Fix** — server cwd = `/tmp/benchmark-repos/django`, `analyze_directory(path="django/contrib/auth", max_depth=1, summary=true)` returns the full tree: `SUMMARY: 19 files (19 prod, 0 test), 4839L, 384F, 76C (max_depth=1)` including `base_user.py [164L, 27F, 3C]` and `models.py [634L, 67F, 14C]`. The absolute `TARGET_REPO_PATH` form also succeeds from this cwd, because the path canonicalizes inside the allowed root.

Conclusion: launching the CLI (and therefore the stdio MCP server) with working directory = Django checkout fully resolves the MCP-arm failure. The runner now does this for all four conditions equally.

## Defects and non-defects, ranked

1. **Harness defect (v17, likely inherited from v12):** MCP server launched without `cwd`, so its sandbox root was the runner repo while the prompt targeted `/tmp/benchmark-repos/django`. Invalidated the entire MCP arm.
2. **Server cost contributor:** the MCP-vs-native per-turn input premium grew from ~4–7k (v12) to ~17–22k tokens (v17, Sonnet). About 7.1k tokens/turn of the v17 premium is directly measured server payload (27,610-byte `tools/list` plus 892-char instructions), including ~2k tokens of edit/exec schemas shipped even when disallowed by the client allowlist. Attribution of the remainder is unresolved. With caching disabled by the harness, this premium was re-billed every turn.
3. **Server usability defect:** `path is outside the working directory` omits the actual working-directory root; models cannot self-correct and waste calls guessing.
4. **README aggregation defect (v12):** "up to 59%" compares best-subset MCP re-runs against the full native condition; the honest all-runs figure is ~53%.
5. **scores.json misdiagnosis:** C-scored-1 attributed to "model-path-choice variance" when the path failure was universal.
6. **Non-defects:** `analyze_directory` result size (tiny and well-bounded); api_time exceeding wall_time in A-scored-2 (0.3%, benign); the condition prompts' recipes (coherent, simply never reached step 2); native wall-time inflation (client-side tool execution, not API).

## Attribution

Because the MCP arm never performed the task, no part of the observed v17 "MCP disadvantage" can be attributed to MCP tooling at all. What can be said:

- **(a) aptu-coder server behavior:** a real cost contributor (~7.1k tokens/turn of measured static payload, ~2k of it disallowed-tool schemas) and a real usability defect in the path-validation error. Neither was the cause of the quality or token anomaly.
- **(b) Prompt/recipe/harness design:** the cause of the anomaly. The cwd misconfiguration made the MCP arm measure prior knowledge instead of tool use.
- **(c) Model variance at n=2:** explains the quality spread within condition C (9/9, ~6, ~1) and nothing about tokens.
- **(d) v12 baseline inflation:** ~7 points of the 59% headline is best-subset aggregation; the remaining ~53% v12 advantage does not reproduce in v17, but v12's MCP arm is unverifiable (zero-byte logs) and may share the same harness flaw, in which case v12's advantage was partly the same prior-knowledge artifact at lower per-turn overhead.

## Follow-ups executed in PR 1562 (2026-09-15)

- Runner: CLI launched with cwd = Django checkout (methodology correction 8); pre-flight MCP gate added (correction 10); `DISABLE_PROMPT_CACHING=1` removed (correction 9). Validated live against aptu-coder 0.34.1.

## Recommended follow-ups (not executed)

- Server: include the canonical working-directory root in the `path is outside the working directory` error (validation.rs already carries an `error_meta` suggestion; add the root to it).
- Server: investigate trimming the static context — ship only client-allowlisted tool schemas where the protocol allows, and audit the v12-to-v17 growth of tool descriptions.
- Benchmark design: use a target repository the models do not know (synthetic or deliberately obscure). Sonnet's 9/9 from zero target reads shows prior knowledge can fully mask tool-access failure on a famous codebase; consider scorer verification that cited `file:line` anchors exist in the checkout.
- Re-run the v12 MCP arm with transcripts preserved to determine whether v12's MCP runs ever accessed the target repo.
