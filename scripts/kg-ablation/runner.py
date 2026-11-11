#!/usr/bin/env python3
"""Session driver for the KG pull-ablation benchmark.

Runs the 24-cell matrix (3 arms x 4 fixtures x 2 runs) via
`pi -p --mode json` with per-arm shadow agent dirs (PI_CODING_AGENT_DIR)
whose mcp.json points at the kg-ablation visibility proxy. Captures the
full pi JSON transcript per run, meters NET spend from the pi session
JSONL (fail-closed on missing usage cost, per bench_v18's live-verified
pattern, re-implemented standalone), enforces the pre-registered USD 0.069
cap, and applies the transcript-completeness stop rule.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import uuid
from pathlib import Path

from config import (
    FIXTURE_BRANCHES,
    FIXTURES,
    MODEL,
    PER_RUN_KILL_USD,
    PI_COMMON_FLAGS,
    PROVIDER,
    SESSION_TIMEOUT_S,
    SPEND_CAP_USD,
    PINNED_REPO_COMMIT,
    review_prompt,
    session_matrix,
)

KEY_RE = re.compile(r"KEY|TOKEN|SECRET|PASSWORD", re.IGNORECASE)
PROVIDER_KEY_ALLOWLIST = frozenset({"ZAI_API_KEY"})


def _safe_env() -> dict[str, str]:
    """Environment minus obvious credentials (provider key preserved)."""
    return {
        k: v for k, v in os.environ.items()
        if not (KEY_RE.search(k) and k not in PROVIDER_KEY_ALLOWLIST)
    }


def proxy_command(arm: str, log_dir: Path) -> list[str]:
    proxy = Path(__file__).resolve().parent / "proxy.py"
    cmd = [sys_executable(), str(proxy), "--arm", arm,
           "--log-dir", str(log_dir)]
    server = os.environ.get("KG_ABLATION_SERVER_CMD")
    if server:
        cmd += ["--server"] + server.split()
    return cmd


def sys_executable() -> str:
    return os.environ.get("KG_ABLATION_PYTHON", "python3")


def build_invocation(arm: str, run_root: Path, session_dir: Path,
                     prompt: str, fixture: str) -> tuple[list[str], dict]:
    """Command and env for one session, mirroring bench_v18's live-verified
    shadow-dir isolation (ambient extensions/mcp.json cannot leak)."""
    agent_dir = run_root / f"agent-{arm}"
    agent_dir.mkdir(parents=True, exist_ok=True)
    log_dir = session_dir / "mcp-logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    (agent_dir / "mcp.json").write_text(json.dumps({
        "mcpServers": {
            "aptu-coder": {
                "type": "stdio",
                "command": sys_executable(),
                "args": proxy_command(arm, log_dir)[1:],
                "directTools": True,
            },
        },
    }) + "\n")
    (agent_dir / "settings.json").write_text(
        '{"packages": ["npm:pi-mcp-adapter"]}\n')
    branch = FIXTURE_BRANCHES[fixture]
    cmd = [
        "pi", "-p", "--mode", "json",
        "--provider", PROVIDER, "--model", MODEL,
        *PI_COMMON_FLAGS,
        "--exclude-tools",
        "aptu-coder_edit_overwrite,aptu-coder_edit_replace,"
        "aptu-coder_exec_command",
        "--session-dir", str(session_dir.resolve()),
        prompt + f" The PR head branch is {branch}.",
    ]
    env = {"PI_CODING_AGENT_DIR": str(agent_dir.resolve())}
    return cmd, env


# -- NET token / spend accounting (standalone re-implementation) ---------

def session_jsonl_files(session_dir: Path) -> list[Path]:
    if not session_dir.exists():
        return []
    return sorted(session_dir.rglob("*.jsonl"))


def _entry_usage(entry: dict) -> dict | None:
    usage = entry.get("usage")
    if not isinstance(usage, dict):
        message = entry.get("message")
        usage = message.get("usage") if isinstance(message, dict) else None
    return usage if isinstance(usage, dict) else None


def parse_session(session_dir: Path) -> dict | None:
    """NET token/cost accounting for one session dir; None = fail closed
    (missing files or malformed usage cost)."""
    files = session_jsonl_files(session_dir)
    if not files:
        return None
    tokens_in = tokens_out = 0
    cost = 0.0
    seen_cost = False
    final_assistant = False
    for path in files:
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                return None
            usage = _entry_usage(entry) if isinstance(entry, dict) else None
            if usage is not None:
                tokens_in += int(usage.get("input", 0) or 0)
                tokens_out += int(usage.get("output", 0) or 0)
                token_cost = usage.get("cost")
                if isinstance(token_cost, dict):
                    token_cost = token_cost.get("total")
                if isinstance(token_cost, (int, float)) \
                        and not isinstance(token_cost, bool):
                    cost += float(token_cost)
                    seen_cost = True
            message = entry.get("message") \
                if isinstance(entry.get("message"), dict) else entry
            if isinstance(message, dict) \
                    and message.get("role") == "assistant" \
                    and message.get("stopReason") in ("end_turn", "stop",
                                                      "stop_sequence"):
                final_assistant = True
    if not seen_cost:
        return None
    return {
        "input_tokens": tokens_in,
        "output_tokens": tokens_out,
        "cost_usd": round(cost, 6),
        "complete": final_assistant,
    }


# -- benchmark state ------------------------------------------------------

class BenchmarkState:
    def __init__(self) -> None:
        self.total_spent_usd = 0.0
        self.halted = False
        self.halt_reason: str | None = None

    def meter(self, accounting: dict, run: dict) -> None:
        self.total_spent_usd += accounting["cost_usd"]
        if accounting["cost_usd"] > PER_RUN_KILL_USD:
            run["defect"] = f"per-run-cap-exceeded:{accounting['cost_usd']:.4f}"
        if self.total_spent_usd > SPEND_CAP_USD:
            self.halted = True
            self.halt_reason = "spend-cap-exceeded"


def run_cell(state: BenchmarkState, run_root: Path, repo_root: Path,
             fixture_prs: dict[str, int], arm: str, fixture: str,
             run_no: int, cache_state: str) -> dict:
    run_id = uuid.uuid4().hex
    session_dir = run_root / "sessions" / f"{arm}-{fixture}-run{run_no}" / run_id
    session_dir.mkdir(parents=True, exist_ok=True)
    pr = fixture_prs[fixture]
    prompt = review_prompt(pr, arm)
    cmd, env = build_invocation(arm, run_root, session_dir, prompt, fixture)
    transcript_path = session_dir / "transcript.jsonl"
    run: dict = {
        "run_id": run_id, "arm": arm, "fixture": fixture, "run": run_no,
        "cache_state": cache_state, "provider": PROVIDER, "model": MODEL,
        "pinned_repo_commit": PINNED_REPO_COMMIT,
        "fixture_branch": FIXTURE_BRANCHES[fixture], "fixture_pr": pr,
        "timeout_s": SESSION_TIMEOUT_S,
        "transcript": str(transcript_path),
    }
    with open(transcript_path, "wb") as out:
        proc = subprocess.Popen(cmd, stdout=out, stderr=subprocess.DEVNULL,
                                cwd=str(repo_root),
                                env={**_safe_env(), **env})
        try:
            proc.wait(timeout=SESSION_TIMEOUT_S)
            run["exit_code"] = proc.returncode
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
            run["exit_code"] = None
            run["defect"] = f"session-timeout:{SESSION_TIMEOUT_S}s"
    accounting = parse_session(session_dir)
    if accounting is None:
        run["defect"] = run.get("defect") or \
            "missing-or-malformed-usage-cost"
        run["complete"] = False
        run["cost_usd"] = None
    else:
        run.update(accounting)
        state.meter(accounting, run)
    return run


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--run-root", type=Path, required=True)
    ap.add_argument("--repo-root", type=Path, required=True)
    ap.add_argument("--fixture-prs", required=True,
                    help="JSON object fixture->PR number")
    ap.add_argument("--arm", choices=["no-graph", "optional-graph",
                                      "forced-graph", None],
                    default=None)
    ap.add_argument("--fixture", default=None)
    ap.add_argument("--manifest", type=Path, default=None)
    args = ap.parse_args()

    prs: dict[str, int] = {
        k: int(v) for k, v in json.loads(args.fixture_prs).items()
    }
    args.run_root.mkdir(parents=True, exist_ok=True)
    state = BenchmarkState()
    runs = []
    for arm, fixture, run_no in session_matrix():
        if args.arm and arm != args.arm:
            continue
        if args.fixture and fixture != args.fixture:
            continue
        cache_state = "cold" if run_no == 1 else "warm"
        run = run_cell(state, args.run_root, args.repo_root, prs,
                       arm, fixture, run_no, cache_state)
        runs.append(run)
        print(json.dumps(run), flush=True)
        if state.halted:
            print(json.dumps({
                "halted": True, "reason": state.halt_reason,
                "total_spent_usd": round(state.total_spent_usd, 6),
            }), flush=True)
            break
    manifest_path = args.manifest or (args.run_root / "manifest.jsonl")
    with open(manifest_path, "a", encoding="utf-8") as fh:
        for run in runs:
            fh.write(json.dumps(run) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
