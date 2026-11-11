# KG Pull-Ablation Benchmark Harness (issue #1809)

Standalone stdlib-only harness executing the pre-registered design in
[docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md](../../docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md).
It does not import any frozen bench_v18/bench_v19 module and does not
modify the aptu-coder tool surface.

## Components

- `config.py`: pre-registered constants (arms, fixtures, 24-cell matrix,
  USD 0.069 spend cap, provider/model, defect signatures, pinned prompt).
- `proxy.py`: stdio MCP visibility proxy. `no-graph` strips
  `analyze_symbol` from `tools/list` responses and rejects
  `resources/list`, `resources/templates/list`, graph `resources/read`,
  and `analyze_symbol` `tools/call` with clean JSON-RPC errors.
  `optional-graph` is full passthrough. `forced-graph` is passthrough
  plus a one-time fixed graph-workflow preamble injected as a logging
  notification after `initialize` (the runner prompt carries the same
  pinned instruction; this is the availability upper bound arm).
- `runner.py`: session driver. Per cell it builds an isolated
  `PI_CODING_AGENT_DIR` shadow dir (mcp.json points at the proxy,
  settings.json installs only pi-mcp-adapter, mirroring the
  live-verified bench_v18 isolation pattern), runs
  `pi -p --mode json`, captures the full JSON transcript, meters NET
  tokens and cost from the pi session JSONL (fail-closed), enforces the
  per-run kill (USD 0.004) and the cumulative USD 0.069 cap, and applies
  the transcript-completeness stop rule (an incomplete run halts and is
  reported, never silently excluded).
- `adjudicate.py`: pre-registered binary named-defect rubric
  (catch / false positive / false negative / false reassurance; generic
  hedges are not catches). Single-reviewer mode: automated classification
  with evidence excerpts plus explicit reviewer overrides.
- `score.py`: fixture-by-arm tables and unique-catch comparison vs the
  no-graph arm (the decision-rule inputs).

## Usage

```bash
# 1. Create the four throwaway fixture draft PRs (see the design doc's
#    fixture table and preflight validation requirements).
# 2. Run the matrix (sessions cost real spend; cap USD 0.069):
python3 scripts/kg-ablation/runner.py \
  --run-root .kg-ablation-runs \
  --repo-root . \
  --fixture-prs '{"broken-caller":N1,"dead-code-path":N2,"wrong-trait-impl":N3,"clean-control":N4}'
# 3. Adjudicate before aggregating:
python3 scripts/kg-ablation/adjudicate.py --run-root .kg-ablation-runs \
  --manifest .kg-ablation-runs/manifest.jsonl \
  --out-dir .kg-ablation-runs/adjudications --overrides '{...}'
# 4. Aggregate:
python3 scripts/kg-ablation/score.py --sheets-dir .kg-ablation-runs/adjudications
# 5. Close the fixture PRs and delete their branches.
```

Tests: `python3 -m unittest discover -s scripts/kg-ablation -t scripts/kg-ablation`

## Declared deviations

- **Provider/model:** the design pre-registered `mistral-small-2603`;
  no Mistral credential exists in the execution environment (only
  `ZAI_API_KEY`), so all 24 sessions run with `zai/glm-5.3-flash`.
  This reduces cross-audit comparability and evidentiary weight.
- **forced-graph preamble mechanism:** injected as an MCP logging
  notification after `initialize` plus the pinned prompt suffix, since a
  proxy cannot rewrite the agent's system prompt.
- Provider auth: `ZAI_API_KEY` from the environment (never logged;
  env is credential-scrubbed before reaching pi, per the bench_v18
  pattern).
