#!/usr/bin/env python3
"""v19 Stage 2 (1-pair smoke) driver for issue #1681.

Runs one Track A hop-1 pair (native + mcp) plus one optional hop-1 Track C
mcp-gateway cell through the full v18 runner pipeline (fail-closed metering,
per-session $0.25 kill, stage cap $0.10 enforced via run_stage_budget_ok),
with v18 blinding (sealed label map, leak-free scores) and v19 F1 scoring.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO / "scripts"))

from bench_v18 import blinding  # noqa: E402
from bench_v18 import runner as v18  # noqa: E402
from bench_v19 import score as v19score  # noqa: E402

SNAPSHOT = Path("/tmp/v19-wiring/django").resolve()
RUN_ROOT = Path("/tmp/v19-smoke2/run1")
TASKS_DIR = REPO / "docs/benchmarks/v19/results/tasks-django"
STAGE = "smoke"
STAGE_CAP = v18.STAGE_BUDGET_CAPS_USD[STAGE]

PATH_RE = re.compile(r"[\w./-]+/[\w./-]+")  # slash-bearing path; dashes/dots allowed


def load_task(track: str, task_id: str) -> dict:
    tasks = json.loads((TASKS_DIR / f"tasks-track-{track}.json").read_text())
    return next(t for t in tasks if t["id"] == task_id)


def run_one(state, task, arm) -> dict:
    """Run one session through the pipeline; abort-before-launch on cap."""
    if not v18.run_stage_budget_ok(state, STAGE):
        return {"skipped": f"stage-cap:{state.halt_reason}"}
    if state.total_spent_usd >= STAGE_CAP:
        return {"skipped": "cumulative-cap-reached"}
    run_id = v18.new_run_id()
    session_dir = RUN_ROOT / "sessions" / run_id / task["id"] / arm
    cmd, env = v18.build_invocation(arm, RUN_ROOT, session_dir, task["prompt"])
    result = v18.run_session(state, STAGE, task["id"], arm, run_id,
                             session_dir, cmd, env, RUN_ROOT)
    files = v18.session_jsonl_files(session_dir)
    # Session-level facts for the report (stopReason, tokens, answer text).
    info = {"run_id": run_id, "arm": arm, "task": task["id"],
            "killed": result.killed, "defect": result.defect,
            "cost_usd": result.cost_usd, "session_files": [], }
    tokens = {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
    stop = None
    answer_texts = []
    for f in files:
        info["session_files"].append(f.name)
        for line in f.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            entry = json.loads(line)
            msg = entry.get("message") if isinstance(entry.get("message"), dict) else {}
            if msg.get("role") == "assistant":
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


def extract_paths(text: str, root: Path) -> set[str]:
    """File paths mentioned in the final assistant text that exist in repo."""
    out = set()
    for cand in PATH_RE.findall(text):
        cand = cand.strip("`*_.")
        p = SNAPSHOT / cand
        if p.is_file():
            out.add(p.resolve().relative_to(SNAPSHOT).as_posix())
    return out


def main() -> None:
    os.chdir(SNAPSHOT)  # sessions must cwd the snapshot
    RUN_ROOT.mkdir(parents=True, exist_ok=True)
    state = v18.LadderState()

    task_a = load_task("A", "task-q-a-hop1-constant_time_compare")
    task_c = load_task("C", "task-c-lookup-constant_time_compare")

    plan = [(task_a, "native"), (task_a, "mcp"), (task_c, "mcp-gateway")]
    assignments: dict[str, str] = {}
    sessions = []
    for task, arm in plan:
        info = run_one(state, task, arm)
        if info.get("skipped"):
            print(json.dumps({"skipped": info["skipped"], "arm": arm,
                              "total": state.total_spent_usd}))
            break
        assignments[info["run_id"]] = arm
        blinding.seal_label_map(RUN_ROOT, "sealed-at-run-time", assignments)
        sessions.append(info)
        print(json.dumps({k: info[k] for k in
                          ("run_id", "arm", "task", "killed", "defect",
                           "cost_usd", "stopReason")}))

    # ---- scoring (blinded) ----
    scores = []
    details = []
    for s in sessions:
        task = task_a if s["task"] == task_a["id"] else task_c
        if task["track"] == "A":
            answer = extract_paths(s["final_text"], SNAPSHOT)
            fabricated = sum(1 for p in answer
                             if p not in set(task["expected_files"]))
            res = v19score.score_task(task, answer, fabricated)
            res["task_id"] = task["id"]
            res["expected_sha"] = v19score.expected_digest(
                "\n".join(sorted(task["expected_files"])))
            scores.append(res)
            details.append({"run_id": s["run_id"], "arm": s["arm"],
                            "answer": sorted(answer), "score": res})
        else:
            answer = extract_paths(s["final_text"], SNAPSHOT)
            ok = answer == set(task["expected_files"])
            metrics = v19score.f1_score(answer, set(task["expected_files"]))
            verdict = ("correct" if ok else
                       "partial" if answer & set(task["expected_files"]) else
                       "no-anchor" if not answer else "incorrect")
            rec = {"verdict": verdict, **metrics,
                   "expected_sha": v19score.expected_digest(
                       "\n".join(sorted(task["expected_files"])))}
            scores.append(rec)
            details.append({"run_id": s["run_id"], "arm": s["arm"],
                            "answer": sorted(answer), "score": rec})

    scores_path = blinding.write_scores(RUN_ROOT, scores)

    # ---- leak check ----
    blob = scores_path.read_text()
    leaked = [w for w in blinding.FORBIDDEN_KEYS
              if f'"{w}"' in blob.lower() or w in blob.lower()]
    report = {
        "sessions": [{k: s[k] for k in ("run_id", "arm", "task", "killed",
                                        "defect", "cost_usd", "stopReason",
                                        "tokens", "session_files")}
                     for s in sessions],
        "details": details,
        "scores_path": str(scores_path),
        "label_map": str(RUN_ROOT / "label-map.json"),
        "leak_check": {"leaked": leaked, "pass": not leaked},
        "total_spent_usd": round(state.total_spent_usd, 6),
        "halted": state.halted,
        "halt_reason": state.halt_reason,
        "defects": state.defects,
    }
    (RUN_ROOT / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
