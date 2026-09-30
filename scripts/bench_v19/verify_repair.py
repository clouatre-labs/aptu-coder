#!/usr/bin/env python3
"""v19 verify-repair mode for fabricated-anchor sessions (A9).

Re-runs exactly the (task, arm) cells whose valid-run session scored
verdict ``fabricated-anchor`` and completed (stopReason ``stop``, not
killed, no defect), re-prompting with a corrective prefix that lists
the fabricated ``path:line`` anchors and restates the anchor rule
(A9a: an anchor verifies only when the cited symbol occurs as a
distinct identifier within the anchor window of the cited line in the
snapshot). One bounded attempt per cell; the filtered task file is
written to a temp path and the frozen task set is never modified.

Emits the sealed-record repairs shape used by
docs/benchmarks/v19/results/tasks-fanin-hop2/sealed/repairs/:

- summary.json: the repair sessions + spend, verbatim policy note
- before-after.json: per-cell before/after with the replacement flag
- arm-means.json: as-sealed and repair-adjusted arm means

Replacement policy (A9): original fabricated-anchor sessions are kept
in the sealed record; a repair replaces its cell only where the repair
completed (stopReason "stop", not killed) AND re-scores with zero
fabricated anchors. Anything else stays as-sealed. Sealed v19 results
are not retroactively re-scored.

Standalone (not a pilot.py flag) per the repair.py/A8 pattern: the
sealed-run code path is untouched; pilot helpers (run_one, score_session,
load_tasks) are reused directly.
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
from bench_v19 import score as v19score  # noqa: E402

FABRICATED_VERDICT = "fabricated-anchor"
REPLACEMENT_POLICY = (
    "A9: fabricated-anchor sessions kept; repair replaces only where "
    "the repair completed (stop) and re-scores with zero fabricated "
    "anchors."
)


def fabricated_anchors_in(text: str, symbol: str, snapshot: Path) -> list[str]:
    """Distinct fabricated ``path:line`` anchors cited in an answer."""
    out: list[str] = []
    seen: set[tuple[str, int]] = set()
    for match in v19score.ANCHOR_RE.finditer(text):
        path = match.group(1)
        lineno = int(match.group(2))
        key = (path, lineno)
        if key in seen:
            continue
        seen.add(key)
        _, fabricated = v19score.verify_anchors(
            f"{path}:{lineno}", symbol, snapshot
        )
        if fabricated:
            out.append(f"{path}:{lineno}")
    return out


def corrective_prefix(fabricated: list[str], symbol: str) -> str:
    """Corrective system/message prefix naming the fabricated anchors."""
    listed = ", ".join(fabricated) if fabricated else "(none)"
    return (
        "\n\nCORRECTION (previous attempt): the following cited anchors "
        f"were fabricated: {listed}. Every file:line anchor you cite "
        f"must be real: the file must exist, the line must be in range, "
        f"and the symbol `{symbol}` must occur as a distinct identifier "
        "within 2 lines of the cited line in the snapshot. Do not cite "
        "an anchor you have not verified."
    )


def select_fabricated_cells(summary: dict) -> list[dict]:
    """Select exactly the completed fabricated-anchor cells, nothing else.

    A cell is selected only when its session scored
    ``verdict == "fabricated-anchor"``, ended ``stopReason == "stop"``,
    was not killed, and carries no defect. Deterministic order.
    """
    cells = []
    for s in summary.get("sessions", []):
        if (s.get("stopReason") == "stop"
                and not s.get("killed")
                and not s.get("defect")
                and s.get("score", {}).get("verdict") == FABRICATED_VERDICT):
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
    out = out_dir / "tasks-verify-repair-filtered.json"
    out.write_text(json.dumps(filtered, indent=2) + "\n", encoding="utf-8")
    return out


def run_repair_one(state, task, arm, tier, snapshot, run_root,
                   note: str) -> dict:
    """One bounded repair attempt with the corrective prefix appended.

    Mirrors pilot.run_one with the corrective note appended to the
    task prompt; the sealed-run pilot loop itself is untouched.
    """
    original = rv19.build_task_prompt
    rv19.build_task_prompt = (
        lambda prompt, track, arm_, _orig=original, _note=note:
        _orig(prompt, track, arm_) + _note
    )
    try:
        return pilot.run_one(state, task, arm, tier, snapshot, run_root)
    finally:
        rv19.build_task_prompt = original


def before_after(before_summary: dict, repair_sessions: list[dict],
                 snapshot: Path) -> list[dict]:
    """Per-cell before/after rows for the A9 verify-repair record."""
    tasks = {
        r["task"]: r for r in before_summary.get("sessions", [])
    }
    rows = []
    for s in repair_sessions:
        b = next((x for x in before_summary.get("sessions", [])
                  if x["task"] == s["task"] and x["arm"] == s["arm"]), None)
        if b is None:
            continue
        task = tasks.get(s["task"], {})
        symbol = task.get("symbol") or s["task"].rsplit("-", 1)[-1]
        _, after_fabricated = v19score.verify_anchors(
            s.get("final_text", ""), symbol, snapshot)
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
            "after_fabricated_anchors": after_fabricated,
            "replaced": bool(
                completed and after_fabricated == 0
                and s["score"].get("verdict") != FABRICATED_VERDICT),
            "repair_cost_usd": s["cost_usd"],
        })
    return rows


def apply_replacement(before_summary: dict, repair_sessions: list[dict],
                      snapshot: Path) -> list[dict]:
    """A9 replacement: kept as-sealed unless the repair completed and
    re-scored with zero fabricated anchors."""
    rows = before_after(before_summary, repair_sessions, snapshot)
    replaced = {(r["task"], r["arm"]) for r in rows if r["replaced"]}
    out = [dict(s) for s in before_summary.get("sessions", [])]
    for i, s in enumerate(out):
        for r in repair_sessions:
            if ((r["task"], r["arm"]) == (s["task"], s["arm"])
                    and (r["task"], r["arm"]) in replaced):
                out[i] = {k: v for k, v in r.items() if k != "final_text"}
    return out


def arm_means(sessions: list[dict]) -> dict:
    """Arm means in the repairs ledger shape: mean F1 and completions."""
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
                    help="repairs output dir (default: <summary dir>/repairs-verify)")
    args = ap.parse_args()

    before_summary = json.loads(
        args.summary.resolve().read_text(encoding="utf-8"))
    snapshot = args.snapshot.resolve()
    cells = select_fabricated_cells(before_summary)
    if not cells:
        print(json.dumps({"repair_cells": 0}))
        return
    arms = [a.strip() for a in args.arms.split(",") if a.strip()]
    for arm in arms:
        if arm not in v18.ARMS:
            ap.error(f"unknown arm: {arm}")

    # Filtered task file lives in a temp dir: the frozen task set is
    # never modified (A9; test-enforced byte identity).
    tmp = Path(tempfile.mkdtemp(prefix="v19-verify-repair-tasks-"))
    filtered_tasks_path = write_filtered_task_file(
        [p.resolve() for p in args.tasks], cells, tmp)

    run_root = args.run_root.resolve()
    os_cwd = Path.cwd()
    os.chdir(snapshot)  # sessions must cwd the snapshot
    run_root.mkdir(parents=True, exist_ok=True)

    tasks = {t["id"]: t for t in pilot.load_tasks([filtered_tasks_path])}
    state = v18.LadderState()
    repair_sessions: list[dict] = []
    cell_keys = {(c["task"], c["arm"]) for c in cells}
    for task_id in sorted(t["id"] for t in tasks.values()):
        task = tasks[task_id]
        symbol = task.get("symbol") or task_id.rsplit("-", 1)[-1]
        before = next((x for x in before_summary.get("sessions", [])
                       if x["task"] == task_id), {})
        fabricated = fabricated_anchors_in(
            before.get("final_text", ""), symbol, snapshot)
        note = corrective_prefix(fabricated, symbol)
        for arm in arms:
            if (task_id, arm) not in cell_keys or state.halted:
                continue
            info = run_repair_one(state, task, arm, args.tier,
                                  snapshot, run_root, note)
            if info.get("skipped"):
                print(json.dumps({"skipped": info["skipped"],
                                  "task": task_id, "arm": arm,
                                  "total": state.total_spent_usd}))
                continue
            info["score"] = pilot.score_session(info, task, snapshot)
            pilot.record_error_session_defect(state, info, stage="verify-repair")
            repair_sessions.append(info)
            print(json.dumps({k: info.get(k) for k in
                              ("run_id", "arm", "task", "killed", "defect",
                               "cost_usd", "stopReason")}))

    rows = before_after(before_summary, repair_sessions, snapshot)
    adjusted = apply_replacement(before_summary, repair_sessions, snapshot)
    pi_version = subprocess.run(["pi", "--version"], capture_output=True,
                                text=True).stdout.strip()

    out_dir = (args.output
               or args.summary.resolve().parent / "repairs-verify")
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "summary.json").write_text(json.dumps({
        "description": "A9 verify-repair of fabricated-anchor sessions",
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
