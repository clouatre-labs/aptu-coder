#!/usr/bin/env python3
"""v19 automated repair mode (amendment A8, issue #1696).

Re-runs exactly the (task, arm) cells whose valid-run session ended
``stopReason: "error"`` (provider-side failure, not killed), using a
filtered task file written to a temp path — the frozen task set is
never modified — and emits A8 repair output in the sealed-record shape
used by docs/benchmarks/v19/results/tasks-fanin-hop2/sealed/repairs/:

- summary.json: the repair sessions + spend, verbatim policy note
- before-after.json: per-cell before/after with the replacement flag
- arm-means.json: as-sealed and repair-adjusted arm means

Replacement policy (A8, ratified): original error sessions are kept in
the sealed record; a repair replaces its cell only where the repair
completed (stopReason "stop", not killed). A repair that itself errors
or is killed stays as-sealed. No retroactive re-scoring of kept
sessions; the A6 scorer is unchanged.

Standalone (not a --repair flag in pilot.py) per the SCOUT validation
on #1696: this keeps the sealed-run code path untouched; the pilot loop
helpers (run_one, score_session, load_tasks) are reused directly.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO / "scripts"))

from bench_v18 import runner as v18  # noqa: E402
from bench_v19 import pilot  # noqa: E402
from bench_v19 import runner_v19 as rv19  # noqa: E402

PROVIDER_ERROR_STOP = "provider-error-stop"
REPLACEMENT_POLICY = (
    "A8: error sessions kept; repair replaces only where the repair "
    "completed (stop)."
)


def select_error_cells(summary: dict) -> list[dict]:
    """Select exactly the provider-error repair cells, nothing else.

    A cell is selected only when its session ended
    ``stopReason == "error"``, was not killed, and carries no defect
    (a killed wait-timeout/turn-cap session is a different defect
    class and is excluded from repair selection). Deterministic order.
    """
    cells = []
    for s in summary.get("sessions", []):
        if (s.get("stopReason") == "error"
                and not s.get("killed")
                and not s.get("defect")):
            cells.append({"task": s["task"], "arm": s["arm"]})
    return cells


def write_filtered_task_file(task_paths: list[Path], cells: list[dict],
                             out_dir: Path) -> Path:
    """Write a filtered task JSON file outside the frozen task set.

    Only the tasks named by ``cells`` are included. The frozen task
    files themselves are never opened for writing.
    """
    ids = {c["task"] for c in cells}
    filtered = [t for t in pilot.load_tasks(task_paths) if t["id"] in ids]
    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / "tasks-repair-filtered.json"
    out.write_text(json.dumps(filtered, indent=2) + "\n", encoding="utf-8")
    return out


def before_after(before_summary: dict,
                 repair_sessions: list[dict]) -> list[dict]:
    """Per-cell before/after rows for the A8 repair record."""
    rows = []
    for s in repair_sessions:
        b = next((x for x in before_summary.get("sessions", [])
                  if x["task"] == s["task"] and x["arm"] == s["arm"]), None)
        if b is None:
            continue
        completed = s.get("stopReason") == "stop" and not s.get("killed")
        rows.append({
            "task": s["task"], "arm": s["arm"],
            "before_f1": round(b["score"]["f1"], 4),
            "before_stop": b.get("stopReason"),
            "before_verdict": b["score"].get("verdict"),
            "after_f1": round(s["score"]["f1"], 4),
            "after_stop": s.get("stopReason"),
            "after_defect": s.get("defect"),
            "after_verdict": s["score"].get("verdict"),
            "replaced": bool(completed),
            "repair_cost_usd": s["cost_usd"],
        })
    return rows


def apply_replacement(before_summary: dict,
                      repair_sessions: list[dict]) -> list[dict]:
    """A8 replacement: error sessions kept; completed repairs swap in.

    Returns the repair-adjusted session list. Only cells whose repair
    completed (stopReason "stop", not killed) are replaced; everything
    else — including a repair that itself errored or was killed — stays
    as-sealed, byte-for-byte in content.
    """
    rows = before_after(before_summary, repair_sessions)
    replaced = {(r["task"], r["arm"]) for r in rows if r["replaced"]}
    out = [dict(s) for s in before_summary.get("sessions", [])]
    for i, s in enumerate(out):
        for r in repair_sessions:
            if ((r["task"], r["arm"]) == (s["task"], s["arm"])
                    and (r["task"], r["arm"]) in replaced):
                out[i] = {k: v for k, v in r.items() if k != "final_text"}
    return out


def arm_means(sessions: list[dict]) -> dict:
    """Arm means in the A8 ledger shape: mean F1 and completion counts."""
    out = {}
    for arm in ("mcp", "native"):
        arm_sessions = [s for s in sessions if s["arm"] == arm]
        f1s = [s["score"]["f1"] for s in arm_sessions]
        completed = sum(
            1 for s in arm_sessions
            if s.get("stopReason") == "stop" and not s.get("killed"))
        out[f"{arm}_mean_f1"] = round(sum(f1s) / len(f1s), 4) if f1s else 0.0
        out[f"{arm}_completed"] = f"{completed}/{len(arm_sessions)}"
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--summary", type=Path, required=True,
                    help="sealed run summary.json to repair against")
    ap.add_argument("--tasks", type=Path, action="append", required=True,
                    help="task JSON file (repeatable; frozen, never written)")
    ap.add_argument("--tier", choices=sorted(rv19.TIERS), required=True)
    ap.add_argument("--arms", default="native,mcp",
                    help="comma-separated arms to re-run (default native,mcp)")
    ap.add_argument("--snapshot", type=Path, required=True)
    ap.add_argument("--run-root", type=Path, required=True)
    ap.add_argument("--output", type=Path, default=None,
                    help="repairs output dir (default: <summary dir>/repairs)")
    args = ap.parse_args()

    before_summary = json.loads(
        args.summary.resolve().read_text(encoding="utf-8"))
    cells = select_error_cells(before_summary)
    if not cells:
        print(json.dumps({"repair_cells": 0}))
        return
    arms = [a.strip() for a in args.arms.split(",") if a.strip()]
    for arm in arms:
        if arm not in v18.ARMS:
            ap.error(f"unknown arm: {arm}")

    # Filtered task file lives in a temp dir: the frozen task set is
    # never modified (A8; test-enforced byte identity).
    tmp = Path(tempfile.mkdtemp(prefix="v19-repair-tasks-"))
    filtered_tasks_path = write_filtered_task_file(
        [p.resolve() for p in args.tasks], cells, tmp)

    snapshot = args.snapshot.resolve()
    run_root = args.run_root.resolve()
    os_cwd = Path.cwd()
    os.chdir(snapshot)  # sessions must cwd the snapshot
    run_root.mkdir(parents=True, exist_ok=True)

    tasks = pilot.load_tasks([filtered_tasks_path])
    state = v18.LadderState()
    repair_sessions: list[dict] = []
    cell_keys = {(c["task"], c["arm"]) for c in cells}
    for task in tasks:
        for arm in arms:
            if (task["id"], arm) not in cell_keys or state.halted:
                continue
            info = pilot.run_one(state, task, arm, args.tier,
                                 snapshot, run_root)
            if info.get("skipped"):
                print(json.dumps({"skipped": info["skipped"],
                                  "task": task["id"], "arm": arm,
                                  "total": state.total_spent_usd}))
                continue
            info["score"] = pilot.score_session(info, task, snapshot)
            pilot.record_error_session_defect(state, info, stage="repair")
            repair_sessions.append(info)
            print(json.dumps({k: info.get(k) for k in
                              ("run_id", "arm", "task", "killed", "defect",
                               "cost_usd", "stopReason")}))

    rows = before_after(before_summary, repair_sessions)
    adjusted = apply_replacement(before_summary, repair_sessions)
    pi_version = subprocess.run(["pi", "--version"], capture_output=True,
                                text=True).stdout.strip()

    out_dir = (args.output or args.summary.resolve().parent / "repairs")
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "summary.json").write_text(json.dumps({
        "description": "A8 automated repair of provider-error sessions",
        "replacement_policy": REPLACEMENT_POLICY,
        "repair_spent_usd": round(state.total_spent_usd, 6),
        "repair_binary": pilot.APTU_CODER_VERSION,
        "snapshot_commit": pilot.SNAPSHOT_COMMIT,
        "snapshot_tarball_sha256_verified": pilot.SNAPSHOT_SHA256,
        "wait_timeout_s": v18.SESSION_WAIT_TIMEOUT_S,
        "defects": state.defects,
        "pi-version": pi_version,
        "sessions": [{k: s[k] for k in s if k != "final_text"}
                     for s in repair_sessions],
    }, indent=2) + "\n", encoding="utf-8")
    (out_dir / "before-after.json").write_text(
        json.dumps(rows, indent=2) + "\n", encoding="utf-8")
    (out_dir / "arm-means.json").write_text(json.dumps({
        "as_sealed": arm_means(before_summary["sessions"]),
        "repair_adjusted": arm_means(adjusted),
    }, indent=2) + "\n", encoding="utf-8")
    os.chdir(os_cwd)
    print(json.dumps({"repair_cells": len(cells),
                      "repair_sessions": len(repair_sessions),
                      "repair_spent_usd": round(state.total_spent_usd, 6),
                      "output": str(out_dir)}))


if __name__ == "__main__":
    main()
