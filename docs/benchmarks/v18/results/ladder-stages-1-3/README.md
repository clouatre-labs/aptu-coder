# v18 Ladder — Stages 1–3 (2026-09-22)

Maintainer-run live execution of the pre-sealed ladder: wiring smoke,
1-pair smoke, and 10-pair pilot. The sealed N=40 run was NOT started;
verdict below is GO. Protocol: [methodology.md](../../methodology.md)
(as amended 2026-09-22 — no `--no-extensions`; per-arm shadow dirs;
MCP arm `directTools: true`).

## Verdict per stage

| Stage | Verdict | Sessions | Spend | Kills | Notes |
|---|---|---|---|---|---|
| 1 — wiring smoke | PASS (after #1614) | 2 live (+2 killed pre-fix) | $0.00304 | 0 spurious | Native: zero MCP tools. MCP: exactly the four `aptu-coder_analyze_*` tools, successful `analyze_directory` on a fixture path, cost telemetry present in session JSONL |
| 2 — 1-pair smoke | PASS | 2 | $0.00188 | 0 | Through runner; blinding + archival verified; `scores.json` leak-free; label map sealed (kept private) |
| 3 — 10-pair pilot | PASS | 20 | $0.01322 | 0 | Tasks q-0041–q-0050 (outside sealed q-0000–q-0039); harness-defect inspection: 0 tool errors, 0 empty answers, 0 no-tool-use sessions |

Total spend: **$0.0230** (incl. $0.00486 spent diagnosing the stage-1
defect). The $0.25/session kill fired only on the two legitimate
fail-closed metering kills during the stage-1 defect; never spuriously.

## Manifest

pi 0.86.1 · aptu-coder 0.35.1 · ripgrep 15.2.0 · provider `zai` ·
model `glm-5.3-flash` ($0.075/$0.25 per 1M, re-verified) · fixture tree
sha256 `df76ffb82a0f2a89ebe86876408c5b48a449619e18cba9ca88713b60cc0bf153`
(1031 files, 89,672 LOC) · HEAD `d155706` (includes #1611, #1612,
#1613) · pytest 36 passed at run time.

## Defects found (fixed in #1614)

Stage 1 fail-closed metering killed both sessions on first attempt;
diagnosis surfaced three real harness defects in `scripts/bench_v18/runner.py`:

1. pi writes session files as `<timestamp>_<uuid>.jsonl`, not
   `session.jsonl`; the runner metered a nonexistent path. Metering now
   globs all `*.jsonl` under the session dir.
2. `usage.cost` is a component dict (`input/output/cacheRead/cacheWrite/total`)
   nested under the assistant `message`, not a bare top-level number.
   Parser handles both shapes; malformed cost still fails closed.
3. `--exclude-tools edit_overwrite,...` never matched because directTools
   surfaces prefixed names (`aptu-coder_edit_overwrite`); excluded tools
   still surfaced. Denylist now uses prefixed names; verified live that
   exactly the four analysis tools surface (the adapter's own
   `mcp`/`mcpScript` gateway tools remain — they are the adapter, not
   aptu-coder tools).

## Sealed N=40: GO

Projected sealed spend at observed rates (~$0.00135/pair) ≈ $0.06 — far
under the $2.00 sealed cap with ~$4.97 of the $5.00 ceiling in reserve.
Frozen config: branch `fix/v18-metering-wiring` @ `e484e38` (PR #1614),
same manifest. Run in a separate session.

## Pilot efficiency observation (indicative, n=10, not a claim)

Total tokens/session: native ~4.1k vs MCP ~16.6k; wall time 17.0s vs
19.4s. The pilot task family (single string-constant lookup on a
synthetic fixture) is ripgrep-optimal and adversarial to structured
analysis: aptu-coder's fixed context-loading overhead cannot amortize.
This does not reproduce or refute the v12/v13 README benchmarks, which
used real codebases and multi-step structural tasks. Any token-savings
re-validation needs a v19 task family of comparable complexity.

## Files

- `manifest.json` — pre-flight manifest
- `sessions.json` — all 26 sessions (incl. the 2 pre-fix kills), opaque
  run IDs, per-session metered cost
- `stage2-scores.json` — blinded scores (no arm/model/label fields)

Raw session transcripts and the sealed label map are archived privately
(dotfiles `archive/aptu-coder-v18-ladder-raw-20260922.tar.gz`); they are
excluded from the repo for size and blinding hygiene.
