# v17 Post-mortem: Investigation of the Discarded Benchmark Execution

Status: analysis of `663467f` artifacts on `bench/v17-results`, read-only.
The v17 execution remains discarded as a measurement; this document treats the
artifacts as observational data only.

## Headline finding (supersedes the framing of all five hypotheses)

**The MCP arm never analyzed the Django checkout in any run.** Across all six
A/C sessions, every absolute attempt at the Django path failed with the exact
error `path is outside the working directory`, and every relative attempt failed
with `path not found`. The only successful `analyze_directory` calls listed
files of the aptu-coder repository itself (e.g. `./CONTRIBUTING.md`,
`./Cargo.lock`, `./crates`). All A/C scores -- including Sonnet's 9/9 -- were
produced from model prior knowledge of Django's contrib.auth plus error
recovery, not from MCP tool output. The A-vs-B and C-vs-D comparisons in v17
measure "prior knowledge + recovery" vs "actual repo access", not MCP vs native.

## Hypothesis verdicts

### H1. Payload bloat from analyze_directory results -- REFUTED as stated; modified mechanism CONFIRMED

Measured tool_result sizes (paired `tool_use`/`toolUseResult` by id):

| run | tool calls | total result bytes | cumulative re-send bytes | telemetry input_tokens |
|---|---|---|---|---|
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

MCP tool results are tiny (3-10 KB total; the largest single result is 6,927
bytes -- a per-file index with `cache_tier`, `class_count`, `function_count`,
`line_count`, `path` fields). Native sessions carry 33-62 KB of results with
134-326 KB of cumulative re-send. The 46-byte MCP results are the repeated
error string `Error: path is outside the working directory`.

The actual input-token driver in MCP sessions is a **~40,000-token static
per-turn base context** (~40,008-40,021 tokens on the first API call of every
A/C session vs ~22,017 for native sessions). With `cache_read_tokens: 0` and
`cache_creation_tokens: 0` everywhere, that ~18k/turn delta is re-billed every
turn: 5-9 turns x ~40k = the entire 146k-348k MCP input totals. Per-turn input
grows only a few hundred tokens across tool calls (A-scored-1: 40,021 ->
40,021 -> 40,266 -> 40,456 -> 42,940), confirming tool results are not the
cost.

Measurement against the live server (homebrew aptu-coder 0.34.1, the build the
runner invokes): `tools/list` returns 7 tools totaling 27,610 bytes (~6.9k
tokens: analyze_symbol 7,153 B, analyze_file 6,616 B, analyze_directory 3,117
B, analyze_module 2,709 B, exec_command 3,595 B, edit_replace 3,225 B,
edit_overwrite 1,195 B) plus ~223 tokens of server instructions (~892 chars).
So the server directly contributes ~7.1k tokens of static context -- including
~2k tokens of edit/exec tool schemas that v17 conditions disallow but the
server unconditionally ships. The remainder of the measured ~15-18k MCP-vs-
native first-turn delta is client-side (Claude Code's MCP integration), not
aptu-coder payload.

**Correction of an earlier draft claim**: "static MCP context tripled from
~14k to ~40k tokens/turn" is wrong. What the data supports is that the
**MCP-vs-native per-turn premium** grew ~3x between v12 and v17 while native
per-turn context also roughly doubled -- i.e., much of the absolute growth is
client-side and affects both arms:

| per-turn input (mean over run) | v12 | v17 |
|---|---|---|
| Sonnet native (B) | 10,547 / 12,429 | ~16,018 |
| Sonnet MCP (A) | 17,424 / 16,722 (runs 3-4: 13,942 / 15,792) | 32,737 / 38,326 |
| Haiku native (D) | 26,888 / 24,254 | 26,463 / 25,900 |
| Haiku MCP (C) | 33,098-48,986 | 38,651 / 36,286 |

v12 Sonnet MCP premium: ~4-7k tokens/turn. v17 Sonnet MCP premium: ~17-22k
tokens/turn. Of the v17 premium, ~7.1k is directly attributable to server
payload (above); the attribution of the remaining ~10-15k between server
schema growth since v12 and client-side MCP context is unresolved (v12-era
schema sizes were not archived).

Two further facts reweight the cost story:
- The runner sets `DISABLE_PROMPT_CACHING=1` (scripts/bench-v17-run.sh), so
  the per-turn re-billing of static context is a deliberate harness choice
  applied to both arms, not a server or client defect.
- Whether v12 made the same choice is not recorded in its methodology; if
  v12 ran with caching enabled, part of v12's "59% cheaper" would itself be a
  caching artifact. v12 telemetry shows cache_read_tokens: 0, consistent with
  caching being disabled there too, so this is likely symmetric.

### H2. Sandbox/path failure -- CONFIRMED, and universal (not C-scored-1-specific)

Every `analyze_directory` invocation across all A/C sessions, verbatim:

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

There is **no path-formation difference that produced success**: C-scored-2
and C-pilot-1 did not reach the repo either. The scores.json note attributing
C-scored-1's failure to "model-path-choice variance" is factually wrong about
the other runs. The only inter-run variance was post-failure behavior: Sonnet
and C-scored-2 reconstructed answers from prior knowledge; C-scored-1 gave up
and emitted an empty `auth_module_map`.

Mechanism: `crates/aptu-coder/src/validation.rs` `validate_path()`
(line ~102) takes `cwd = std::env::current_dir()` as the allowed root and
rejects anything that canonicalizes outside it. The MCP config stub
(`mcp-aptu-coder-only.json`, byte-identical to v12's:
`{"mcpServers":{"aptu-coder":{"type":"stdio","command":"aptu-coder","args":[]}}}`)
has no `cwd`, so the server inherited the benchmark-runner repo root, while the
prompt substitutes `TARGET_REPO_PATH=/tmp/benchmark-repos/django` -- outside
the sandbox by construction.

Classification:
1. **Harness/prompt defect (primary)**: prompt target is outside the server's
   working directory; a correctly behaving server must reject it.
2. **Server usability defect (contributing)**: the error
   `path is outside the working directory` never states what the working
   directory is (the `error_meta` suggestion
   `provide a path within the current working directory` exists but omits the
   root), so every model burned calls guessing (`/Users/hugues.clouatre`,
   `../django`, `/django/...`).
3. **Model variance (consequence only)**: explains 9/9 vs 6 vs 1, not the
   failure itself.

### H3. Tool-selection collapse -- CONFIRMED as observation; cause is H2, so server-side summary-mode effect is UNRESOLVED

Counts: `analyze_file` 0, `analyze_module` 0, `analyze_symbol` 0 in all six
A/C sessions; only `analyze_directory` (3-6 calls) plus a final
`StructuredOutput`. The prompts' recipes (condition-a/c lines 14-19) are
internally coherent post-drift-corrections: step 2's `analyze_file` on "2-3
key files identified above" depends on step 1's directory overview, which
always failed -- the recipe never got past step 1 in any run. Git history
between v12 and v17 (`bfe0e96` tree output by default + 5K auto-summary
threshold, `bd3229e` default max_depth=3, `e67c792`/`5800b2b` server-owned
paging) plausibly made `analyze_directory` output richer, but v17 data cannot
test whether that changes drill-down behavior because no session ever saw a
successful target-repo analysis.

### H4. v12 comparison integrity -- PARTIALLY CONFIRMED (headline cherry-picked; underlying effect real)

The 59% claim lives in README.md ("cutting token usage by up to 59%"; MCP
Sonnet 112k/$0.39 vs Native 276k/$0.95), computed as input tokens, median,
with the MCP side using only A runs 3-4 (the improved-build re-run) against
the full n=2 native B condition. Recomputed aggregation matrix (A vs B):

| Aggregation | A | B | Reduction |
|---|---:|---:|---:|
| input, median, A runs 3-4 (README basis) | 111,966 | 276,291 | 59.5% |
| total tokens, median, all 4 A runs | 134,489 | 284,025 | 52.6% |
| input, mean, all 4 A runs | 128,633 | 276,291 | 53.4% |
| input, median, first 2 A runs | 145,300 | 276,291 | 47.4% |

So the headline is inflated by ~7 points via best-subset selection, but the
v12 effect is robust at 47-59% under every aggregation. The v17 Sonnet
collapse to -3% (MCP slightly worse) therefore survives conservative
re-aggregation. However, given the H2 finding, v12's MCP arm validity is now
itself in question: v12 preserved no session transcripts (all `.log` files are
0 bytes; A-scored-3 has none), so we cannot verify that v12 MCP runs ever
accessed Django either. If they did not, v12's "cheaper AND equal quality"
could be the same prior-knowledge artifact at lower per-turn cost (v12 MCP ran
~13.9k tokens/turn vs v17's ~40k).

### H5. Wall-time / API-time -- PARTIALLY CONFIRMED (benign accounting, real latency effect from static context)

- api_time ~= wall_time holds for all A/C runs (ratio 1.006-1.027). A-scored-2
  api 137,949 ms > wall 137,508 ms is a 441 ms (0.32%) excess consistent with
  timer ordering/rounding, not a defect; the same ratio pattern appears in
  every A/C run.
- Native runs B-scored-1, D-scored-1, D-scored-2 have 25-39% of wall time
  outside api_time (client-side Bash/Read execution) -- native wall times
  overstate API cost.
- Large MCP tool results do NOT inflate latency (results are <=7 KB and
  per-turn input barely grows). The uncached ~40k-token base context plausibly
  does: A runs ~17-20 s/turn vs B ~6.9-8.5 s/turn, with A-scored-2 showing
  18-40 s single turns re-processing ~35-43k input tokens at zero cache.

## Ranked defects and non-defects

1. **Harness defect (v17, likely inherited from v12)**: MCP server launched
   without `cwd`, so its sandbox root is the runner repo while the prompt
   targets `/tmp/benchmark-repos/django`. Invalidates the entire MCP arm.
2. **Server cost contributor**: the MCP-vs-native per-turn input premium grew
   from ~4-7k (v12) to ~17-22k tokens (v17, Sonnet). Of the v17 premium,
   ~7.1k tokens/turn is directly measured server payload (7-tool tools/list =
   27,610 bytes + 892-char instructions), including ~2k tokens of edit/exec
   schemas shipped unconditionally even when disallowed by the client
   allowlist. The rest of the premium's growth is unresolved between server
   schema growth since v12 and client-side MCP context. With caching disabled
   by the harness (DISABLE_PROMPT_CACHING=1), this premium is re-billed every
   turn.
3. **Server usability defect**: `path is outside the working directory` error
   omits the actual working-directory root; models cannot self-correct and
   burn calls guessing path forms.
4. **README aggregation defect (v12)**: "up to 59%" compares best-subset MCP
   re-runs against the full native condition; honest all-runs figure is ~53%.
5. **scores.json misdiagnosis**: C-scored-1 attributed to "model-path-choice
   variance" when the path failure was universal.
6. **Non-defects**: analyze_directory result size (tiny, well-bounded);
   api_time > wall_time in A-scored-2 (0.3%, benign); the condition prompts'
   recipes (coherent, never reached step 2); native-condition wall-time
   inflation is client-side tool execution, not API.

## Attribution of the observed MCP disadvantage

- **(a) aptu-coder server behavior**: the MCP-vs-native per-turn premium
  (~17-22k tokens on Sonnet, of which ~7.1k directly measured server payload,
  ~2k of it disallowed-tools schemas) is a genuine server-side cost
  contributor and the only mechanism that made MCP runs token-competitive-or-
  worse despite doing strictly less work (never reading Django). Est.
  contribution to the Sonnet A-vs-B gap: material, though the majority of the
  premium's growth since v12 is unresolved between server and client. The
  error-message UX defect wasted 2-5 calls per session but is second-order
  for tokens.
- **(b) Prompt/recipe/harness design**: the sandbox/cwd misconfiguration is
  the reason the MCP arm measured nothing about MCP. It does not explain the
  token gap per se (failed calls are cheap) but it invalidates what the gap
  means.
- **(c) Model variance at n=2**: explains quality spread (9 vs 6 vs 1 in C)
  and nothing about the token anomaly. C-scored-1's 347,858 input tokens are
  9 turns x ~40k static context, not 5 tool calls of payload.
- **(d) v12 baseline inflation**: ~7 points of the 59% headline is
  best-subset aggregation; the remaining ~53% v12 advantage still does not
  reproduce in v17, but v12's MCP arm is unverifiable (0-byte logs) and may
  share the same harness flaw, in which case v12's "advantage" was partly the
  same prior-knowledge artifact at one-third the per-turn overhead.

## Root-cause fix validation (2026-09-15, live test against aptu-coder 0.34.1)

Tested via direct MCP stdio (initialize / tools/list / tools/call):

1. **Reproduction** -- server cwd = aptu-coder repo, call
   `analyze_directory(path="/tmp/benchmark-repos/django/django/contrib/auth")`:
   returns `isError: true`, text `path is outside the working directory`,
   `structuredContent.suggestedAction` = `"provide a path within the current
   working directory"`. The payload nowhere states what the working directory
   actually is -- confirming the usability defect verbatim.
2. **Fix** -- server cwd = `/tmp/benchmark-repos/django`, call
   `analyze_directory(path="django/contrib/auth", max_depth=1, summary=true)`:
   full tree returns, e.g. `SUMMARY: 19 files (19 prod, 0 test), 4839L, 384F,
   76C (max_depth=1)` including `base_user.py [164L, 27F, 3C]` and
   `models.py [634L, 67F, 14C]`. The absolute TARGET_REPO_PATH form also
   succeeds from this cwd because validation.rs canonicalizes it inside the
   allowed root.

Conclusion: launching the CLI (and therefore the stdio MCP server) with
working directory = Django checkout fully resolves the MCP-arm failure. The
runner fix (run `claude` from `$DJANGO_REPO`) applies to all four conditions
equally, preserving the 2x2 comparison.

## Follow-ups executed in PR 1562 (2026-09-15)

- Runner: CLI launched with cwd = Django checkout (correction 8); pre-flight
  MCP gate added (correction 10); `DISABLE_PROMPT_CACHING=1` removed
  (correction 9). Validated live against aptu-coder 0.34.1 (see fix-test
  section above).

## Recommended follow-ups (not executed)

- Fix the harness: launch the MCP server with `cwd` = Django checkout (or make
  the prompt path relative to the server's working directory). Requires a new
  commit on a new branch; re-freezes the pending benchmark.
- Server: include the canonical working-directory root in
  `path is outside the working directory` errors (validation.rs already
  carries an `error_meta` suggestion; add the root to it).
- Server: investigate the v12->v17 growth of the static MCP context
  (tool descriptions, server instructions) and why prompt caching is not
  engaged (cache_read_tokens: 0 in every run of both benchmarks).
- Re-run v12 MCP arm with transcripts preserved to determine whether v12's
  MCP runs ever accessed the target repo.
