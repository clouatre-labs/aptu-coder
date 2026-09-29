#!/usr/bin/env python3
"""v19 pilot-runner core (A1/A2 amendments + carry-over ratifications).

Implements the ratified pre-stage-3 changes on top of the frozen v18
runner (never mutating bench_v18 module constants in its own file):

- Wait override (carry-over): SESSION_WAIT_TIMEOUT_S becomes 300s for v19
  runs via an environment-driven assignment on the imported v18 module.
  The frozen default in bench_v18/runner.py is NOT edited.
- Per-session turn cap (fail-closed): the session subprocess is polled
  while it runs; if its session JSONL accumulates more assistant turns
  than the tier cap, the process is killed, the defect
  ``turn-cap-exceeded:<n>`` is recorded in LadderState.defects with
  killed=True, and the spend is still metered (same handling as the v18
  wait-timeout kill).
- Tool-call extraction: aptu-coder tool calls (toolCall blocks in
  assistant messages whose name starts with ``aptu-coder_``) are counted
  per session for the F3 activation gate.
- Activation gate (F3, fail-closed): below 80% of completed scorable
  tool-arm sessions with >=1 aptu-coder tool call, the ladder halts.
- Budget tiers (F1): per-tier turn caps as a plain dict constant; cost
  caps stay in the frozen v18 module ($0.25/session, $0.60 pilot stage,
  $5.00 ceiling) and are never bypassed.
- Prompt parity (F4): reworded Track A callers wording requesting
  file:line anchors for BOTH arms, plus exactly one availability
  sentence appended to tool-arm prompts only. The native arm never
  receives it.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

from bench_v18 import runner as v18

# Carry-over ratification: 120s -> 300s wait deadline for v19, driven by
# environment with a 300s default. Applied to the imported module object
# before any meter_and_close call; bench_v18/runner.py is never edited.
v18.SESSION_WAIT_TIMEOUT_S = int(os.environ.get("SESSION_WAIT_TIMEOUT_S", "300"))

# F1 budget tiers: per-tier turn caps (fail-closed, kill + defect on
# exceedance). The fanin tier floods context with grep output; its cap
# was raised 25 -> 40 by A6b (5/8 re-pilot sessions hit the cap 25
# mid-loop without a final answer even with correct tool data; 40 is
# the sealed-stage cap, still fail-closed via the same kill+defect
# path). Cost caps are NOT tiered: per-session $0.25, pilot stage
# $0.60, ceiling $5.00 (v18).
TIERS: dict[str, dict] = {
    "control": {"turn_cap": 40},
    "fanin": {"turn_cap": 40},
}

# F3 activation gate: fraction of completed scorable tool-arm sessions
# that must contain >=1 aptu-coder tool call, and the halt reason prefix.
ACTIVATION_GATE_THRESHOLD = 0.8
# A4b: a rate gate on a tiny sample is granularity noise (the stage-3
# hop-2 re-pilot halted at 3/4 = 0.75 with one session of slack). The
# gate is only evaluated once at least this many scorable tool-arm
# sessions exist; below that it records "pending" and passes vacuously.
ACTIVATION_GATE_MIN_SAMPLE = 8
ACTIVATION_GATE_REASON = "activation-gate-failed:{pct:.0f}"

# F4 prompt parity. The rewording requests file:line anchors and applies
# to BOTH arms; the availability sentence is appended to tool arms only,
# verbatim as frozen in docs/benchmarks/v19/AMENDMENTS.md (F4).
TRACK_A_REWORDING = (
    "Report file paths only. "
    "Report each calling file as a file:line anchor (path:line)."
)
TRACK_A_REWORD_TARGET = "Report file paths only."
AVAILABILITY_SENTENCE = (
    "In addition to the default tools, repository-analysis tools for "
    "call graphs and symbol lookup are available in this session."
)

TOOL_CALL_PREFIX = "aptu-coder_"


def build_task_prompt(task_text: str, track: str, arm: str) -> str:
    """F4 prompt parity builder.

    Track A prompts get the reworded callers wording (file:line anchors)
    in both arms. Tool arms (mcp, mcp-gateway) additionally get exactly
    the frozen availability sentence appended; the native arm never does.
    Track C prompts are passed through unchanged (parity holds
    trivially; the availability sentence still applies to tool arms).
    """
    if track == "A" and TRACK_A_REWORD_TARGET in task_text:
        text = task_text.replace(
            TRACK_A_REWORD_TARGET, TRACK_A_REWORDING, 1
        )
    else:
        text = task_text
    if arm == "native":
        return text
    if arm not in v18.ARMS:
        raise ValueError(f"unknown arm: {arm}")
    return f"{text} {AVAILABILITY_SENTENCE}"


def count_aptu_tool_calls(session_dir: Path) -> int:
    """Count aptu-coder tool calls across all session JSONL files.

    Tool calls appear as ``toolCall`` blocks inside assistant messages
    (verified against a live session JSONL); aptu-coder tools arrive
    name-prefixed, e.g. ``aptu-coder_analyze_directory``.
    """
    count = 0
    for path in v18.session_jsonl_files(session_dir):
        for line in path.read_text(encoding="utf-8").splitlines():
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
            for block in msg.get("content") or []:
                if (
                    isinstance(block, dict)
                    and block.get("type") == "toolCall"
                    and str(block.get("name", "")).startswith(TOOL_CALL_PREFIX)
                ):
                    count += 1
    return count


def count_assistant_turns(session_dir: Path) -> int:
    """Count assistant messages across all session JSONL files so far."""
    count = 0
    for path in v18.session_jsonl_files(session_dir):
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            msg = entry.get("message") if isinstance(entry.get("message"), dict) else {}
            if msg.get("role") == "assistant":
                count += 1
    return count


def run_session_with_turn_cap(
    state: v18.LadderState,
    stage: str,
    task: str,
    arm: str,
    run_id: str,
    session_dir: Path,
    cmd: list[str],
    env: dict[str, str],
    run_root: Path,
    turn_cap: int,
    poll_s: float = 2.0,
) -> v18.SessionResult:
    """Run one session with the fail-closed per-session turn cap.

    Mirrors v18.run_session's launch/meter structure. While the child
    runs, its session JSONL is polled; exceeding ``turn_cap`` assistant
    turns kills the process, records ``turn-cap-exceeded:<n>`` in the
    defect ledger, and still meters the spend through
    v18.meter_and_close (which retains the missing-cost / per-session
    cost / ceiling / wait-timeout fail-closed behavior).
    """
    resolved = v18.validate_session_dir(session_dir, run_root)
    session_dir.mkdir(parents=True, exist_ok=True)
    session_dir = resolved
    full_env = {k: v for k, v in {**v18._safe_env(), **env}.items()}
    proc = subprocess.Popen(
        cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env=full_env,
    )
    elapsed = 0.0
    turns = 0
    capped = False
    wait_timed_out = False
    while True:
        try:
            proc.wait(timeout=poll_s)
            break
        except subprocess.TimeoutExpired:
            pass
        elapsed += poll_s
        turns = count_assistant_turns(session_dir)
        if turns > turn_cap:
            proc.kill()
            proc.wait()
            capped = True
            break
        if elapsed >= v18.SESSION_WAIT_TIMEOUT_S:
            proc.kill()
            proc.wait()
            wait_timed_out = True
            break
    result = v18.meter_and_close(
        state, stage, task, arm, run_id, session_dir, None,
    )
    if capped:
        reason = f"turn-cap-exceeded:{turns}"
        state.defects.append({
            "stage": stage, "task": task, "arm": arm, "reason": reason,
        })
        result = v18.SessionResult(
            stage, task, arm, run_id, True, reason, result.cost_usd,
        )
    elif wait_timed_out:
        # A7b: a wait-deadline kill is a defect, same fail-closed
        # visibility as a turn-cap kill. Previously this branch fell
        # through with killed=False and no defect, so sessions killed at
        # the wall-clock deadline were indistinguishable from sessions
        # the model ended mid-loop (stopReason toolUse, no answer).
        reason = f"wait-timeout-killed:{elapsed:.0f}s"
        state.defects.append({
            "stage": stage, "task": task, "arm": arm, "reason": reason,
        })
        result = v18.SessionResult(
            stage, task, arm, run_id, True, reason, result.cost_usd,
        )
    return result


def activation_gate(
    tool_arm_sessions: list[dict],
    threshold: float = ACTIVATION_GATE_THRESHOLD,
    min_sample: int = ACTIVATION_GATE_MIN_SAMPLE,
) -> dict:
    """F3 activation gate over completed scorable tool-arm sessions.

    Each session dict needs ``killed`` and ``aptu_tool_calls``. Sessions
    that were killed are excluded (not completed). Per A4b the gate is
    evaluated only when ``len(scorable) >= min_sample``; below that it
    passes vacuously with ``pending`` set so the ledger shows the gate
    has not been judged yet. Returns the gate decision with the pass
    fraction for the ledger.
    """
    scorable = [s for s in tool_arm_sessions if not s.get("killed")]
    if not scorable:
        return {"pass": True, "pending": True, "active": 0,
                "scorable": 0, "fraction": None}
    if len(scorable) < min_sample:
        return {"pass": True, "pending": True, "active": sum(
                    1 for s in scorable if s.get("aptu_tool_calls", 0) >= 1),
                "scorable": len(scorable), "fraction": None}
    active = sum(1 for s in scorable if s.get("aptu_tool_calls", 0) >= 1)
    fraction = active / len(scorable)
    return {
        "pass": fraction >= threshold,
        "active": active,
        "scorable": len(scorable),
        "fraction": fraction,
    }


def gate_halt_reason(gate: dict) -> str:
    """Halt reason string for a failed activation gate (F3)."""
    pct = (gate["fraction"] or 0.0) * 100
    return ACTIVATION_GATE_REASON.format(pct=pct)
