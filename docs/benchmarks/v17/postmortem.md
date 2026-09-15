# v17 Post-mortem: Investigation of the Discarded Benchmark Execution

Analysis of the artifacts preserved at commit `663467f` on `bench/v17-results`, conducted read-only. The v17 execution remains discarded as a measurement; this document treats the artifacts as observational data for diagnosis only.

## What the results support, and what they do not

The results support exactly one conclusion: **the experiment was completely broken, and no comparison drawn from it is valid.** No MCP run ever accessed the target repository, so the MCP conditions measured prior knowledge and error recovery rather than MCP tool use. Nothing in this dataset constitutes evidence about MCP tool performance, cost, latency, or quality relative to native tools — in either direction. Every number below is reported as diagnostic evidence about the *harness*, or as neutral engineering observation about the server, never as a performance comparison.

## Headline finding

**No MCP run in any condition ever read a single file of the Django checkout.** Across all six A/C sessions, every absolute attempt at the Django path failed with the exact error `path is outside the working directory`, and every relative attempt failed with `path not found`. The only successful `analyze_directory` calls listed files of the aptu-coder repository itself, for example `./CONTRIBUTING.md`, `./Cargo.lock`, and `./crates`. All A/C scores, including Sonnet's 9/9, were produced from the model's prior knowledge of Django's contrib.auth plus error recovery. The scores.json note attributing C-scored-1's failure to "model-path-choice variance" is incorrect: the path failure was universal. The only inter-run variation was post-failure behavior — Sonnet and C-scored-2 reconstructed plausible answers from memory, while C-scored-1 gave up and emitted an empty `auth_module_map`.

Root cause: the MCP config stub (`mcp-aptu-coder-only.json`, byte-identical to v12's) specifies no `cwd`, so the stdio server inherited the benchmark-runner repo root as its sandbox. `validate_path()` in `crates/aptu-coder/src/validation.rs` canonicalizes the server's `std::env::current_dir()` as the allowed root and rejects anything outside it. The prompt, meanwhile, substitutes `TARGET_REPO_PATH=/tmp/benchmark-repos/django` — outside that root by construction. A correctly behaving server *must* reject these calls; the harness pointed the agent at a path the server could never accept.

## Diagnostic findings

### D1: The MCP sessions' tool-result payloads were small

Tool-result sizes were measured by pairing `tool_use` blocks with `toolUseResult` entries by `tool_use_id`. The MCP sessions' tool results total 3 to 10 KB per session, the largest single result being 6,927 bytes (a per-file index with `cache_tier`, `class_count`, `function_count`, `line_count`, and `path` fields). The recurring 46-byte results are the error string `Error: path is outside the working directory`. Native sessions, for reference, carried 33 to 62 KB of results. The sessions' high telemetry input-token counts are explained by static context re-billed per turn (the harness set `DISABLE_PROMPT_CACHING=1`), not by tool-result size: per-turn input grew only a few hundred tokens across tool calls (A-scored-1: 40,021 → 40,021 → 40,266 → 40,456 → 42,940). This rules out any hypothesis that `analyze_directory` payloads inflated MCP token usage.

A neutral engineering observation from the live server (homebrew aptu-coder 0.34.1, the build the runner invokes): `tools/list` returns 7 tools totaling 27,610 bytes (~6.9k tokens) plus roughly 223 tokens of server instructions, so the server contributes about 7.1k tokens of static context per session — including ~2k tokens of edit/exec schemas that v17 conditions disallowed but the server ships unconditionally. This is a fixed cost independent of task behavior and is recorded here as an optimization lead, not as a measured performance effect.

### D2: Complete inventory of aptu-coder tool calls (evidence for the headline finding)

The only aptu-coder tool ever invoked in any A/C session was `analyze_directory` (plus the CLI's `StructuredOutput` to finish):

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

There is no path-formation difference between failing and succeeding runs: every run failed against the target. The server's behavior was correct in every case given its working directory. Two usability observations follow:

- **Harness/prompt defect (primary):** the prompt target was outside the server's working directory; rejection was the correct response.
- **Server usability defect (contributing, error UX only):** the error text `path is outside the working directory` never states what the working directory actually is, and the structured `suggestedAction` (`provide a path within the current working directory`) also omits the root. Models therefore wasted calls guessing path forms (`/Users/hugues.clouatre`, `../django`, `/django/...`). This is about error-message quality, not about validation behavior, which was correct.

### D3: Tool-selection collapse is fully explained by the harness defect

`analyze_file`, `analyze_module`, and `analyze_symbol` were called zero times in all six A/C sessions. The condition prompts' recipes are internally coherent after the drift corrections: step 2's `analyze_file` on "2-3 key files identified above" depends on step 1's directory overview — which always failed. The recipe never advanced past step 1 in any run. Whether richer `analyze_directory` output (changed between v12 and v17 by `bfe0e96`, `bd3229e`, `e67c792`/`5800b2b`) would alter drill-down behavior is untestable from this dataset.

### D4: Telemetry accounting is sound

Across all 12 runs, `api_time` ≈ `wall_time` holds for all six A/C runs (ratio 1.006–1.027); A-scored-2's api 137,949 ms exceeding wall 137,508 ms is a 441 ms (0.32%) excess consistent with timer ordering, not a defect. Native runs B-scored-1, D-scored-1, and D-scored-2 show 25–39% of wall time outside api time (client-side Bash/Read execution), which is expected and benign. The telemetry pipeline itself did not mislead us; the harness configuration did.

### D5: The v12 baseline's own integrity is now in question

Separately from v17, the v12 headline claim was audited. The "59%" in the README ("cutting token usage by up to 59%"; MCP Sonnet 112k tokens / $0.39 vs native 276k / $0.95) is computed as median input tokens with the MCP side using only A runs 3–4 (the improved-build re-run) against the full n=2 native condition:

| Aggregation | A | B | MCP reduction |
|---|---:|---:|---:|
| input, median, A runs 3–4 (README basis) | 111,966 | 276,291 | 59.5% |
| total tokens, median, all 4 A runs | 134,489 | 284,025 | 52.6% |
| input, mean, all 4 A runs | 128,633 | 276,291 | 53.4% |
| input, median, first 2 A runs | 145,300 | 276,291 | 47.4% |

The v12 effect is robust at 47–59% under every aggregation, so the underlying v12 result was not fabricated — but the headline is inflated ~7 points by best-subset selection. More importantly, v12 preserved no session transcripts (all `.log` files are zero bytes; A-scored-3 has none), so it cannot be verified that v12's MCP runs ever accessed Django either. If they did not, v12's headline shares the same validity problem as v17. This is an open question, not a finding.

## Root-cause fix validation (2026-09-15, live test against aptu-coder 0.34.1)

Tested via direct MCP stdio (initialize / tools/list / tools/call):

1. **Reproduction** — server cwd = aptu-coder repo, `analyze_directory(path="/tmp/benchmark-repos/django/django/contrib/auth")` returns `isError: true`, text `path is outside the working directory`, `suggestedAction` = `provide a path within the current working directory`. The payload nowhere states the working directory.
2. **Fix** — server cwd = `/tmp/benchmark-repos/django`, `analyze_directory(path="django/contrib/auth", max_depth=1, summary=true)` returns the full tree: `SUMMARY: 19 files (19 prod, 0 test), 4839L, 384F, 76C (max_depth=1)` including `base_user.py [164L, 27F, 3C]` and `models.py [634L, 67F, 14C]`. The absolute `TARGET_REPO_PATH` form also succeeds from this cwd, because the path canonicalizes inside the allowed root.

Conclusion: launching the CLI (and therefore the stdio MCP server) with working directory = Django checkout fully resolves the failure. The runner now does this for all four conditions equally. The server required no changes to pass this test.

## Defects, ranked

1. **Harness defect (v17, possibly inherited from v12):** MCP server launched without `cwd`, so its sandbox root was the runner repo while the prompt targeted `/tmp/benchmark-repos/django`. Invalidated the entire MCP arm.
2. **scores.json misdiagnosis:** C-scored-1 attributed to "model-path-choice variance" when the path failure was universal.
3. **Server usability defect (error UX only):** `path is outside the working directory` omits the actual working-directory root; models cannot self-correct and waste calls guessing. Validation behavior itself was correct.
4. **README aggregation defect (v12):** "up to 59%" compares best-subset MCP re-runs against the full native condition; the honest all-runs figure is ~53%.
5. **Harness cost-policy defect:** `DISABLE_PROMPT_CACHING=1` (a v9-era Bedrock workaround) re-billed full static context every turn for both arms; removed.

Non-defects: the server's path validation (correct in every observed call); `analyze_directory` result size (tiny and well-bounded); api_time exceeding wall_time in A-scored-2 (0.3%, timer ordering); the condition prompts' recipes (coherent, simply never reached step 2); native wall-time inflation (client-side tool execution, not API); the telemetry pipeline.

## Attribution

The observed v17 anomaly — MCP token/cost parity or worse alongside mixed quality — is attributable entirely to the harness defect plus model variance in prior-knowledge recovery at n=2. The MCP arm never performed the task, so nothing in v17 is attributable to MCP tooling, favorably or unfavorably. The server contributed two secondary items: an uninformative error message (wasted calls, quality impact only through recovery paths) and a fixed ~7.1k-token static-context cost that is an optimization lead, not a measured performance effect. The v12 baseline remains favorable to MCP under every aggregation tested, but its MCP arm's validity is unverifiable and should be treated as open until a transcript-preserved re-run says otherwise.

## Follow-ups executed in PR 1562 (2026-09-15)

- Runner: CLI launched with cwd = Django checkout (methodology correction 8); pre-flight MCP gate added (correction 10); `DISABLE_PROMPT_CACHING=1` removed (correction 9). Validated live against aptu-coder 0.34.1. The server required no changes.

## Recommended follow-ups (not executed)

- Server: include the canonical working-directory root in the `path is outside the working directory` error (validation.rs already carries an `error_meta` suggestion; add the root to it).
- Server: optimization lead — audit the static context (27,610-byte `tools/list` across 7 tools) and whether disallowed tools' schemas can be withheld.
- Benchmark design: use a target repository the models do not know. Sonnet's 9/9 from zero target reads shows prior knowledge can fully mask tool-access failure on a famous codebase; consider scorer verification that cited `file:line` anchors exist in the checkout.
- Re-run the v12 MCP arm with transcripts preserved to determine whether v12's MCP runs ever accessed the target repo.
