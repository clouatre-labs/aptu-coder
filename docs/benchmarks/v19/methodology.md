# v19: Real-Codebase Structural-Reasoning Benchmark

Status: **design — implementation not started.** This document is the v19
methodology; generator/scorer code and the vendored snapshot are
delivered by the follow-up implementation issue. v18 artifacts
([../v18/](../v18/)) are frozen and are not modified by v19; v19 reuses
the v18 harness machinery (runner, blinding, scoring, stats) unchanged
except where noted.

## Motivation

v18's pilot finding (see
[../v18/results/ladder-stages-1-3/README.md](../v18/results/ladder-stages-1-3/README.md))
was that the v18 synthetic fixture is **ripgrep-optimal**: a
find-a-string-constant task has no structure for `analyze_symbol` /
call graphs to exploit, so the MCP arm paid aptu-coder's fixed
context-loading overhead (~16.6k vs ~4.1k tokens/session) with no
compensating benefit. This does **not** refute the v12/v13 README
claims (59% / 46–68% token savings), which were earned on **real
codebases with multi-step structural tasks**.

v19 re-tests the token-savings claim under the conditions where it was
originally earned: a real OSS codebase with organic structure and tasks
that require structural reasoning (callers-of-X, transitive depth,
rename impact) where grep-alone is the wrong tool and structured
analysis can amortize its setup cost.

## Codebases

Real OSS, vendored as a pinned snapshot with hash verification (no
generation, unlike v18). Primary repo continues the v12 lineage.

### Primary: Django (Python)

- Upstream: `https://github.com/django/django`
- Pinned commit: `dd6f6b1531984823e3dc56740dfa93f3ceb09357` (2026-09-20)
- Vendored scope: `django/` source tree only (tests, docs, and
  `tests/` scaffolding excluded) — **~194k LOC** (165,503 Python across
  907 files + 28,141 JS across 86 files, verified by line count on the
  pinned commit)
- License: BSD 3-clause (DSF); redistribution permitted with attribution
- Why: v12 continuity (same repo family, comparable task shapes);
  deep, organic module structure (`contrib.auth`, `db.models`,
  `forms`) with real call graphs, mixin hierarchies, and
  cross-module dependency chains — exactly the structure
  `analyze_symbol` exploits

### Secondary: rust-clippy (Rust)

- Upstream: `https://github.com/rust-lang/rust-clippy`
- Pinned commit: `f0c668aa82a404de25d4f331210e018b3eb8feed` (2026-09-22)
- Vendored scope: `clippy_lints/`, `clippy_utils/`, `clippy_config/` —
  **~155k LOC Rust** (132,951 + 19,752 + 2,576 across 778 files,
  verified by line count on the pinned commit)
- License: Apache-2.0 / MIT dual
- Why: widens language coverage to Rust (aptu-coder's first-class
  tree-sitter support); `clippy_lints` has a highly regular per-lint
  module structure with heavy shared-helper call graphs through
  `clippy_utils` — good discriminative material for callers-of-X and
  rename-impact tasks

### Considered and rejected

- **eslint** (`lib/` ≈ 107k JS/TS): fits the LOC band, but TS/JS call
  resolution in-tree is no stronger a differentiator than Python here,
  and three repos would double the pilot budget. Deferred to v20.
- **cpython / rust-lang/rust**: far above the 100–300k LOC band.

The final snapshot set (both repos, exact paths, per-file SHA256
manifest) is committed under `docs/benchmarks/v19/snapshots/` and
verified by hash in CI, mirroring the v18 fixture-determinism check.

## Design

### Arms (unchanged from v18)

Two arms, per the v18 merged methodology (shadow dirs, no
`--no-extensions`, MCP arm `directTools: true`, prefix-corrected
`--exclude-tools`):

- **Native arm:** pi with `--tools read,bash` (rg available via bash).
- **MCP arm:** pi with the four `aptu-coder_analyze_*` tools surfaced
  via the MCP adapter; edit/exec tools excluded.

Pinned model: `glm-5.3-flash` via `zai` ($0.075/$0.25 per 1M,
re-verified at run time). Any model change is an explicit budget re-cut
and invalidates comparability with v18.

### Task tracks (3 × ~10 tasks = 30 tasks per repo-family decision, see Task selection)

**Track A — Structural trace (the claim-testing track).**
Tasks that require graph reasoning: callers-of-X (transitive, with
depth), mutation/rename impact, override chains, cross-module
dependency paths. Native arm limited to read/bash; MCP arm is expected
to use `analyze_symbol` call graphs and `analyze_module`/`analyze_file`
indexes. Both arms *may* grep; the hypothesis is that grep-alone
degrades on transitive tasks (misses dynamic dispatch, re-exports,
aliasing) while the MCP arm's graph queries stay complete.

**Track B — Multi-step modification prep (v12-style amortization
track).** Tasks in the shape of v12's migration-prep: "enumerate the
integration points and affected files for a hypothetical change X,
with file:line evidence." These are long sessions with many distinct
files to touch, where fixed context-loading cost amortizes over
repeated structural lookups.

**Track C — Lookup control (calibration baseline).** v18-style
single-lookup tasks (find the definition of Y, locate the constant Z).
Expected result from v18: native wins on tokens. The purpose here is
not to win but to **quantify the crossover point**: the task-shape
complexity at which the MCP arm's amortized cost drops below the
native arm's cumulative re-search cost. Track C anchors one end of the
curve; Track A anchors the other.

### Scoring: partial credit for Track A

v18's categorical scorer (`correct` / `incorrect` / `omitted` /
`fabricated-anchor` / `no-target-read`) is too coarse for caller-list
tasks. v19 adds a **set-recall scoring layer** for Track A:

- Per task, the oracle defines a ground-truth set of entities (e.g.
  call sites) each with a file:line anchor.
- Score = `|answer ∩ truth| / |truth|` (recall), penalized by
  `|answer \ truth| / |answer|` (precision) — reported as F1-style
  `2RP/(R+P)`, plus the categorical verdicts for anchors that resolve
  to nothing (`fabricated-anchor`) and answers with no cited anchor
  (`no-anchor`).
- A run is `correct` iff recall ≥ 0.9 with no fabricated anchors;
  partial values in (0, 0.9) are `partial`. Tracks B and C keep the
  v18 categorical scale.

Anchor resolution reuses the v18 scorer logic (resolve cited
file:line against the vendored snapshot).

#### Anchor adjudication layer (post-pilot amendment #1686)

**Status: calibration-gated, not yet active.** The v19 pilot's stage-2
smoke (WATCH-TRIP gate, #1681) exposed a textual-resolution failure
mode: a semantic-role judgment (call site vs definition vs mention) was
miscounted as a `fabricated-anchor`. To fix this, an anchor-adjudication
layer was specified in #1686 using Jev as a *verification* oracle, not a
decision-maker:

- A frozen question protocol asks exactly one `choice` question per
  cited anchor over five anchor-role criteria (`call_of_target`,
  `definition_of_target`, `textual_mention_only`,
  `other_symbol_same_name`, `unrelated`), with no arm or human labels in
  the state. The protocol, the model pin (`jev-1.13.0`), and the
  code-side decision rule are manifest-recorded in
  `scripts/bench_v19/adjudicate.py`.
- The code owns the decision rule: `call_of_target` with the anchor in
  the oracle set scores as a true positive; `call_of_target` outside the
  oracle set flags an adjudication disagreement for human review (never
  silently scored); every other role takes a precision penalty per the
  existing F1 layer. The AST oracle remains the sole ground truth for
  the oracle *set*; Jev only classifies the *role of cited evidence*.
- The layer activates only if calibration freezes: measured Jev-vs-label
  agreement must be ≥ 0.9 overall with no role below 0.75 (gate
  constants in `scripts/bench_v19/calibrate.py`). Calibration runs with
  zero benchmark spend, before any stage-3 pilot; a gate failure means
  the protocol is amended or dropped and textual resolution stays.

### Token-savings measurement (the primary endpoint)

Per session, from the pi JSONL `message.usage` shape handled by the
v18 runner: total tokens and cost. Primary endpoint per task-pair:
**token delta (native − MCP) and correctness delta**, jointly — a
token saving only counts toward the claim when Track-A correctness
does not regress. The claim under test, restated for v19:

> On structural-reasoning tasks over a real codebase, the MCP arm
> achieves equal or better correctness at lower total token cost than
> the native arm.

### Task selection rule (pilot-first, discriminative-power filter)

1. Draft ~2× the target task count per track.
2. Run one pilot pair (native + MCP, 1 session each) per task.
3. **Keep** tasks where arm correctness diverges (Track A: F1 gap
   ≥ 0.2 in either direction; Tracks B/C: any categorical divergence)
   or where token deltas are informative for the crossover curve.
4. **Drop** tasks both arms ace (non-discriminative) or both fail
   (broken/ambiguous task).
5. Survivors become the sealed task set; the sealed dataset is drawn
   from a disjoint ID range exactly as in v18 (pilot q-pN vs sealed
   q-sN) so no sealed task is ever seen during piloting.

## Stage ladder

Identical shape to v18 (reuse `scripts/bench_v18/runner.py` stage
logic, renamed/parameterized as `bench_v19`):

| Stage | Content | Cap |
|---|---|---|
| 1 — wiring smoke | 1 pair on Django, tool-surface + telemetry assertions | $0.10 |
| 2 — 1-pair smoke | 1 pair through full runner + blinding + scoring | $0.10 |
| 3 — pilot | discriminative-power filter (above), ~40–60 pairs | $0.60 |
| 4 — sealed | sealed task set, N per kept task | $2.00 |

Budget machinery is unchanged and fail-closed: **$0.25/session kill**,
malformed/missing usage is a kill + defect record, hard cumulative
**$5.00 ceiling** halting the ladder.

**Open budget question (see below):** Track B multi-step sessions may
legitimately exceed $0.25/session at 16.6k-token MCP-arm rates —
recheck after the stage-3 pilot; if the median MCP Track-B session
exceeds ~$0.10 the kill must be re-cut explicitly (e.g. $0.50) with
the change recorded in the manifest, not silently.

### Statistical plan

Same as v18 (`stats.py` reused): exact sign test (primary), Wilcoxon
signed-rank, rank-biserial, bootstrap CIs; pairs are per-task
native-vs-MCP. Track A analyzed on F1; all tracks on token totals.
No pooling across tracks for the primary endpoint — tracks report
separately, because Track C is *expected* to favor native and would
dilute Track A.

## Isolation, blinding, metering

Reused verbatim from v18: per-arm shadow `PI_CODING_AGENT_DIR`
(`mcp.json` + minimal `settings.json`), opaque uuid4 run IDs, sealed
`label-map.json`, leak-free `scores.json`, per-session fail-closed
metering. See
[../v18/harness.md](../v18/harness.md) for the verbatim flag sets.

## Oracle construction for a real repo

Unlike v18 (generated fixture → oracle from the generator's own
structure), v19's ground truth must be extracted from the vendored
snapshot and **independently verified**:

1. **Extraction.** A build script (`scripts/bench_v19/oracle.py`)
   extracts candidate ground truth from the snapshot using *both*
   ripgrep patterns and tree-sitter queries via the aptu-coder-core
   parser — the two methods must agree, or the task is rejected
   (disagreement usually means dynamic dispatch or aliasing that
   would also confuse the arms).
2. **Dynamic-dispatch audit.** For Python tasks, candidate
   callers-of-X lists are audited for overridden methods: if the
   target method participates in an inheritance chain, the oracle
   enumerates the full MRO-resolved call-site set, and the task prompt
   explicitly scopes the question (e.g. "callers of
   `Model.save` in `django/db/` excluding subclasses' overrides")
   so "correct" is well-defined.
3. **Human spot-check.** ≥20% of sealed tasks (and 100% of tasks with
   >20 truth entries) are manually verified by a reviewer who does
   not see arm outputs. Verification notes are recorded in the
   oracle manifest.
4. **Stability check.** Oracle regeneration on the pinned SHA must be
   byte-identical (hash-checked in CI, like the v18 fixture).

## Open questions (must be resolved before implementation)

1. **Snapshot licensing/redistribution.** Both licenses (BSD-3,
   Apache-2.0/MIT) permit redistribution with attribution, but
   committing ~350k LOC of third-party code into this repo needs an
   explicit NOTICE + provenance manifest decision. Alternative:
   CI downloads at run time by pinned SHA + hash (no vendoring in
   git), trading reproducibility-offline for repo hygiene.
2. **Snapshot size in git.** Two vendored trees ≈ several MB. Does
   this repo accept that, or does the snapshot live in a release
   artifact / separate branch fetched by the runner?
3. **CI verification path.** v18 CI regenerates the fixture and
   compares hashes. v19's equivalent (download both repos at pinned
   SHAs, hash the vendored scope) hits network + rate limits in CI;
   needs either a cached tarball or a manual/nightly job.
4. **Session budget for Track B.** Multi-step modification-prep
   sessions will be the most expensive of the benchmark; whether the
   $0.25/session kill survives stage-3 pilot data is unresolved
   (see ladder section).
5. **Pin freshness.** Pinned SHAs are from 2026-09-20/22 (HEAD at
   design time). They must be re-frozen at implementation time and
   never re-pinned after oracle generation — the oracle, snapshot,
   and task set are a frozen triple.
6. **Rust task shape.** clippy_lints' regularity makes some structural
   tasks trivially greppable (`fn check_*` naming convention). Task
   drafting must prefer cross-crate `clippy_utils` helper chains over
   within-lint lookups, or Track A collapses to Track C on this repo.
7. **Sealed N per kept task.** v18 used N=40 total sessions. With
   ~20–30 kept tasks across two repos, N per task may be as low as
   1–2 pairs; whether the sealed stage re-uses v18's fixed N=40 or
   scales with the kept-task count (with a revised cap) is a
   pilot-stage decision.

## Deliverables (implementation issue scope)

- `scripts/bench_v19/`: snapshot fetcher/verifier, task generator
  (templates below), oracle builder, v18-reusing runner/score/stats
  adapters.
- `docs/benchmarks/v19/snapshots/`: vendored trees + SHA256 manifest
  (or the CI-fetch equivalent, per open question 1/2).
- `docs/benchmarks/v19/tasks/`: task prompts, seeded and templated.
- pytest suite extension in `scripts/tests/` (snapshot hash
  verification, oracle stability, partial-credit scorer).

### Task templates

**Track A (structural trace)** — parameterized: `{repo_scope},
{target_symbol}, {relation}`:

- *callers:* "List every call site of `{target}` under `{scope}` with
  file:line. Include transitive callers at depth 2 via `{hops}`."
- *rename-impact:* "`{target}` is being renamed to `{new}`. List every
  file requiring an edit and the line count of edits per file."
- *override-chain:* "Enumerate the override chain of `{method}` from
  `{base}` through all subclasses, with defining file:line each."

Ground truth: oracle caller-site set + anchor list (see Oracle).

**Track B (multi-step modification prep)** — v12-style:

- "We plan to {change}, e.g. replace `{module_a}` usage of `{api}` with
  `{api_b}`. Produce a preparation plan: every file to touch, the
  integration points with file:line, and the risk ordering." Ground
  truth: a curated (human-verified) file set + integration-point
  anchors; scored on the v18 categorical rubric with the file-set
  recall as a sub-check.

**Track C (lookup control)** — v18-style:

- "In which file and at what line is `{symbol}` defined?" /
  "What is the value of `{constant}` and where is it defined?" Ground
  truth: single anchor; v18 categorical scoring.

## File references

- v18 methodology (reused harness contract): [../v18/methodology.md](../v18/methodology.md)
- v18 harness artifacts: [../v18/harness.md](../v18/harness.md)
- v18 ladder results (pilot finding): [../v18/results/ladder-stages-1-3/README.md](../v18/results/ladder-stages-1-3/README.md)
- v12 methodology (task-shape lineage): [../v12/methodology.md](../v12/methodology.md)
- Django pinned commit: `dd6f6b1531984823e3dc56740dfa93f3ceb09357`
- rust-clippy pinned commit: `f0c668aa82a404de25d4f331210e018b3eb8feed`
