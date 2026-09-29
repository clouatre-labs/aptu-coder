# v19 A5 re-pilot — hop-2 fan-in, mcp arm only (`run-fanin-hop2-a5`)

Provenance: this is the first v19 pilot run against a **fixed**
`analyze_symbol` binary (A5, see `../../AMENDMENTS.md`). All prior
tool-arm sessions were poisoned by the call-graph cache-key bug
(#1691, fixed in PR #1692, workspace 0.37.0).

- Binary: aptu-coder 0.37.0 (installed from `fix/call-graph-cache-key`
  at 560e602; manifest records the workspace version string 0.36.1
  because the pilot ran from the pre-bump checkout — the cache fix is
  commit 730c90c in the binary)
- pi: 0.87.1; provider/model: zai / glm-5.3-flash
- snapshot: dd6f6b1 (sha256 9dc904f5...)
- tasks: `tasks-track-a-hop2-a4.json` (sealed 8-task hop-2 set, A4)
- arms: mcp only (`pilot.py --arms` honored; hardcoded native+mcp pair
  fixed in this PR)
- run root: `/tmp/v19-pilot/run-fanin-hop2-a5`
- note: a first attempt at this run was halted mid-flight (command
  timeout) before the `--arms` fix; its partial artifacts were
  discarded, and the native-arm sessions it accidentally started are
  not used anywhere.

## Results (8 sessions, all mcp arm)

| task | aptu calls | stopReason | cost |
|---|---|---|---|
| chain | 0 | toolUse | $0.0117 |
| dec | 12 | toolUse | $0.0134 |
| include | 9 | toolUse | $0.0120 |
| qualname | 5 | toolUse | $0.0094 |
| register_lookup | 8 | stop | $0.0134 |
| skipIfDBFeature | 7 | stop | $0.0064 |
| timezone | 6 | toolUse | $0.0089 |
| view_func | 7 | toolUse | $0.0096 |

- **Total spend: $0.0849** (well under the ~$0.07 projection envelope;
  no sealed-stage spend was triggered — this is a pilot only).
- **F3 activation gate: 7/8 = 0.875 ≥ 0.80, min sample 8 met → PASS**
  (A4b). No kills, no defects, no wait-timeout or turn-cap trips.
- **Flag:** `chain` made 0 aptu tool calls yet stopped `toolUse` —
  the session fell back to non-tool exploration and did not produce a
  final answer. Consistent with the pre-fix observation that `chain`
  (319 callers) is completion-bounded for the tool arm.

Per the task-in-hand protocol, this pilot is the re-pilot record only;
**no sealed-stage spend has occurred.** STOP marker honored.
