#!/usr/bin/env python3
"""Runner for the v18 benchmark ladder.

Runs per task/arm ``pi -p --mode json`` sessions with per-arm shadow dirs
(PI_CODING_AGENT_DIR) and archival --session-dir, meters cost from the
session JSONL (fail-closed on missing or malformed usage.cost), kills any
session above USD 0.25, and halts the ladder at the cumulative USD 5
ceiling. No live sessions are launched by this repository's CI; execution
is maintainer-only post-merge.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import uuid
from dataclasses import dataclass, field
from pathlib import Path

PER_SESSION_KILL_USD = 0.25
TOTAL_CEILING_USD = 5.0
STAGE_BUDGET_CAPS_USD = {
    "wiring_smoke": 0.10,
    "smoke": 0.10,
    "pilot": 0.60,
    "sealed": 2.00,
}
STAGES = ["wiring_smoke", "smoke", "pilot", "sealed"]

# Per-arm flag set, verbatim per merged #1603 methodology.
COMMON_FLAGS = [
    "--no-extensions", "--no-skills", "--no-context-files",
]
NATIVE_TOOLS = ["--tools", "read,bash"]
MCP_EXCLUDE = ["--exclude-tools", "edit_overwrite,edit_replace,exec_command"]


def new_run_id() -> str:
    """Opaque randomized run ID; never derived from task/model strings."""
    return uuid.uuid4().hex


def build_invocation(
    arm: str, run_root: Path, session_dir: Path, prompt: str
) -> tuple[list[str], dict[str, str]]:
    """Build the (command, env) for one arm's pi invocation."""
    if arm not in ("native", "mcp"):
        raise ValueError(f"unknown arm: {arm}")
    agent_dir = run_root / f"agent-{arm}"
    agent_dir.mkdir(parents=True, exist_ok=True)
    if arm == "native":
        (agent_dir / "mcp.json").write_text('{"mcpServers": {}}\n')
        arm_flags = NATIVE_TOOLS
    else:
        (agent_dir / "mcp.json").write_text(json.dumps({
            "mcpServers": {
                "aptu-coder": {
                    "type": "stdio",
                    "command": "aptu-coder",
                },
            },
        }) + "\n")
        arm_flags = MCP_EXCLUDE
    cmd = ["pi", "-p", "--mode", "json", *COMMON_FLAGS, *arm_flags,
           "--session-dir", str(session_dir.resolve()), prompt]
    env = {"PI_CODING_AGENT_DIR": str(agent_dir.resolve())}
    return cmd, env


def validate_session_dir(session_dir: Path, run_root: Path) -> Path:
    """Resolve session_dir and reject anything outside the run root."""
    if ".." in session_dir.parts or ".." in run_root.parts:
        raise ValueError("session-dir must not contain traversal components")
    resolved = session_dir.resolve()
    allowed = run_root.resolve()
    if resolved != allowed and allowed not in resolved.parents:
        raise ValueError(
            f"session-dir {resolved} resolves outside the allowlisted "
            f"run root {allowed}"
        )
    return resolved


def parse_session_cost(session_jsonl: Path) -> float | None:
    """Sum usage.cost across the session JSONL.

    Returns None on a missing or malformed usage field (fail closed).
    """
    if not session_jsonl.exists():
        return None
    total = 0.0
    seen = False
    for line in session_jsonl.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            return None
        usage = entry.get("usage") if isinstance(entry, dict) else None
        if usage is None:
            continue
        cost = usage.get("cost")
        if isinstance(cost, bool) or not isinstance(cost, (int, float)):
            return None
        total += float(cost)
        seen = True
    return total if seen else None


@dataclass
class SessionResult:
    stage: str
    task: str
    arm: str
    run_id: str
    killed: bool
    defect: str | None
    cost_usd: float | None


@dataclass
class LadderState:
    total_spent_usd: float = 0.0
    halted: bool = False
    halt_reason: str | None = None
    defects: list[dict] = field(default_factory=list)


def meter_and_close(
    state: LadderState,
    stage: str,
    task: str,
    arm: str,
    run_id: str,
    session_jsonl: Path,
    proc: subprocess.Popen | None,
) -> SessionResult:
    """Meter one finished session; kill/record defect on failure (fail-closed)."""
    if proc is not None:
        try:
            proc.wait(timeout=120)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    cost = parse_session_cost(session_jsonl)
    if cost is None:
        state.defects.append({
            "stage": stage, "task": task, "arm": arm,
            "reason": "missing-or-malformed-usage-cost",
        })
        return SessionResult(stage, task, arm, run_id, True,
                             "missing-or-malformed-usage-cost", None)
    state.total_spent_usd += cost
    if state.total_spent_usd > TOTAL_CEILING_USD:
        state.halted = True
        state.halt_reason = "total-ceiling-exceeded"
    if cost > PER_SESSION_KILL_USD:
        reason = f"per-session-cap-exceeded:{cost:.4f}"
        state.defects.append({
            "stage": stage, "task": task, "arm": arm, "reason": reason,
        })
        return SessionResult(stage, task, arm, run_id, True, reason, cost)
    return SessionResult(stage, task, arm, run_id, False, None, cost)


def run_stage_budget_ok(state: LadderState, stage: str) -> bool:
    """Stage cap check: halt the ladder before entering a capped stage."""
    if state.total_spent_usd > STAGE_BUDGET_CAPS_USD.get(stage, 0.0):
        state.halted = True
        state.halt_reason = f"stage-budget-cap-exceeded:{stage}"
        return False
    return True


def run_session(
    state: LadderState,
    stage: str,
    task: str,
    arm: str,
    run_id: str,
    session_dir: Path,
    cmd: list[str],
    env: dict[str, str],
    run_root: Path,
) -> SessionResult:
    """Run one pi session and meter it. Env is passed only to the child."""
    resolved = validate_session_dir(session_dir, run_root)
    session_dir.mkdir(parents=True, exist_ok=True)
    session_jsonl = resolved / "session.jsonl"
    full_env = {
        k: v for k, v in {**_safe_env(), **env}.items()
    }
    proc = subprocess.Popen(
        cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env=full_env,
    )
    return meter_and_close(state, stage, task, arm, run_id, session_jsonl, proc)


_KEY_RE = re.compile(r"KEY|TOKEN|SECRET|PASSWORD", re.IGNORECASE)

# Credentials the child harness itself requires: pi resolves provider auth
# from the environment or from auth.json under the agent dir, and the
# benchmark's fresh PI_CODING_AGENT_DIR shadow dir contains no auth.json,
# so the pre-registered provider key must survive the scrub.
PROVIDER_KEY_ALLOWLIST = frozenset({"ZAI_API_KEY"})


def _safe_env() -> dict[str, str]:
    """Pass through the environment minus obvious credential variables."""
    return {
        k: v for k, v in os.environ.items()
        if not (_KEY_RE.search(k) and k not in PROVIDER_KEY_ALLOWLIST)
    }


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--stage", choices=STAGES, required=True)
    ap.add_argument("--arm", choices=("native", "mcp"), required=True)
    ap.add_argument("--task", required=True)
    ap.add_argument("--prompt", required=True)
    ap.add_argument("--run-root", type=Path, required=True)
    ap.add_argument("--run-id", default=None)
    ap.add_argument("--jsonl", type=Path, default=None,
                    help="pre-existing session JSONL to meter (no-op run)")
    args = ap.parse_args()
    run_id = args.run_id or new_run_id()
    state = LadderState()
    if not run_stage_budget_ok(state, args.stage):
        print(json.dumps({"halted": True, "reason": state.halt_reason}))
        raise SystemExit(2)
    if args.jsonl is not None:
        result = meter_and_close(
            state, args.stage, args.task, args.arm, run_id, args.jsonl, None,
        )
    else:
        session_dir = args.run_root / "sessions" / run_id / args.task / args.arm
        cmd, env = build_invocation(args.arm, args.run_root, session_dir,
                                    args.prompt)
        result = run_session(state, args.stage, args.task, args.arm, run_id,
                             session_dir, cmd, env, args.run_root)
    print(json.dumps({
        "run_id": result.run_id,
        "killed": result.killed,
        "defect": result.defect,
        "cost_usd": result.cost_usd,
        "total_spent_usd": round(state.total_spent_usd, 6),
        "halted": state.halted,
        "halt_reason": state.halt_reason,
        "defects": state.defects,
    }))


if __name__ == "__main__":
    main()
