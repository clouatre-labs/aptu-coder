# KG Pull-Ablation Benchmark Harness (issues #1809, #1824)

Standalone stdlib-only harness executing the pre-registered design in
[docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md](../../docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md).
The v20 design (frozen, pre-registered) lives in
[docs/audit/2026-11-19-v20-ablation-benchmark-design.md](../../docs/audit/2026-11-19-v20-ablation-benchmark-design.md).
It does not import any frozen bench_v18/bench_v19 module and does not
modify the aptu-coder tool surface.

## Components

- `config.py`: pre-registered constants (arms, v20 fixture packs, 24-cell
  matrix, USD 0.64 spend cap re-registered from observed per-session costs,
  provider/model, defect signatures, pinned prompt).
- `proxy.py`: stdio MCP visibility proxy. `no-graph` strips
  `analyze_symbol` from `tools/list` responses and rejects
  `resources/list`, `resources/templates/list`, graph `resources/read`,
  and `analyze_symbol` `tools/call` with clean JSON-RPC errors.
  `optional-graph` is full passthrough. `forced-graph` is passthrough
  plus a one-time fixed graph-workflow preamble injected as a logging
  notification after `initialize` (the runner prompt carries the same
  pinned instruction; this is the availability upper bound arm).
- `runner.py`: session driver. Takes a single-instance `flock` lock on
  `<run_root>/.runner.lock` before any cell runs (a second invocation
  exits nonzero; the #1818 duplication defect is structurally
  impossible). Per cell it builds an isolated `PI_CODING_AGENT_DIR`
  shadow dir (mcp.json points at the proxy, settings.json installs only
  pi-mcp-adapter), runs `pi -p --mode json`, captures the full JSON
  transcript, meters NET tokens and cost from the pi session JSONL
  (fail-closed), enforces the per-run kill (USD 0.03) and the cumulative
  USD 0.64 cap, and applies the transcript-completeness stop rule (an
  incomplete run halts and is reported, never silently excluded).
- `fixtures/`: four self-contained Rust fixture packs
  (`family1-contract-drift`, `family2-reference-resolution`,
  `family3-pattern-mode`, `clean-control`), each with Cargo.toml,
  src/lib.rs, and ground-truth.json. Defective packs are compile-invisible
  (`cargo check` and `cargo test` pass) and grep-hostile (no distinctive
  literal near the defect; dense lexical collisions). Ground truth lives
  only in ground-truth.json and `config.py`, never in source comments.
- `preflight.py`: fail-closed gate run before any session. Verifies
  cargo check and cargo test on every pack, ground-truth schema validity,
  DEFECT_SYMBOLS coverage, and grep-hostility (distinctive-literal
  defect-locating queries must fail to localize). Missing cargo or any
  failed check exits nonzero.
- `adjudicate.py`: pre-registered binary named-defect rubric
  (catch / false positive / false negative / false reassurance; generic
  hedges are not catches). Single-reviewer mode: automated classification
  with evidence excerpts plus explicit reviewer overrides.
- `score.py`: fixture-by-arm tables and unique-catch comparison vs the
  no-graph arm (the decision-rule inputs).

## Usage

```bash
# 0. Preflight every fixture pack (fail-closed; requires cargo):
python3 scripts/kg-ablation/preflight.py
# 1. Run the matrix (sessions cost real spend; cap USD 0.64; one
#    runner instance per run root -- a second exits nonzero):
python3 scripts/kg-ablation/runner.py \
  --run-root .kg-ablation-runs \
  --repo-root . \
  --fixture-prs '{"family1-contract-drift":N1,"family2-reference-resolution":N2,"family3-pattern-mode":N3,"clean-control":N4}'
# 2. Adjudicate before aggregating:
python3 scripts/kg-ablation/adjudicate.py --run-root .kg-ablation-runs \
  --manifest .kg-ablation-runs/manifest.jsonl \
  --out-dir .kg-ablation-runs/adjudications --overrides '{...}'
# 3. Aggregate:
python3 scripts/kg-ablation/score.py --sheets-dir .kg-ablation-runs/adjudications
# 4. Close any fixture draft PRs and delete their branches.
```

Tests: `python3 -m unittest discover -s scripts/kg-ablation`

## Declared deviations

- **Provider/model:** the original design pre-registered
  `mistral-small-2603`; no Mistral credential exists in the execution
  environment (only `ZAI_API_KEY`), so all sessions run with
  `zai/glm-5.3-flash`. The v20 design records this as the declared
  deviation up front, before any result. All findings are conditional
  on this model.
- **forced-graph preamble mechanism:** injected as an MCP logging
  notification after `initialize` plus the pinned prompt suffix, since a
  proxy cannot rewrite the agent's system prompt.
- Provider auth: `ZAI_API_KEY` from the environment (never logged;
  env is credential-scrubbed before reaching pi, per the bench_v18
  pattern).
