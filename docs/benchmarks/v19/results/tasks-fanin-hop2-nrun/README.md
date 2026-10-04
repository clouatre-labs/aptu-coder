# tasks-fanin-hop2-nrun — HALTED before spend (2026-10-03)

Status: **halted before any LLM spend.** The A11 ratified design
(n=23 pairs per arm, same frozen A4 selection procedure) cannot be
constructed on the pinned Django snapshot (`dd6f6b1`, tarball sha256
`9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`,
re-verified against `bench_v19.snapshots.SNAPSHOT_MANIFEST` before this
probe).

## Finding: pool exhaustion

The A4a sealed-tier filters (hop-2 caller-file window `[20, 60]`,
`rg_output_bytes >= 20,000`, unambiguous single Python function
definition, hop-3 gate dropped per A4a, ordering by descending hop-1
caller-file count) yield **exactly 8 viable symbols on the entire
snapshot**. An exhaustive probe over every callee in the call-edge
index (3,810 fail-closed exclusions recorded in
`selections-a11.json.exhaustion-probe.json`) returns the same 8 symbols
as the frozen `selections-a4.json`, in the same order, with
byte-identical gating fields (hop-1 counts, hop-2 counts, hop-2
expected file sets):

`register_lookup` (43), `skipIfDBFeature` (41), `include` (33),
`chain` (56), `view_func` (36), `qualname` (22), `dec` (23),
`timezone` (20).

The A11 requirement that the 23-task set be a superset of the frozen
8 is therefore satisfiable only trivially (23 > 8 is unreachable);
n=23 requires widening the ratified A4a window or floor, which the
A11 text and this run's binding specification both forbid ("same
frozen selection procedure (A4)"). Per the spec's halt rule (spec
violation before spend), the sealed stage was not started.

## Zero-spend verification performed (per spec preamble)

- `aptu-coder` binary: 0.39.0 (matches workspace `Cargo.toml` at HEAD
  `12c2324`; the stale `pilot.py` `APTU_CODER_VERSION` constant was
  corrected 0.38.0 -> 0.39.0 per the A9b caveat-2 precedent).
- Snapshot tarball re-fetched and hash-verified against the pinned
  manifest. The prior sessions-cwd checkout (`/tmp/v19-wiring/django`)
  had zero content diffs against a fresh extraction of the verified
  tarball (only dotfiles missing from its original extraction); the
  fresh extraction is the sessions cwd of record.
- Harness tests: `PYTHONPATH=scripts .venv-v19/bin/python -m pytest
  scripts/tests/ -q` green.
- Selection reproduction: `select_fanin_symbols(snapshot,
  max_symbols=8, min_hop1=0, min_hop3=None)` reproduces
  `selections-a4.json` exactly on all gating fields. The only drifting
  datum is `rg_output_bytes` (ripgrep 15.2.0; byte totals include the
  absolute snapshot path prefix, which differed in the original run).
  This datum is F2 justification metadata only and gates nothing here.

## Artifacts (zero-spend)

- `selections-a11.json` — the exhaustive pool (8 selections; NOT the
  A11 n=23 set; retained as probe evidence only).
- `tasks-track-a-hop2-a11.json` — tasks for the 8-symbol pool,
  byte-identical to the frozen `tasks-track-a-hop2-a4.json` on every
  field (verified programmatically).
- `selections-a11.json.exhaustion-probe.json` — fail-closed exclusion
  ledger of the exhaustive probe.

## Decision requested on #1681 / PR #1732

Either (a) re-derive n<=8 and amend A11's sample size with the pool
exhaustion as the recorded reason, (b) ratify a widened window (e.g.
hop-2 set in `[61, N]` or a lower rg floor) as a new amendment with
the resulting pool documented, or (c) accept the sealed 8-pair result
as final. No spend has occurred against the $0.95 stage allowance.

---

# A11b re-probe — HALTED again before spend (2026-10-03)

Status: **halted before any LLM spend.** The A11b widened window
(hop-2 caller-file set in `[10, 60]`, rg floor >= 20,000 bytes,
unambiguous-single-definition invariant, descending hop-1 caller-file
count ordering) was executed exactly as ratified; the resulting pool is
**12 viable symbols, fewer than the ratified n=23**. Per the binding
halt rule ("halt before spend if the widened pool yields fewer than 23
viable symbols"), the sealed stage was not started.

## Zero-spend verifications performed

- `aptu-coder` binary: the installed binary had drifted to 0.37.0; it
  was rebuilt from workspace HEAD `117f110` via
  `cargo install --path crates/aptu-coder --profile release` and now
  reports 0.39.0, matching the workspace `Cargo.toml` and the
  `pilot.py` constant.
- Harness tests: 173 passed (`PYTHONPATH=scripts .venv-v19/bin/python
  -m pytest scripts/tests/ -q`).
- Snapshot: tarball re-fetched and SHA256-verified
  (`9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`).
  The prior sessions cwd (`/tmp/v19-wiring/django`) is missing 7,087
  files from the tarball; a fresh extraction with zero content diffs
  against the verified tarball is the snapshot of record
  (`/tmp/v19-nrun/django-dd6f6b1...`).

## Finding 1: pool still exhausted (12 < 23)

The widened window admits 4 new symbols over the frozen 8:

`function` (hop1=7, hop2=10), `handler` (hop1=5, hop2=10),
`formset_factory` (hop1=4, hop2=15), `back` (hop1=1, hop2=13)

for a full viable pool of 12:

`register_lookup` (43), `skipIfDBFeature` (41), `include` (33),
`chain` (11), `view_func` (7), `function` (7), `qualname` (6),
`handler` (5), `formset_factory` (4), `dec` (2), `timezone` (1),
`back` (1) — hop-1 caller-file counts in parens; hop-2 counts 43, 42,
33, 56, 36, 10, 22, 10, 15, 23, 20, 13 respectively. 3,806 candidates
were fail-closed excluded (full ledger in the probe artifact).

## Finding 2: A11b's "frozen 8 remain the head" is falsified

Selection orders by descending hop-1 caller-file count. The newly
admitted symbols interleave ahead of frozen members: `function`
(hop1=7) slots ahead of `qualname` (hop1=6); `handler` (5) and
`formset_factory` (4) follow. Only the first 5 frozen symbols
(`register_lookup`, `skipIfDBFeature`, `include`, `chain`,
`view_func`) retain their positions. The only drifting gating-neutral
datum remains `rg_output_bytes` (absolute snapshot path prefix;
F2 justification metadata only).

## Artifacts (zero-spend)

- `selections-a11b-widened-pool-probe.json` — full widened-window pool
  (12 selections, 3,806 fail-closed exclusions), probe metadata, and
  the `frozen8_head_intact: false` flag.

## Decision requested on #1681

The A11b relaxation is insufficient: no n=23 set exists on this
snapshot under any window relaxation that preserves the rg floor,
since at most 12 symbols on the entire snapshot pass
rg-output >= 20,000 + unambiguity with a nonempty in-window hop-2 set.
Options: (a) re-derive n <= 12 and amend A11's sample size (post-hoc
power must then be re-evaluated at the pilot SD), (b) lower the rg
floor as a new amendment (this dilutes the grep-hostile property A11b
explicitly declined to dilute and needs fresh justification), or
(c) ratify the sealed 8-pair result as final. No spend has occurred
against the $0.95 stage allowance.

---

# A11c stratified N-run — HALTED before spend (2026-10-03)

Status: **halted before any LLM spend.** All zero-spend step-1
verifications were executed per the ratified execution order; one
failed: the Django 6.1.1 tarball SHA256 recomputed from a fresh fetch
does not match the full digest string in the run order. The order's
binding rule is "recompute and record; halt on mismatch," so neither
stratum was started.

## Zero-spend verifications performed

- `aptu-coder` binary: 0.39.0 (matches workspace `Cargo.toml` 0.39.0;
  no rebuild required).
- Harness tests: 173 passed
  (`PYTHONPATH=scripts .venv-v19/bin/python -m pytest scripts/tests/ -q`).
- Stratum 1 tarball (dd6f6b1): re-fetched, SHA256
  `9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`
  — matches the pinned manifest. A fresh extraction is byte-identical
  (`diff -rq`, exit 0, no output) to the extraction of record at
  `/tmp/v19-nrun/django-dd6f6b1531984823e3dc56740dfa93f3ceb09357`.
- Stratum 2 tarball (6.1.1): re-fetched twice from
  `https://github.com/django/django/archive/refs/tags/6.1.1.tar.gz`;
  the two fetches are byte-identical (`cmp` clean), so GitHub's tag
  archive digest is deterministic and the artifact is stable.
  Recomputed SHA256:
  `32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77604cdc48b84007d73`.

## The digest mismatch (halt cause)

Run-order expected:
`32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77704cdc48b84007d73`
Recomputed (twice, byte-stable):
`32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77604cdc48b84007d73`

The strings differ at exactly one hex nibble (`...e3b5b777|04...` vs
`...e3b5b776|04...`). The ratified A11c amendment text pins only the
prefix `32e24244...`, which the recomputed digest matches in full;
the full 64-hex-digit string appears nowhere in the repository. The
recomputed digest is the verified artifact digest; the run-order
string is consistent with a single-character transcription error in
the order itself. Resolution requires the order's owner to re-issue
the expected digest (or ratify the recomputed digest by amendment);
per the halt rule the run did not proceed on its own interpretation.

## 6.1.1 selection re-probe (zero-spend, evidence for the re-run)

The same selection probe
(`select_fanin_symbols`, window `[10,60]`, rg floor 20,000 bytes,
unambiguity, descending hop-1 order, pinned tree-sitter deps) was run
on a fresh extraction of the verified 6.1.1 tarball and **reproduces
exactly 8 viable symbols** in the ratified order with 3,777 fail-closed
exclusions:

`register_lookup` (hop1=43, hop2=43), `include` (32, 32),
`chain` (12, 57), `function` (7, 10), `formset_factory` (4, 15),
`dec` (2, 23), `timezone` (1, 20), `back` (1, 22).

This matches the A11c probe pool symbol-for-symbol and order-for-order.

## Spend

$0.00. No sessions were run against either stratum; the $0.71 stage
cap is untouched. No artifacts beyond this halt record were emitted;
no selections/tasks/manifest files were written for either stratum
(the per-stratum artifact set is only valid once the digest question
above is resolved and the snapshot manifest pin is ratified against
the final digest string).

---

# A11c stratified N-run — HALTED mid-Stratum-1 by the F3 activation gate (2026-10-04)

Status: **halted after 9 of 12 Stratum-1 pairs; Stratum 2 not started; $0.2984
of the $0.71 stage cap metered.** The ratified F3 gate (>= 0.80 of scorable
tool-arm sessions with >= 1 aptu-coder tool call, fail-closed) read
**0/8 = 0.0** and the ladder halted with `activation-gate-failed:0`.

## Pre-run verifications (all pass)

- `aptu-coder` binary: 0.39.0 (matches workspace `Cargo.toml`; no rebuild).
- Harness tests: 175 passed after the A11c pin was added
  (`PYTHONPATH=scripts .venv-v19/bin/python -m pytest scripts/tests/ -q`;
  two new assertions cover the `django-6.1.1` manifest entry).
- Stratum 1 tarball (dd6f6b1): SHA256 re-verified
  `9dc904f5...`; extraction of record byte-identical to a fresh
  extraction (`diff -rq` clean).
- Stratum 2 tarball (6.1.1): fresh fetch reproduces the ratified A11c
  digest
  `32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77604cdc48b84007d73`
  (three independent fetches byte-identical); extraction of record
  `/tmp/v19-nrun/x611/django-6.1.1` byte-identical to a fresh extraction.
- Stratum 2 selection re-probe on the extraction of record reproduces the
  ratified 8-symbol pool symbol-for-symbol and order-for-order
  (3,777 fail-closed exclusions): `register_lookup`, `include`, `chain`,
  `function`, `formset_factory`, `dec`, `timezone`, `back`.
- Snapshots.py manifest: `django-6.1.1` pin added (commit field carries
  the tag; `archive/6.1.1.tar.gz` verified byte-identical to
  `archive/refs/tags/6.1.1.tar.gz`).

## The halt cause: tool-surface drift, not tool non-use

The mcp arm's tool calls are present and genuine: session JSONL shows
`mcp` gateway tool calls with `{"tool": "aptu-coder_analyze_symbol",
"args": {"mode": "call_graph", "symbol": "register_lookup", ...}}` and
prior `mcp({ connect: "aptu-coder" })` setup. But they are recorded as
gateway `mcp` calls, not direct `aptu-coder_*` toolCall blocks, so
`runner_v19.count_aptu_tool_calls` (which counts `aptu-coder_*`-prefixed
blocks, per the ratified directTools assumption) correctly counts 0.

This is a drift in the unpinned runtime stack, not in the harness:
- prior sealed run (2026-09-29): pi 0.87.1, adapter floating; tool-arm
  sessions recorded 1-19 direct `aptu-coder_*` calls; gate PASS.
- this run (2026-10-04): pi 1.0.2, pi-mcp-adapter 5.0.0
  (`npm:pi-mcp-adapter` is unpinned in `runner.py`); the adapter surfaces
  aptu-coder behind its `mcp` gateway tool despite
  `directTools: true` in the generated `mcp.json`.

Per A5 gate discipline (a gate-relevant measurement defect halts spend),
the run stopped rather than silently reinterpreting the counter.
Repairing this requires a ratified amendment (e.g. pin the adapter
version and/or update the activation counter and directTools contract,
with a zero-spend tool-surface validation gate added pre-sealed); the
harness files were not edited.

## Run record (Stratum 1 partial, as-sealed)

- 9 pairs run in task-id order (back, chain, dec, formset_factory,
  function, handler, include, qualname, register_lookup); 18 sessions.
- Spend: $0.298385 metered. Tokens: 484,504 in / 271,074 out.
- Defect ledger: 1 entry — `task-fanin-chain`/`mcp`,
  `wait-timeout-killed:900s`, `killed=true`, cost metered (A7b
  visibility discipline held; no silent kills). No provider errors.
- Session-level notes: the machine entered macOS maintenance sleep mid
  `task-fanin-include`/native (00:50-02:03 local); `caffeinate` was
  started and the session completed normally with `stopReason: stop`,
  so no truncation resulted. The chain/mcp 900s kill predates the sleep
  window and is a genuine deadline kill.
- Native arm: 9/9 completed (`stopReason: stop`). MCP arm: 8/9 completed,
  1 deadline-killed. Gate: 0/8 active (drift, above).
- Per-pair F1 comparison is NOT reported: the mcp arm's tool surface
  differs from the ratified design (gateway tax vs directTools), so the
  cells are not the ratified treatment and any paired statistic would be
  invalid. The 18 sessions are retained verbatim under
  `stratum1-dd6f6b1/sealed/` as drift evidence.

## Spend

$0.2984 total (Stratum 1 partial). Stratum 2: 0 pairs, $0.00. The $0.71
stage cap was not reached; the halt is a design-halt (F3 fail-closed),
not a budget halt.

## Decision requested on #1681

(a) ratify an adapter-version pin + updated activation counter/tool-surface
contract as a new amendment, with a zero-spend pre-sealed validation, and
re-run both strata fresh (the 18 drift sessions remain as evidence, not
data); or (b) re-scope per the A11 re-scope clause. No further spend
until one is ratified.

---

# A12 ratified re-run — COMPLETE (2026-10-04), A9b curtailed by maintainer
# close-out directive

Status: **complete.** Both strata ran fresh under the A12 harness fix
(mcp-adapter.json + adapter pin 5.0.0), the F3 activation gate PASSED on
the corrected direct-tool surface in both strata, and the pre-registered
claim gate evaluates **NOT-SUPPORTED**. The A9b verify-repair pass was
curtailed mid-Stratum-2 by the maintainer close-out directive (no
further LLM spend); repairs already on disk are applied per the A9
replacement policy, the rest are recorded as not performed.

## Pre-run verifications (all pass, zero spend)

- `aptu-coder` binary: 0.39.0 (`/opt/homebrew/bin` and `~/.cargo/bin`),
  matches workspace `Cargo.toml`; no rebuild.
- Harness tests: 175 passed after the A12 filename change
  (`PYTHONPATH=scripts .venv-v19/bin/python -m pytest scripts/tests/ -q`).
- Harness fix applied exactly per A12: the three MCP arms write the
  directTools-bearing `mcpServers` payload to `mcp-adapter.json`
  (payload otherwise identical); the native arm keeps its empty
  `mcp.json`; shadow `settings.json` pins `npm:pi-mcp-adapter@5.0.0`.
  Tests updated for the new filename and the pin.
- Stratum 1 tarball re-verified: sha256
  `9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e`;
  extraction of record unchanged.
- Stratum 2 tarball re-verified: sha256
  `32e24244c151fb1e1257a4e550557f1c052a48a9e3b5b77604cdc48b84007d73`;
  extraction of record `/tmp/v19-nrun/x611/django-6.1.1`.
- Pool reproduction (re-probed, zero spend): `select_fanin_symbols`
  with the A11b window reproduces both strata symbol-for-symbol and
  order-for-order — stratum 1: 12 selections, stratum 2: 8 selections
  (`register_lookup, include, chain, function, formset_factory, dec,
  timezone, back`). Existing per-stratum selections/tasks/manifests
  re-verified against the reproduced pools and reused.

## A12 pre-sealed surface gates (zero spend, offline) — PASS

Before EACH stratum, the harness-built shadow `agent-mcp` config ran
`pi -p --mode json` against a mock endpoint (provider zai, model
`unreachable-mock-model`); the emitted agent surface contains both
asserted tools (`aptu-coder_analyze_directory`,
`aptu-coder_verify_anchors`, plus the full direct 8-tool family).\n
Gates: `pi --version` = 1.0.2; adapter 5.0.0 (verified from the
installed shadow package manifest). Verbatim outputs:
`stratum1-dd6f6b1/sealed-a12/surface-gate-output.jsonl`,
`stratum2-611/sealed/surface-gate-output.jsonl`; gate metadata recorded
in each run root's `manifest.json`.

## F3 activation gate on the corrected surface — PASS

- Stratum 1: 11/11 scorable tool-arm sessions with >= 1 direct
  `aptu-coder_*` toolCall block = 1.0 (threshold 0.8). PASS.
- Stratum 2: 5/5 scorable = 1.0, but n < the A4b minimum sample of 8,
  so the gate records `pending` and passes vacuously; the observed
  rate is 1.0 with 154 direct calls across 8 sessions.
- The A12 drift defect is gone: 262 direct `aptu-coder_*` calls across
  the 20 mcp sessions; zero gateway-mediated `mcp` calls observed.

## Spend ledger

| Component | Sessions | USD |
|---|---|---|
| Stratum 1 pairs (12 x 2) | 24 | 0.330420 |
| Stratum 2 pairs (8 x 2) | 16 | 0.273341 |
| Stratum 1 A9b repairs (17 cells) | 17 | 0.288885 |
| Stratum 2 A9b repairs (9 cells + 1 directive kill) | 10 | 0.154460 |
| **Total fresh run** | **67** | **1.047106** |

The discarded gateway-mediated partial ($0.2984) is not included; it is
not pooled or re-scored as data per A12.

**Budget note (defect, disclosed):** the fresh run exceeded the $0.85
hard budget by $0.197. Causes: (a) observed per-pair cost $0.0302 vs
the $0.0285 projection; (b) the repair driver's cap checked the cap
per stratum rather than against the global cumulative, so Stratum-2
repairs ran past the global allowance; (c) the close-out directive
arrived mid-Stratum-2 repairs. The overrun is bounded ($0.197) and fully
ledgered above; no further spend has occurred since the directive.

Per-session costs are in `stratum1-dd6f6b1/sealed-a12/summary.json`,
`stratum2-611/sealed/summary.json`, and the two
`repairs-verify/summary.json` files (every session metered fail-closed;
no silent kills).

## Defect ledger

| Stratum | Cell | Reason | Metered |
|---|---|---|---|
| 1 | chain/mcp | wait-timeout-killed:900s | yes |
| 2 | back/mcp | turn-cap-exceeded:41 | yes |
| 2 | include/mcp | turn-cap-exceeded:41 | yes |
| 2 | timezone/mcp | wait-timeout-killed:900s | yes |
| 2 repair | timezone/native | killed by close-out directive mid-flight (stopReason toolUse, no answer) | yes ($0.0298) |

No provider-error (`stopReason: "error"`) sessions occurred, so A8 had
nothing to repair.

## Primary endpoint: paired F1 diffs (mcp - native)

As-sealed and repair-adjusted per pair are in
`nrun-closeout-analysis.json` (committed artifact of this record).

| Scope | n | mean | SD | 95% CI (t, df=19) | post-hoc power |
|---|---|---|---|---|---|
| Pooled as-sealed | 20 | -0.0888 | 0.2536 | [-0.2075, 0.0298] | 0.347 |
| **Pooled repair-adjusted** | **20** | **-0.0709** | **0.2319** | **[-0.1794, 0.0377]** | **0.277** |
| Stratum 1 as-sealed | 12 | -0.0472 | 0.1283 | [-0.1247, 0.0304]* | 0.247 |
| Stratum 1 repair-adjusted | 12 | -0.0080 | 0.1589 | [-0.1040, 0.0881]* | 0.054 |
| Stratum 2 as-sealed | 8 | -0.1513 | 0.3758 | [-0.4294, 0.1268]* | 0.207 |
| Stratum 2 repair-adjusted | 8 | -0.1652 | 0.2989 | [-0.3864, 0.0559]* | 0.346 |

\* Per-stratum CIs use the same t(0.975) = 2.093 critical value
(df=19 approximation; the per-stratum df is 11 and 7 respectively, so
per-stratum intervals are nominally slightly narrow).

## Pre-registered claim gate (verbatim) — NOT-SUPPORTED

"supported IFF repair-adjusted pooled 95% CI excludes 0 in mcp's favour
AND mean paired diff >= 0.1."

- Repair-adjusted pooled 95% CI: [-0.1794, 0.0377] — includes 0, and
  the point estimate is in native's favour.
- Mean paired diff: -0.0709 < 0.1.

Both conditions fail. **The sealed stage does NOT support the claim
"aptu-coder improves structural fan-in outcomes at iso-cost".** Per the
A11 decision rule, the paper's claim is re-scoped per the pilot's
option (iii) (activation + cost structure + methodology), with this
N-run recorded as the confirmatory negative result.

## Secondary endpoints

| Metric | native | mcp |
|---|---|---|
| Completion (pairs) | 20/20 | 16/20 (s1 11/12, s2 5/8) |
| Correct cells (F1 >= 0.75, snapshot-verified) | 2 | 2 |
| Pair-session cost | $0.2501 | $0.3536 |
| Correct cells per dollar | 7.99 | 5.66 |
| Mean F1 as-sealed (pooled) | 0.4147 | 0.3258 |
| Mean F1 repair-adjusted (pooled) | 0.4236 | 0.3527 |
| Tokens in / out (pooled) | 460,046 / 250,834 | 552,935 / 270,464 |
| aptu tool calls (total) | 0 | 262 |

Per-stratum: s1 native 2 correct/$13.81-per-dollar vs mcp 1/$5.39;
s2 native 0 vs mcp 1/$5.95. Correct-cell-per-dollar favours native in
both strata; the mcp arm's per-cell cost is ~1.4x native's (schema
overhead + tool turns), consistent with the F5 accounting.

## A9b repair coverage (curtailed)

- Stratum 1: all 17 selected fabricated-anchor cells repaired before
  the directive; 6 cells replaced under the A9 policy
  (`repairs-verify/` artifacts complete).
- Stratum 2: 9 of 9 selected cells repaired and scored from disk;
  5 replaced. A 10th in-flight session (timezone/native) was killed by
  the directive, metered ($0.0298), and kept as-sealed.
- Remaining planned repairs: **none** — the selected cell set was
  finished; the directive landed after the last selected cell's
  salvage. Future verify-repair passes are superseded by the A12
  close-out directive.

## Limitations

- Single provider/model/tier (zai / glm-5.3-flash, fanin-hop2 Track A).
- SD estimate inherited from the n=8 pilot; observed pooled SD 0.2319
  gives post-hoc power 0.277 at the observed effect — the design was
  under-powered for the ratified 0.1 gap in the direction observed.
- Unbalanced strata (12 + 8), pooled on the within-pair difference.
- Runtime drift vs the prior sealed run: this run is pi 1.0.2 +
  pi-mcp-adapter 5.0.0 (mcp-adapter.json); the ratified 8-pair sealed
  run was pi 0.87.1 (mcp.json honoured). Arm semantics are the same
  direct-tool surface per A12's comparability note, but cross-run
  comparisons carry the runtime delta.
- The A9a verifier's word-boundary rule labels most fan-in answers
  `fabricated-anchor` (18/24 s1 sessions, 9/16 s2 sessions as-sealed);
  F1 itself is verdict-independent, but `correct` cells are rare and
  the per-dollar secondary metric rests on a small counts basis.
- The run-root `manifest.json` files carry a corrected snapshot block:
  `pilot.py` hardcodes the stratum-1 pin in its emitted manifest; the
  corrected fields (and this record) are authoritative.

## Artifacts

- `stratum1-dd6f6b1/sealed-a12/` — sessions, summary.json (24 sessions,
  $0.330420), manifest.json (runtime pin + surface-gate metadata),
  crossover.csv, kept-set.json, repairs-verify/ (17 repair sessions),
  surface-gate-output.jsonl.
- `stratum2-611/sealed/` — sessions, summary.json (16 sessions,
  $0.273341), manifest.json (corrected snapshot block),
  repairs-verify/ (10 sessions incl. the directive kill),
  surface-gate-output.jsonl.
- `nrun-closeout-analysis.json` — pooled + per-stratum paired stats,
  per-pair diffs (as-sealed and repair-adjusted), secondary endpoints.

The previously recorded gateway-mediated partial (`stratum1-dd6f6b1/
sealed/`, $0.2984) remains untouched as drift evidence and is excluded
from every statistic above.


---

## Worktree incident (2026-10-04)

The final-run artifacts summarized above were written uncommitted on
`bench/v19-nrun-decision` and were lost from the worktree when a later
branch switch moved to an origin/main-based branch. They were recovered
from the executing agent's transcript and the surviving raw run logs
(`/tmp/v19-nrun/logs/`) and re-committed on this branch. Honesty notes:

- Reproduced-from-data: the per-session summary rows and repair rows
  (cost, stopReason, defect, aptu tool calls, repair F1/verdict) come
  from the harness's own run logs and match the attested totals
  ($0.330420 / $0.273341 / $0.288885 / $0.154460) exactly.
- Transcript-attested: the pooled and per-stratum statistics, per-pair
  paired diffs, verdicts, and arm-level aggregates in
  `nrun-closeout-analysis.json` are the values the closeout script
  computed and printed at run time, transcribed verbatim; they could
  NOT be recomputed from raw session data.
- NOT recovered (lost with the worktree): all raw session JSONLs
  (both run roots and both repairs-verify dirs), `crossover.csv`,
  `kept-set.json`, and `label-map.json` for both run roots, and the
  per-pair absolute F1 values inside the original closeout analysis.
  The `surface-gate-output.jsonl` files and the selection/task pools
  were recovered or regenerated deterministically (task IDs verified
  symbol-for-symbol against the transcript output).
- Nothing in the recovered record has been altered, rounded, or
  "improved" relative to the transcript; gaps are marked, not filled.
