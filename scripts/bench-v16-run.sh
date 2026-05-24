#!/usr/bin/env bash
# v16 Benchmark Runner
# Runs MCP-full conditions (A and C) only. Native conditions (B/D) are reused from v13.
# Model and tool allowlist are declared in agent frontmatter (prompts/condition-*.md);
# the runner expands repo-path/run-id placeholders into a temp agent file and passes
# it via --agent. No --model or --allowedTools flags needed.
#
# Usage:
#   bash scripts/bench-v16-run.sh <CONDITION_ID> <RUN_ID>
#
# Examples:
#   bash scripts/bench-v16-run.sh A A-pilot
#   bash scripts/bench-v16-run.sh C C-scored-1
#   bash scripts/bench-v16-run.sh A A-scored-2
#
# Environment variables:
#   BENCH_MAX_BUDGET_USD  -- cap spend per run (optional, e.g. "2.00")
#   OPENFAST_REPO         -- local path to openfast clone (default: /tmp/openfast-benchmark)

set -euo pipefail

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------
if ! command -v claude >/dev/null 2>&1; then
  echo "ERROR: 'claude' CLI not found. Install Claude Code and authenticate before running." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNS_DIR="$REPO_ROOT/docs/benchmarks/v16/results/runs"
PROMPTS_DIR="$REPO_ROOT/docs/benchmarks/v16/prompts"
MCP_CONFIG="$REPO_ROOT/docs/benchmarks/v16/mcp-aptu-coder-full.json"

mkdir -p "$RUNS_DIR"

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <CONDITION_ID> <RUN_ID>" >&2
  echo "CONDITION_ID: A or C" >&2
  echo "RUN_ID: e.g. A-pilot, C-scored-1" >&2
  exit 1
fi

CONDITION_ID="$1"
RUN_ID="$2"

if [[ ! "$CONDITION_ID" =~ ^[AC]$ ]]; then
  echo "ERROR: CONDITION_ID must be A or C (native conditions B/D are reused from v13)" >&2
  exit 1
fi

if [[ ! "$RUN_ID" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "ERROR: RUN_ID must contain only alphanumeric characters, dots, underscores, and hyphens" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# OpenFAST repo setup
# ---------------------------------------------------------------------------
OPENFAST_REPO="${OPENFAST_REPO:-/tmp/openfast-benchmark}"
OPENFAST_COMMIT="2895884d2be01862173c88d70f86b358d2f1a50a"

if [[ -d "$OPENFAST_REPO" ]] && { find "$OPENFAST_REPO" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null | grep -q .; }; then
  if ! git -C "$OPENFAST_REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "ERROR: OPENFAST_REPO ('$OPENFAST_REPO') exists but is not a git repository." >&2
    echo "       Remove the directory or set OPENFAST_REPO to an empty/absent path." >&2
    exit 1
  fi
  REMOTE_URL=$(git -C "$OPENFAST_REPO" remote get-url origin 2>/dev/null || echo "")
  REMOTE_URL_LOWER=$(echo "$REMOTE_URL" | tr '[:upper:]' '[:lower:]')
  if [[ -z "$REMOTE_URL" ]]; then
    echo "WARNING: OPENFAST_REPO has no origin remote. Proceeding with local-only repo." >&2
  elif [[ "$REMOTE_URL_LOWER" != *openfast* ]]; then
    echo "ERROR: OPENFAST_REPO remote URL ('$REMOTE_URL') does not contain 'openfast'." >&2
    echo "       This does not appear to be the OpenFAST repository." >&2
    exit 1
  fi
elif [[ ! -d "$OPENFAST_REPO/modules/aerodyn" ]]; then
  echo "Cloning OpenFAST (shallow) into $OPENFAST_REPO ..."
  git clone --depth=1 https://github.com/OpenFAST/openfast.git "$OPENFAST_REPO"
fi

if ! git -C "$OPENFAST_REPO" rev-parse --verify "${OPENFAST_COMMIT}^{commit}" >/dev/null 2>&1; then
  echo "Fetching pinned OpenFAST commit $OPENFAST_COMMIT ..." >&2
  if ! git -C "$OPENFAST_REPO" fetch --depth=1 origin "$OPENFAST_COMMIT" 2>/dev/null; then
    if [[ "$RUN_ID" == *scored* ]]; then
      echo "ERROR: Failed to fetch pinned commit $OPENFAST_COMMIT for scored run $RUN_ID." >&2
      exit 1
    else
      echo "WARNING: Failed to fetch pinned commit $OPENFAST_COMMIT; proceeding with existing clone." >&2
    fi
  fi
fi

if git -C "$OPENFAST_REPO" rev-parse --verify "${OPENFAST_COMMIT}^{commit}" >/dev/null 2>&1; then
  git -C "$OPENFAST_REPO" -c advice.detachedHead=false checkout "$OPENFAST_COMMIT" >/dev/null 2>&1 || true
fi

ACTUAL_COMMIT=$(git -C "$OPENFAST_REPO" rev-parse HEAD)
if [[ "$ACTUAL_COMMIT" != "$OPENFAST_COMMIT" ]]; then
  if [[ "$RUN_ID" == *scored* ]]; then
    echo "ERROR: OpenFAST HEAD is $ACTUAL_COMMIT, expected $OPENFAST_COMMIT." >&2
    echo "       Scored runs require the pinned commit for reproducibility." >&2
    exit 1
  else
    echo "WARNING: OpenFAST HEAD is $ACTUAL_COMMIT, expected $OPENFAST_COMMIT." >&2
    echo "         Pilot runs may proceed; scored runs must use the pinned commit." >&2
  fi
fi

# ---------------------------------------------------------------------------
# Condition dispatch
# ---------------------------------------------------------------------------
case "$CONDITION_ID" in
  A) AGENT_SOURCE="$PROMPTS_DIR/condition-a-mcp-sonnet.md" ;;
  C) AGENT_SOURCE="$PROMPTS_DIR/condition-c-mcp-haiku.md" ;;
esac

# ---------------------------------------------------------------------------
# Output files
# ---------------------------------------------------------------------------
OUTPUT_FILE="$RUNS_DIR/${RUN_ID}-report.json"
TELEMETRY_FILE="$RUNS_DIR/${RUN_ID}-telemetry.json"
LOG_FILE="$RUNS_DIR/${RUN_ID}.log"
SCRATCH_FILE=$(mktemp /tmp/bench-v16-XXXXXX.json)
# Expand repo path and run-id placeholders into a temp agent file
AGENT_FILE=$(mktemp /tmp/bench-v16-agent-XXXXXX.md)
sed \
  -e "s|<repo>|$OPENFAST_REPO|g" \
  -e "s|REPO_PATH_PLACEHOLDER|$OPENFAST_REPO|g" \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$CONDITION_ID|g" \
  "$AGENT_SOURCE" > "$AGENT_FILE"
trap 'rm -f "$SCRATCH_FILE" "$AGENT_FILE"' EXIT

# ---------------------------------------------------------------------------
# MCP config flag (server registration only; tool allowlist lives in agent frontmatter)
# ---------------------------------------------------------------------------
MCP_FLAGS="--mcp-config $MCP_CONFIG --strict-mcp-config"

# ---------------------------------------------------------------------------
# Output schema (identical to v13)
# ---------------------------------------------------------------------------
OUTPUT_SCHEMA=$(cat <<'SCHEMA'
{
  "type": "object",
  "properties": {
    "run_id":               { "type": "string" },
    "condition":            { "type": "string" },
    "aerodyn_entry_points": { "type": "array", "items": { "type": "object" } },
    "nwtc_callees":         { "type": "array", "items": { "type": "object" } },
    "nwtc_types_used":      { "type": "array", "items": { "type": "object" } },
    "integration_map":      { "type": "array", "items": { "type": "object" } },
    "tool_calls_total":     { "type": "integer" }
  },
  "required": [
    "run_id",
    "condition",
    "aerodyn_entry_points",
    "nwtc_callees",
    "nwtc_types_used",
    "integration_map",
    "tool_calls_total"
  ]
}
SCHEMA
)

# Task user message: repo path is the only runtime context needed (agent file has the rest)
TASK_CONTENT=$(sed \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$CONDITION_ID|g" \
  "$PROMPTS_DIR/task.md")

TASK_CONTENT="$TASK_CONTENT

Repository is cloned at: $OPENFAST_REPO
All tool paths must use this absolute prefix."

# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------
cat <<EOF
=== v16 Benchmark Run ===
CONDITION:   $CONDITION_ID
RUN_ID:      $RUN_ID
AGENT:       $AGENT_FILE
OPENFAST:    $OPENFAST_REPO ($ACTUAL_COMMIT)
BUDGET:      ${BENCH_MAX_BUDGET_USD:-unlimited} USD
OUTPUT:      $OUTPUT_FILE
TELEMETRY:   $TELEMETRY_FILE
EOF

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
echo "Starting run at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
touch /tmp/.v16-run-marker

BUDGET_FLAG=()
if [[ -n "${BENCH_MAX_BUDGET_USD:-}" ]]; then
  BUDGET_FLAG=(--max-budget-usd "$BENCH_MAX_BUDGET_USD")
fi

# --agent supplies model + tool allowlist via frontmatter; no --model/--allowedTools needed.
# --strict-mcp-config ensures only the aptu-coder server is loaded regardless of user config.
DISABLE_PROMPT_CACHING=1 claude \
  -p \
  --agent "$AGENT_FILE" \
  $MCP_FLAGS \
  --dangerously-skip-permissions \
  --output-format json \
  --json-schema "$OUTPUT_SCHEMA" \
  ${BUDGET_FLAG:+"${BUDGET_FLAG[@]}"} \
  "$TASK_CONTENT" \
  > "$SCRATCH_FILE" \
  2> "$LOG_FILE"

echo "Run completed at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ---------------------------------------------------------------------------
# Extract report and telemetry
# ---------------------------------------------------------------------------
python3 - "$SCRATCH_FILE" "$OUTPUT_FILE" "$TELEMETRY_FILE" << 'PYEOF'
import json, sys

scratch, out_path, tel_path = sys.argv[1], sys.argv[2], sys.argv[3]

with open(scratch) as f:
    content = f.read().strip()

if not content:
    print("ERROR: output file is empty", file=sys.stderr)
    sys.exit(1)

try:
    messages = json.loads(content)
    if not isinstance(messages, list):
        messages = [messages]
except json.JSONDecodeError as e:
    print(f"ERROR: could not parse output as JSON: {e}", file=sys.stderr)
    sys.exit(1)

result = next((m for m in messages if isinstance(m, dict) and m.get("type") == "result"), None)
if result is None:
    print("ERROR: no result message found in output", file=sys.stderr)
    sys.exit(1)

structured = result.get("structured_output")
if structured is None:
    print("ERROR: structured_output is null or missing", file=sys.stderr)
    sys.exit(1)

with open(out_path, "w") as f:
    json.dump(structured, f, indent=2)

usage = result.get("usage") or {}
if not isinstance(usage, dict):
    usage = {}
telemetry = {
    "wall_time_ms":          result.get("duration_ms"),
    "api_time_ms":           result.get("duration_api_ms"),
    "num_turns":             result.get("num_turns"),
    "cost_usd":              result.get("total_cost_usd"),
    "input_tokens":          usage.get("input_tokens"),
    "output_tokens":         usage.get("output_tokens"),
    "cache_read_tokens":     usage.get("cache_read_input_tokens"),
    "cache_creation_tokens": usage.get("cache_creation_input_tokens"),
}
with open(tel_path, "w") as f:
    json.dump(telemetry, f, indent=2)

print(f"Report:    {out_path}")
print(f"Telemetry: {tel_path}")
PYEOF

# ---------------------------------------------------------------------------
# Tool isolation validation
# ---------------------------------------------------------------------------
# Claude Code writes session JSONL to ~/.claude/projects/<slug>/ where <slug> is the
# working directory path with slashes replaced by hyphens. Override with CLAUDE_SESSION_DIR
# if your installation uses a different layout.
_REPO_SLUG="${REPO_ROOT//\//-}"
SESSION_DIR="${CLAUDE_SESSION_DIR:-$HOME/.claude/projects/${_REPO_SLUG}}"

_sessions=()
while IFS= read -r f; do _sessions+=("$f"); done < <(find "$SESSION_DIR" -name "*.jsonl" -newer /tmp/.v16-run-marker 2>/dev/null || true)
if [[ ${#_sessions[@]} -gt 0 ]]; then
  LATEST_SESSION=$(ls -t "${_sessions[@]}" 2>/dev/null | head -1)
  SESSION_COPY="$RUNS_DIR/${RUN_ID}-session.jsonl"
  cp "$LATEST_SESSION" "$SESSION_COPY"
  echo "Session JSONL: $SESSION_COPY"

  python3 - "$SESSION_COPY" << 'PYEOF'
import json, sys

session_file = sys.argv[1]

MCP_TOOLS = {
    "mcp__aptu-coder__analyze_directory",
    "mcp__aptu-coder__analyze_file",
    "mcp__aptu-coder__analyze_symbol",
    "mcp__aptu-coder__analyze_module",
    "mcp__aptu-coder__exec_command",
}
NATIVE_TOOLS = {"Bash", "Glob", "Grep", "Read", "Write", "ToolSearch"}

tools_used = set()
with open(session_file) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if entry.get("type") == "assistant":
            for block in entry.get("message", {}).get("content", []):
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tools_used.add(block["name"])

print(f"Tools used: {sorted(tools_used)}")

forbidden = tools_used & NATIVE_TOOLS
if forbidden:
    print(f"ISOLATION FAIL: native tools used in MCP condition: {forbidden}", file=sys.stderr)
    sys.exit(1)
print(f"MCP tools used: {sorted(tools_used & MCP_TOOLS)}")
print("ISOLATION PASS")
PYEOF
else
  echo "WARNING: could not find session JSONL for isolation validation" >&2
fi

# ---------------------------------------------------------------------------
# Final summary
# ---------------------------------------------------------------------------
echo ""
echo "=== Run complete ==="
if [[ -f "$OUTPUT_FILE" ]]; then
  echo "Report:    $OUTPUT_FILE"
  python3 -c "
import json
d = json.load(open('$OUTPUT_FILE'))
ep  = len(d.get('aerodyn_entry_points', []))
cal = len(d.get('nwtc_callees', []))
typ = len(d.get('nwtc_types_used', []))
imp = len(d.get('integration_map', []))
tc  = d.get('tool_calls_total', '?')
print(f'  entry_points={ep}  nwtc_callees={cal}  types={typ}  integration_map={imp}  tool_calls={tc}')
"
fi
if [[ -f "$TELEMETRY_FILE" ]]; then
  echo "Telemetry: $TELEMETRY_FILE"
  python3 -c "
import json
t = json.load(open('$TELEMETRY_FILE'))
print(f'  turns={t.get(\"num_turns\",\"?\")}  cost_usd={t.get(\"cost_usd\",\"?\")}  input_tokens={t.get(\"input_tokens\",\"?\")}')
"
fi
if [[ -s "$LOG_FILE" ]]; then
  echo "Log:       $LOG_FILE"
fi
