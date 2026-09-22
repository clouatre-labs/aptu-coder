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

# Pre-registered model (methodology "Model (one)"); pi only honors explicit
# --provider/--model flags (PI_PROVIDER/PI_MODEL are session-state outputs,
# not selection inputs), so the manifest-pinned model must be on the command
# line or sessions silently fall back to the harness default provider.
PROVIDER = "zai"
MODEL = "glm-5.3-flash"

# Per-arm flag set. --no-extensions is intentionally absent: MCP support in
# pi is provided by the pi-mcp-adapter package, which is itself an extension
# and would be disabled by that flag (verified live: with --no-extensions
# plus a fresh shadow dir, the shadow-dir mcp.json is never read and the
# MCP arm exposes zero MCP tools). Ambient-configuration isolation is
# instead provided by the PI_CODING_AGENT_DIR shadow dir: the launcher's
# real agent dir is fully shadowed, so ambient extensions, skills, and
# mcp.json cannot leak. Each shadow dir carries a minimal settings.json
# naming only the packages that arm needs.
COMMON_FLAGS = [
    "--no-skills", "--no-context-files",
]
NATIVE_TOOLS = ["--tools", "read,bash"]
# Tool names must match the directTools-surfaced names exactly: aptu-coder
# tools arrive prefixed (aptu-coder_edit_overwrite etc.), so an unprefixed
# denylist silently fails to exclude them (found live in wiring smoke).
MCP_EXCLUDE = ["--exclude-tools",
               "aptu-coder_edit_overwrite,aptu-coder_edit_replace,"
               "aptu-coder_exec_command"]


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
        (agent_dir / "settings.json").write_text('{"packages": []}\n')
        arm_flags = NATIVE_TOOLS
    else:
        # directTools: true surfaces the four analysis tools directly by
        # name (aptu-coder_analyze_directory etc.) instead of behind the
        # adapter's mcp search/call gateway tool; verified live.
        (agent_dir / "mcp.json").write_text(json.dumps({
            "mcpServers": {
                "aptu-coder": {
                    "type": "stdio",
                    "command": "aptu-coder",
                    "directTools": True,
                },
            },
        }) + "\n")
        (agent_dir / "settings.json").write_text(
            '{"packages": ["npm:pi-mcp-adapter"]}\n')
        arm_flags = MCP_EXCLUDE
    cmd = ["pi", "-p", "--mode", "json", "--provider", PROVIDER,
           "--model", MODEL, *COMMON_FLAGS, *arm_flags,
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


def session_jsonl_files(session_dir: Path) -> list[Path]:
    """All session JSONL files pi wrote under one session dir.

    pi names session files <timestamp>_<uuid>.jsonl, not session.jsonl
    (found live in wiring smoke), so glob rather than hardcode.
    """
    if not session_dir.exists():
        return []
    return sorted(session_dir.rglob("*.jsonl"))


def _entry_cost_usd(entry: object) -> float | None:
    """Extract one entry's cost in USD, or None when absent.

    pi nests usage under the assistant message and represents cost as a
    dict of components with a numeric 'total' (a bare number is accepted
    for forward compatibility). Entries without usage are normal.
    """
    if not isinstance(entry, dict):
        return None
    usage = entry.get("usage")
    if not isinstance(usage, dict):
        message = entry.get("message")
        usage = message.get("usage") if isinstance(message, dict) else None
    if not isinstance(usage, dict):
        return None
    cost = usage.get("cost")
    if isinstance(cost, dict):
        cost = cost.get("total")
    if isinstance(cost, bool) or not isinstance(cost, (int, float)):
        return None
    return float(cost)


def parse_session_cost(session_dir: Path) -> float | None:
    """Sum usage cost across all session JSONL files in the session dir.

    Returns None on a missing or malformed usage field (fail closed).
    """
    files = session_jsonl_files(session_dir)
    if not files:
        return None
    total = 0.0
    seen = False
    for session_jsonl in files:
        for line in session_jsonl.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                return None
            cost = _entry_cost_usd(entry)
            if cost is None:
                continue
            total += cost
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
    session_dir = resolved
    full_env = {
        k: v for k, v in {**_safe_env(), **env}.items()
    }
    proc = subprocess.Popen(
        cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env=full_env,
    )
    return meter_and_close(state, stage, task, arm, run_id, session_dir, proc)


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
    ap.add_argument("--session-dir", type=Path, default=None,
                    help="pre-existing session dir to meter (no-op run)")
    args = ap.parse_args()
    run_id = args.run_id or new_run_id()
    state = LadderState()
    if not run_stage_budget_ok(state, args.stage):
        print(json.dumps({"halted": True, "reason": state.halt_reason}))
        raise SystemExit(2)
    if args.session_dir is not None:
        result = meter_and_close(
            state, args.stage, args.task, args.arm, run_id,
            args.session_dir, None,
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
