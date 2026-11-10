# Usage-Data Streamlining Audit (2026-11-10)

Cross-client, data-driven audit of the aptu-coder MCP tool surface. All figures come from
validated local telemetry and session logs on a single development machine, aggregated by
independent analysis passes, then cross-checked. Machine-specific paths are omitted; this is
a public repository.

## Sources and methodology

| Source | Window | Volume |
|---|---|---|
| aptu-coder JSONL telemetry (`~/.local/share/aptu-coder/metrics-*.jsonl`) | 2026-11-01 to 2026-11-10 | 11,286 events, 452 sessions |
| pi coding-agent session JSONLs (`~/.pi/agent/sessions/`) | ~2026-10-21 to 2026-11-11 | 1,869 session files, machine-wide |
| Claude Code session JSONLs (`~/.claude/projects/`) | all time | 505 files, machine-wide |
| Vibe session logs (`~/.vibe/logs/session/`) | recent | 17 parseable message files |

Aggregation was performed by four independent read-only analysis passes (one per source), on
`main` at `a448943`. No figure below is inferred from training data. Known caveats are stated
inline.

## 1. Validated usage and reliability data

### 1.1 aptu-coder tool share and server-side error rates (Nov 1-10 telemetry)

| Tool | Share of calls | Server-side error rate | Dominant `error_subtype` |
|---|---|---|---|
| exec_command | 62.3% (7,023) | 0.9% (64) | timeout 37, heredoc_error 22, invalid_params 27 |
| edit_replace | 18.5% (2,088) | 3.7% (77) | old_text_not_found 36, overlapping_edits 9, stale_content_hash 4 |
| edit_overwrite | 12.6% (1,425) | 0% | n/a |
| analyze_file | 1.5% (170) | 9.4% (16) | not_found 17-class, path_not_found 15 |
| analyze_module | 0.27% (30) | 10% (3) | invalid_params |
| analyze_directory | 0.21% (24) | 4% (1) | invalid_params |
| verify_anchors | 0 events | n/a | n/a |

Full error vocabulary: `ok` 5,720, `received` (exec streaming success) 5,405, `error` 161.
123 events carry `error_subtype`; 38 carry `error_type` only (open item from the
2026-10-30 error-taxonomy audit). `edit_overwrite` had zero failures in 1,425 calls.

Latency (p50 / p95, ms): edit_replace 1/2, edit_overwrite 1/1, analyze_module 23/43,
analyze_file 39/435, analyze_directory 30/417, exec_command 36/2,846. Edit tools are
effectively free; analysis tools are sub-half-second at p95.

Output volume: exec_command emitted 12.5M chars total (p95 8,954); `schema_surface` events
(526 calls, uniform 11,262 chars each) account for 5.9M chars of logged emission.

### 1.2 Client-side error rates from pi sessions (3 weeks, machine-wide)

| Tool | Calls | Errors | Rate |
|---|---|---|---|
| bash (native) | 31,671 | 1,779 | 5.6% |
| aptu-coder exec_command | 11,182 | 757 | 6.8% |
| aptu-coder edit_replace | 3,135 | 215 | 6.9% (2.5% within the aptu-coder repo) |
| edit (pi native) | 2,969 | 287 | **9.7%** (9.1% within the aptu-coder repo) |
| aptu-coder edit_overwrite | 1,892 | 71 | 3.8% |
| aptu-coder analyze_file | 331 | 60 | **18.1%** |
| aptu-coder analyze_module | 85 | 13 | 15.3% |
| aptu-coder analyze_symbol | 27 | 14 | **51.9%** |

Key factual finding: **the complaint that pi-native edit tools are reliable while
aptu-coder edit tools are not is inverted by the data.** Native `edit` fails at 9.7%
machine-wide vs 6.9% for `edit_replace`; inside this repository 9.1% vs 2.5%. Both fail
for the same dominant reason: exact-match `oldText`/`old_text` not found (~80% of all edit
failures across both tools). Zero `expected_content_hash` staleness failures and zero
`ambiguous` edit failures were observed anywhere.

Schema-shape footgun: 68 `edit_replace`/`edit_overwrite` failures came from the client
sending camelCase (`newText`) instead of snake_case (`new_text`), a confusion induced by
the adjacent native-tool schema.

### 1.3 Claude Code and Vibe comparison

- Claude Code native `Edit`: **222/222 first-shot success (0% failures)**, machine-wide.
  Note: this Claude Code version emits no MultiEdit and applies smaller, atomic edits.
- Claude Code calling aptu-coder edit tools: 348 calls, 22 failures (6.3% first-shot),
  dominated by JSON `InputValidationError` (5) and the hash/ambiguity guards (4).
- Claude Code calling aptu-coder `exec_command`: 2,179 of 2,987 results flagged as errors;
  this counts non-zero exit codes as errors (bash-equivalent baseline in the same logs is
  384/12,989 = 3.0%), so the flagging semantics differ between the two transports and the
  number is not comparable to pi's 6.8%. Treated as unvalidated.
- Vibe: no explicit edit failures in 174 edit attempts across 17 parseable sessions
  (small n; failed runs may be filtered from its logs).

### 1.4 Retry patterns

Repeated blind retries on the same failing edit are visible in all three clients
(consecutive identical "could not find" failures on the same file; 10-14 edit_replace calls
on single hot files within the window). No client-side retry backoff or guidance consumption
is observable from the logs.

## 2. Cross-checks against prior audits

- **2026-09-30 v19 pivot (NOT-SUPPORTED):** structural analysis shows no F1 gain in agentic
  loops that have bash+rg (paired F1 diff -0.071, N=20). The present telemetry is
  consistent: `analyze_symbol` and `verify_anchors`, the pivot's recommended surface, show
  near-zero and zero adoption respectively.
- **2026-11-01 agent-optimization audit:** V1-V5 cache/perf fixes landed (#1783, #1786) and
  were validated by 2026-11-02 perf validation (analyze_module -8.1%, file_details -5.7%,
  graph-query paths -3-4%). No regression.
- **2026-10-30 error taxonomy:** T1-T3 fixes landed (#1766, #1767). Residual gap: 38 of 161
  error events still lack `error_subtype`.
- **KG pull-ablation benchmark** was designed (2026-08-29) and never executed. KG resource
  reads: 12/month (October telemetry); `read_resource` events in the Nov window are absent.

## 3. Recommendations (KISS, evidence-ranked)

Each recommendation states its evidence and decision-gate outcome (Jev typed-judgment,
model `jev-1.14.0`). None of these are implemented in this PR; each should be its own
atomic PR.

### R1. Remove `verify_anchors` (~168 LOC) — do it
Zero events in 10 days of telemetry and 11,286-event sample; the v19 program that
motivated it is closed NOT-SUPPORTED; dead tools still cost schema bytes on every session
bootstrap. Decision gate: remove, confidence 0.94. Keep the golden-test lessons in git
history.

### R2. Cut `analyze_symbol` (~2,120 LOC with dispatch) — do it, staged
Both under-utilized and not useful: the v19 crossover benchmark measured its core premise
directly — no F1 gain in agentic loops that have bash+rg (paired F1 diff -0.071, N=20) —
and clients discovered this empirically. Usage collapsed from 638 calls/month (October)
to 27 calls in 3 weeks of pi sessions (52% errors, "symbol not defined in directory" class)
and 0 events in the 10-day server telemetry. Not useful as positioned, hence under-utilized.
Decision gate: cut, confidence 0.68, with "slim" as fallback. Staging: deprecate
in the schema description first (one release), remove next release, watch telemetry for
objections. If any consumer objects, fall back to keeping only the exact-match, no-pagination
mode (~one third of the LOC).

### R3. Gate KG resource surface (~3,100 LOC) behind the ablation benchmark it was designed for — decide by data
12 reads/month, benchmark never executed, v19 says structural analysis does not help
agentic loops. Decision gate: run the 3-arm ablation first (confidence 0.54 for
gate-then-cut vs 0.45 for immediate cut). KISS path: schedule the already-designed
benchmark on throwaway draft PRs; cut the surface if it confirms no value. Immediate cut is
defensible but the gate was pre-registered; honor it.

### R4. Improve `edit_replace`/`edit_overwrite` error text for wrong-cased fields — do it
68 failures are pure camelCase/snake_case confusion. Decision gate: better error message,
confidence 0.78 (accepting aliases violates the strict-param policy; keep strict). Change:
when validation rejects an unknown field, the error must name the expected snake_case
fields and echo the offending key. This is the highest-leverage accuracy fix available
(~40% of observed edit-tool failures in pi logs).

### R5. Close the `error_subtype` gap — do it
38 of 161 error events carry `error_type` only. The 2026-11-01 telemetry-validation-gate
window opened 2026-11-01; publish its verdict with this audit's data as the baseline and
make `error_subtype` non-null on all error paths.

### R6. Keep the historical bench scripts — do not delete
Decision gate: keep, confidence 0.75. They document methodology lineage (v12-v19) referenced
by audit docs; deletion saves nothing measurable and breaks reproducibility of prior
audits' comparisons.

### R7. Do not add features to chase Claude Code's 0% native-Edit rate — record and stop
Claude Code's native Edit succeeds 222/222, but only because its harness makes smaller,
atomic, freshly-read edits; pi's native edit on the same machine fails 9.7% of the time.
The dominant failure class (exact-match miss) is a model-behavior problem, not a tool bug;
`edit_overwrite` at 0/1,425 and `edit_replace` at 3.7% server-side are already at or below
native-tool baselines. Any further investment here should target the error text (R4), not
new matching heuristics (fuzzy match would trade determinism for the 36 not-found cases).

### R8. MCP 2026-07-28 alignment — verify, then plan
The 2026-07-28 spec (GA) makes the protocol core stateless, adds an extensions framework,
cacheable list results, and changes the missing-resource error from -32002 to standard
-32602; error codes -32020..-32099 are now spec-reserved. The repo pins `rmcp` 3.9.0 /
`schemars` 1.2.3. Action: run a conformance pass against the 2026-07-28 changelog (error
codes, list-result cacheability, extensions header) and file follow-up issues per gap. The
existing 2026-08-01 audit predates GA; mark it superseded once the conformance pass lands.

## 4. What the data says we should keep as-is

- `exec_command`: 62.3% of traffic, 0.9% server-side error rate, sub-3s p95. Its
  heredoc rejections (22) are by-design; consider only an error-message pointer to
  `stdin`-parameter usage, no behavior change.
- `edit_overwrite`: 1,425 calls, zero failures. The simplest tool is the most reliable;
  keep it untouched.
- `analyze_file`/`analyze_directory`/`analyze_module`: low volume but cheap (261-1,082 LOC
  total for module+directory), post-fix error rates trending down per the open
  validation gate. Keep; re-evaluate at the next 30-day window.
- Opaque-cursor pagination and cache-key consolidation (#1783/#1786/#1792): validated by
  criterion benches; no further churn.

## 5. Follow-up work items (atomic, tracked as issues)

1. Remove `verify_anchors` (R1) + update README/docs/`docs-output-contract-check.sh`.
   Tracked as [#1806](https://github.com/clouatre-labs/aptu-coder/issues/1806).
2. Remove `analyze_symbol` (R2). Tracked as
   [#1807](https://github.com/clouatre-labs/aptu-coder/issues/1807).
3. Edit-tool validation errors: name expected snake_case fields; make `error_subtype`
   non-null on all error paths (R4 + R5). Tracked as
   [#1808](https://github.com/clouatre-labs/aptu-coder/issues/1808).
4. Execute the KG ablation benchmark; cut KG resources if NOT-SUPPORTED (R3). Tracked as
   [#1809](https://github.com/clouatre-labs/aptu-coder/issues/1809).
5. MCP 2026-07-28 conformance pass (R8). Tracked as
   [#1810](https://github.com/clouatre-labs/aptu-coder/issues/1810).

*Table 1: aptu-coder tool share and server-side error rates (section 1.1).*
*Table 2: client-side error rates from pi sessions (section 1.2).*
