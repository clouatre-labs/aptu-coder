#!/usr/bin/env python3
"""v19 pilot driver (A1/A2 amendments, stage-3 pilot).

Generalizes the stage-2 smoke driver (smoke_1pair.py) to a full pilot
ladder: iterates task files, runs a native+mcp pair per task (mcp-gateway
only on hop-1 Track C cells, per the v18 runner gate), meters every
session through the v18 fail-closed pipeline with the v19 wait override
and per-tier turn cap, enforces the F3 activation gate after each batch
of tool-arm sessions, scores with v18 blinding + v19 F1/categorical
scoring, and applies the discriminative filter.

Outputs at the run root: summary.json (all sessions), kept-set.json
(filter output), crossover.csv, manifest.json.

No v18 constant is edited in its own file; cost caps ($0.25/session,
$0.60 pilot stage, $5.00 ceiling) come from the frozen v18 module and
are never bypassed.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO / "scripts"))

from bench_v18 import blinding  # noqa: E402
from bench_v18 import runner as v18  # noqa: E402
from bench_v19 import generate_tasks as v19gen  # noqa: E402
from bench_v19 import score as v19score  # noqa: E402
from bench_v19 import runner_v19 as rv19  # noqa: E402

STAGE = "pilot"
STAGE_CAP = v18.STAGE_BUDGET_CAPS_USD[STAGE]
APTU_CODER_VERSION = "0.39.0"
SNAPSHOT_COMMIT = "dd6f6b1"
SNAPSHOT_SHA256 = "9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e"

PATH_RE = re.compile(r"[\w./-]+/[\w./-]+")


def load_tasks(paths: list[Path]) -> list[dict]:
    tasks: list[dict] = []
    for p in paths:
        tasks.extend(json.loads(p.read_text(encoding="utf-8")))
    return sorted(tasks, key=lambda t: t["id"])


def extract_paths(text: str, root: Path) -> set[str]:
    """Existing repo paths mentioned in the final text, file:line stripped."""
    out = set()
    for cand in PATH_RE.findall(text):
        cand = cand.strip("`*_.")
        cand = cand.split(":")[0]
        p = root / cand
        if p.is_file():
            try:
                out.add(p.resolve().relative_to(root).as_posix())
            except ValueError:
                # Mentions of paths outside the snapshot are not
                # repository anchors; skip them instead of crashing.
                continue
    return out


def run_one(state, task, arm, tier, snapshot, run_root) -> dict:
    """Run one session; abort-before-launch on caps, fail-closed metering."""
    if not v18.run_stage_budget_ok(state, STAGE):
        return {"skipped": f"stage-cap:{state.halt_reason}"}
    if state.total_spent_usd >= STAGE_CAP:
        return {"skipped": "cumulative-cap-reached"}
    run_id = v18.new_run_id()
    session_dir = run_root / "sessions" / run_id / task["id"] / arm
    prompt = rv19.build_task_prompt(task["prompt"], task["track"], arm)
    cmd, env = v18.build_invocation(arm, run_root, session_dir, prompt)
    result = rv19.run_session_with_turn_cap(
        state, STAGE, task["id"], arm, run_id, session_dir, cmd, env,
        run_root, turn_cap=rv19.TIERS[tier]["turn_cap"],
    )
    info = {
        "run_id": run_id, "arm": arm, "task": task["id"], "tier": tier,
        "killed": result.killed, "defect": result.defect,
        "cost_usd": result.cost_usd,
        "aptu_tool_calls": rv19.count_aptu_tool_calls(session_dir),
    }
    tokens = {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
    stop = None
    answer_texts: list[str] = []
    for f in v18.session_jsonl_files(session_dir):
        for line in f.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            msg = entry.get("message") if isinstance(entry.get("message"), dict) else {}
            if msg.get("role") != "assistant":
                continue
            u = msg.get("usage") or {}
            for k in tokens:
                tokens[k] += u.get(k) or 0
            if msg.get("stopReason"):
                stop = msg["stopReason"]
            for block in msg.get("content") or []:
                if isinstance(block, dict) and block.get("type") == "text":
                    answer_texts.append(block.get("text", ""))
    info.update({"tokens": tokens, "stopReason": stop,
                 "final_text": answer_texts[-1] if answer_texts else ""})
    return info


def record_error_session_defect(state, info: dict, stage: str = STAGE) -> None:
    """Fail-closed defect ledger entry for provider-error sessions.

    A session ending ``stopReason == "error"`` is a provider-side
    failure (e.g. zai/glm-5.3-flash 5xx mid-loop with an empty 0-token
    assistant message), not an arm outcome. Mirrors the wait-timeout /
    turn-cap fail-closed pattern in runner_v19: if no killed defect was
    already recorded for the session, append a ``provider-error-stop``
    entry to the run summary's defects ledger. Idempotent per
    (task, arm) so re-invocation after scoring cannot duplicate entries.
    """
    if info.get("stopReason") != "error" or info.get("killed"):
        return
    if info.get("defect"):
        return
    reason = "provider-error-stop"
    if any(d.get("task") == info["task"] and d.get("arm") == info["arm"]
           and d.get("reason") == reason for d in state.defects):
        return
    state.defects.append({
        "stage": stage, "task": info["task"], "arm": info["arm"],
        "reason": reason,
    })


def score_session(info: dict, task: dict, snapshot: Path) -> dict:
    """Blinded-style scoring: v19score F1 for Track A, categorical Track C.

    A6a: fabricated anchors are determined by verifying each cited
    file:line anchor against the snapshot (file exists, line in range,
    symbol within the anchor window). Gold-set membership decides only
    precision/recall/F1, never the fabricated count.
    """
    answer = extract_paths(info["final_text"], snapshot)
    expected = set(task["expected_files"])
    if task["track"] == "A":
        symbol = task.get("symbol") or task["id"].rsplit("-", 1)[-1]
        _, fabricated = v19score.verify_anchors(
            info["final_text"], symbol, snapshot
        )
        res = v19score.score_task(task, answer, fabricated)
    else:
        ok = answer == expected
        verdict = ("correct" if ok else
                   "partial" if answer & expected else
                   "no-anchor" if not answer else "incorrect")
        res = {"verdict": verdict, **v19score.f1_score(answer, expected)}
    res["expected_sha"] = v19score.expected_digest(
        "\n".join(sorted(task["expected_files"])))
    return res


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--tasks", type=Path, action="append", required=True,
                    help="task JSON file (repeatable)")
    ap.add_argument("--tier", choices=sorted(rv19.TIERS), required=True)
    ap.add_argument("--arms", default="native,mcp",
                    help="comma-separated: native,mcp[,mcp-gateway]")
    ap.add_argument("--cells", choices=("hop1", "hop2", "hop3", "all"),
                    default="hop2",
                    help="hop filter (default hop2 per amendment A3)")
    ap.add_argument("--max-pairs", type=int, default=None)
    ap.add_argument("--snapshot", type=Path, required=True)
    ap.add_argument("--run-root", type=Path,
                    default=Path("/tmp/v19-pilot/run1"))
    args = ap.parse_args()
    arms = [a.strip() for a in args.arms.split(",") if a.strip()]
    for arm in arms:
        if arm not in v18.ARMS:
            ap.error(f"unknown arm: {arm}")
    hop_filter = None if args.cells == "all" else int(args.cells[-1])
    snapshot = args.snapshot.resolve()
    run_root = args.run_root.resolve()
    os.chdir(snapshot)  # sessions must cwd the snapshot
    run_root.mkdir(parents=True, exist_ok=True)

    tasks = [t for t in load_tasks(args.tasks)
             if hop_filter is None or t.get("hop_depth") == hop_filter]
    state = v18.LadderState()
    assignments: dict[str, str] = {}
    sessions: list[dict] = []
    tool_arm_sessions: list[dict] = []
    pairs = 0

    for task in tasks:
        if args.max_pairs is not None and pairs >= args.max_pairs:
            break
        if state.halted:
            break
        # Arms run as specified by --arms; mcp-gateway additionally only
        # where the cell gate admits it (hop-1 Track C).
        pair_arms = [arm for arm in arms if arm != "mcp-gateway"] + (
            ["mcp-gateway"] if "mcp-gateway" in arms and _arm_allowed(
                arm="mcp-gateway", task=task) else [])
        batch: list[dict] = []
        for arm in pair_arms:
            info = run_one(state, task, arm, args.tier, snapshot, run_root)
            if info.get("skipped"):
                print(json.dumps({"skipped": info["skipped"],
                                  "task": task["id"], "arm": arm,
                                  "total": state.total_spent_usd}))
                if state.halted:
                    break
                continue
            assignments[info["run_id"]] = arm
            blinding.seal_label_map(run_root, "sealed-at-run-time",
                                    assignments)
            info["score"] = score_session(info, task, snapshot)
            # Fail-closed: an error-stop session is a harness defect,
            # not an outcome; give it a ledger entry like the kills do.
            record_error_session_defect(state, info)
            sessions.append(info)
            if arm != "native":
                tool_arm_sessions.append(info)
            batch.append(info)
            print(json.dumps({k: info.get(k) for k in
                              ("run_id", "arm", "task", "tier", "killed",
                               "defect", "cost_usd", "aptu_tool_calls",
                               "stopReason")}))
        pairs += 1
        # F3 activation gate after each batch of tool-arm sessions.
        gate = rv19.activation_gate(tool_arm_sessions)
        if not gate["pass"]:
            state.halted = True
            state.halt_reason = rv19.gate_halt_reason(gate)
            print(json.dumps({"halted": True,
                              "reason": state.halt_reason,
                              "gate": gate}))
            break

    # ---- discriminative filter ----
    kept: list[dict] = []
    for task in tasks:
        arm_sessions = {s["arm"]: s for s in sessions if s["task"] == task["id"]}
        nat, mcp = arm_sessions.get("native"), arm_sessions.get("mcp")
        if not nat or not mcp:
            continue
        f1_native = nat["score"]["f1"]
        f1_mcp = mcp["score"]["f1"]
        divergent = nat["score"].get("verdict") != mcp["score"].get("verdict")
        if v19gen.apply_discriminative_filter(task, f1_native, f1_mcp,
                                              divergent):
            kept.append(task["id"])

    # ---- artifacts ----
    (run_root / "summary.json").write_text(json.dumps({
        "sessions": [{k: s[k] for k in s if k != "final_text"}
                     for s in sessions],
        "total_spent_usd": round(state.total_spent_usd, 6),
        "halted": state.halted,
        "halt_reason": state.halt_reason,
        "defects": state.defects,
        "activation_gate": rv19.activation_gate(tool_arm_sessions),
    }, indent=2) + "\n")
    (run_root / "kept-set.json").write_text(json.dumps(
        {"kept": kept}, indent=2) + "\n")
    with (run_root / "crossover.csv").open("w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["tier", "task", "arm", "cost_usd", "tokens_input",
                    "tokens_output", "f1", "score", "solved"])
        for s in sessions:
            sc = s["score"]
            w.writerow([s["tier"], s["task"], s["arm"], s["cost_usd"],
                        s["tokens"]["input"], s["tokens"]["output"],
                        sc.get("f1"), sc.get("verdict"),
                        sc.get("verdict") == "correct"])
    pi_version = subprocess.run(["pi", "--version"], capture_output=True,
                                text=True).stdout.strip()
    manifest = {
        "aptu-coder-version": APTU_CODER_VERSION,
        "pi-version": pi_version,
        "snapshot-commit": SNAPSHOT_COMMIT,
        "snapshot-sha256": SNAPSHOT_SHA256,
        "repo-head": subprocess.run(
            ["git", "rev-parse", "HEAD"], capture_output=True, text=True,
            cwd=str(REPO)).stdout.strip(),
        "provider": v18.PROVIDER,
        "model": v18.MODEL,
        "amendments": ["A1", "A2", "A3", "A4"],
        "carry-overs": [
            "session-wait-timeout-300s",
            "per-session-turn-cap",
            "track-a-file-line-anchor-prompt-rewording",
        ],
        "jev-layer": "dropped",
        "tier": args.tier,
        "turn_cap": rv19.TIERS[args.tier]["turn_cap"],
    }
    (run_root / "manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n")


def _arm_allowed(arm: str, task: dict) -> bool:
    """v18-runner-gate cell placement for the v19 arms."""
    from bench_v19 import runner as v19runner
    return v19runner.arm_allowed_for_cell(
        arm, task["track"], task.get("hop_depth"))


if __name__ == "__main__":
    main()
