#!/usr/bin/env bash
# v17 Benchmark Runner
# Faithful re-run of the v12 Django auth migration benchmark, corrected for
# aptu-coder drift (MCP tool prefix rename, page_size removal).
# Parameterized by condition ID (A, B, C, D) and run ID.
# Condition A/B use claude-sonnet-4-6, C/D use claude-haiku-4-5.
# A/C use MCP tools, B/D use native tools.
# Validates tool isolation from session JSONL.
#
# Improvements inherited from the v13 runner:
#   - Django commit pinning with checkout before every run
#   - BENCH_MAX_BUDGET_USD cap enforcement (--max-budget-usd)
#   - Session transcript archival into results/ after each run
#
# Isolation flags: --settings '{"disableAllHooks":true}' and
# --setting-sources "project,local". CLAUDE_CONFIG_DIR isolation is NOT used
# (it breaks keychain auth).
#
# Usage:
#   bash scripts/bench-v17-run.sh <CONDITION_ID> <RUN_ID>
#
# Environment variables:
#   BENCH_MAX_BUDGET_USD -- cap spend per run (optional, e.g. "2.00")
#   DJANGO_REPO          -- local path to django clone
#                           (default: /tmp/benchmark-repos/django)

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNS_DIR="$REPO_ROOT/docs/benchmarks/v17/results/runs"
PROMPTS_DIR="$REPO_ROOT/docs/benchmarks/v17/prompts"
MCP_APTU_CODER_CONFIG="$REPO_ROOT/docs/benchmarks/v17/mcp-aptu-coder-only.json"

mkdir -p "$RUNS_DIR"

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <CONDITION_ID> <RUN_ID>" >&2
  echo "CONDITION_ID: A, B, C, or D" >&2
  echo "RUN_ID: e.g. A-pilot-1, B-scored-2" >&2
  exit 1
fi

CONDITION_ID="$1"
RUN_ID="$2"

if [[ ! "$CONDITION_ID" =~ ^[ABCD]$ ]]; then
  echo "ERROR: CONDITION_ID must be A, B, C, or D" >&2
  exit 1
fi

if [[ ! "$RUN_ID" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "ERROR: RUN_ID must contain only alphanumeric characters, dots, underscores, and hyphens" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Django repo setup (commit pinning, inherited from v13)
# ---------------------------------------------------------------------------
DJANGO_REPO="${DJANGO_REPO:-/tmp/benchmark-repos/django}"
DJANGO_COMMIT="6b90f8a8d6994dc62cd91dde911fe56ec3389494"

if [[ ! -d "$DJANGO_REPO" ]]; then
  echo "Cloning django (full clone required for pinned commit) into $DJANGO_REPO ..."
  mkdir -p "$(dirname "$DJANGO_REPO")"
  git clone https://github.com/django/django.git "$DJANGO_REPO"
fi

if ! git -C "$DJANGO_REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "ERROR: DJANGO_REPO ('$DJANGO_REPO') is not a git repository." >&2
  exit 1
fi

# Checkout pinned commit before every run
if ! git -C "$DJANGO_REPO" rev-parse --verify "${DJANGO_COMMIT}^{commit}" >/dev/null 2>&1; then
  echo "Fetching pinned Django commit $DJANGO_COMMIT ..." >&2
  git -C "$DJANGO_REPO" fetch origin "$DJANGO_COMMIT" || {
    if [[ "$RUN_ID" == *scored* ]]; then
      echo "ERROR: Failed to fetch pinned commit $DJANGO_COMMIT for scored run $RUN_ID." >&2
      exit 1
    else
      echo "WARNING: Failed to fetch pinned commit $DJANGO_COMMIT; proceeding with existing clone." >&2
    fi
  }
fi

git -C "$DJANGO_REPO" -c advice.detachedHead=false checkout "$DJANGO_COMMIT" >/dev/null 2>&1 || true

ACTUAL_COMMIT=$(git -C "$DJANGO_REPO" rev-parse HEAD)
if [[ "$ACTUAL_COMMIT" != "$DJANGO_COMMIT" ]]; then
  if [[ "$RUN_ID" == *scored* ]]; then
    echo "ERROR: Django HEAD is $ACTUAL_COMMIT, expected $DJANGO_COMMIT." >&2
    echo "       Scored runs require the pinned commit for reproducibility." >&2
    exit 1
  else
    echo "WARNING: Django HEAD is $ACTUAL_COMMIT, expected $DJANGO_COMMIT." >&2
    echo "         Pilot runs may proceed; scored runs must use the pinned commit." >&2
  fi
fi

# ---------------------------------------------------------------------------
# Condition dispatch
# ---------------------------------------------------------------------------
case "$CONDITION_ID" in
  A)
    MODEL="claude-sonnet-4-6"
    TOOL_SET="mcp"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-a-mcp-sonnet.md"
    ;;
  B)
    MODEL="claude-sonnet-4-6"
    TOOL_SET="native"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-b-native-sonnet.md"
    ;;
  C)
    MODEL="claude-haiku-4-5"
    TOOL_SET="mcp"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-c-mcp-haiku.md"
    ;;
  D)
    MODEL="claude-haiku-4-5"
    TOOL_SET="native"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-d-native-haiku.md"
    ;;
esac

# ---------------------------------------------------------------------------
# Output files
# ---------------------------------------------------------------------------
OUTPUT_FILE="$RUNS_DIR/${RUN_ID}-report.json"
TELEMETRY_FILE="$RUNS_DIR/${RUN_ID}-telemetry.json"
JSONL_FILE="/tmp/bench-v17-${RUN_ID}.json"
LOG_FILE="$RUNS_DIR/${RUN_ID}.log"

# ---------------------------------------------------------------------------
# Tool isolation flags
# ---------------------------------------------------------------------------
MCP_TOOLS="mcp__aptu-coder__analyze_directory,mcp__aptu-coder__analyze_file,mcp__aptu-coder__analyze_symbol,mcp__aptu-coder__analyze_module"
NATIVE_TOOLS="Bash,Glob,Grep,Read,Write,ToolSearch"

if [[ "$TOOL_SET" == "mcp" ]]; then
  ALLOWED_TOOLS="$MCP_TOOLS"
  # ToolSearch is a built-in native tool that post-dates v12; the models invoke
  # it voluntarily to discover MCP tools, which trips tool-isolation validation.
  # exec_command/edit_* are aptu-coder MCP tools outside the v12 four-tool
  # allowlist (shell/file escape). Disallow all of them explicitly in MCP
  # conditions so isolation holds.
  DISALLOWED_TOOLS="ToolSearch,mcp__aptu-coder__exec_command,mcp__aptu-coder__edit_overwrite,mcp__aptu-coder__edit_replace,WebFetch,WebSearch,Task,TodoWrite"
  MCP_FLAGS="--mcp-config $MCP_APTU_CODER_CONFIG --strict-mcp-config --disallowedTools $DISALLOWED_TOOLS"
  trap 'rm -f "$JSONL_FILE"' EXIT
else
  ALLOWED_TOOLS="$NATIVE_TOOLS"
  EMPTY_MCP_CONFIG=$(mktemp /tmp/bench-v17-empty-mcp.XXXXXX.json)
  echo '{"mcpServers":{}}' > "$EMPTY_MCP_CONFIG"
  # Block built-ins outside the v12 native allowlist (haiku voluntarily invoked
  # WebFetch in D conditions); same list applies to MCP conditions above.
  DISALLOWED_TOOLS="WebFetch,WebSearch,Task,TodoWrite"
  MCP_FLAGS="--mcp-config $EMPTY_MCP_CONFIG --strict-mcp-config --disallowedTools $DISALLOWED_TOOLS"
  trap 'rm -f "$EMPTY_MCP_CONFIG" "${JSONL_FILE:-}"' EXIT
fi

# ---------------------------------------------------------------------------
# Output schema
# ---------------------------------------------------------------------------
OUTPUT_SCHEMA='{"type":"object","properties":{"run_id":{"type":"string"},"condition":{"type":"string"},"auth_module_map":{"type":"array","items":{"type":"object"}},"migration_trace":{"type":"array","items":{"type":"string"}},"unmappable_fields":{"type":"array","items":{"type":"object","properties":{"field":{"type":"string"},"reason":{"type":"string"},"migration_strategy":{"type":"string"},"evidence":{"type":"string"}},"required":["field","reason","migration_strategy","evidence"]}},"tool_calls_total":{"type":"integer"}},"required":["run_id","condition","auth_module_map","migration_trace","unmappable_fields","tool_calls_total"]}'

# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------
cat <<EOF
=== v17 Benchmark Run ===
CONDITION: $CONDITION_ID
RUN_ID:    $RUN_ID
MODEL:     $MODEL
TOOL_SET:  $TOOL_SET
ALLOWED:   $ALLOWED_TOOLS
DJANGO:    $DJANGO_REPO ($ACTUAL_COMMIT)
BUDGET:    ${BENCH_MAX_BUDGET_USD:-unlimited} USD
OUTPUT:    $OUTPUT_FILE
EOF

# ---------------------------------------------------------------------------
# Build prompts (substitute placeholders)
# ---------------------------------------------------------------------------
SYSTEM_PROMPT=$(sed \
  -e "s|TARGET_REPO_PATH|$DJANGO_REPO|g" \
  -e "s|OUTPUT_PATH|$OUTPUT_FILE|g" \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  "$SYSTEM_PROMPT_FILE")
TASK_CONTENT=$(sed \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$CONDITION_ID|g" \
  "$PROMPTS_DIR/task.md")

# ---------------------------------------------------------------------------
# Tool isolation validation function
# ---------------------------------------------------------------------------
validate_tool_isolation() {
  local session_file="$1"
  local expected_tool_set="$2"  # "mcp" or "native"

  python3 - "$session_file" "$expected_tool_set" << 'PYEOF'
import json, sys

session_file = sys.argv[1]
expected_tool_set = sys.argv[2]

MCP_TOOLS = {
    "mcp__aptu-coder__analyze_directory",
    "mcp__aptu-coder__analyze_file",
    "mcp__aptu-coder__analyze_symbol",
    "mcp__aptu-coder__analyze_module",
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

# StructuredOutput is the CLI's response-formatting tool, not an agent tool.
PERMITTED_META = {"StructuredOutput"}

if expected_tool_set == "mcp":
    forbidden_used = (tools_used - MCP_TOOLS) - PERMITTED_META
    if forbidden_used:
        print(f"ISOLATION FAIL: non-allowlisted tools used in MCP condition: {forbidden_used}", file=sys.stderr)
        sys.exit(1)
    print(f"MCP tools used: {sorted(tools_used & MCP_TOOLS)}")
    print("ISOLATION PASS: only allowlisted MCP tools used")
else:
    forbidden_used = (tools_used - NATIVE_TOOLS) - PERMITTED_META
    if forbidden_used:
        print(f"ISOLATION FAIL: non-allowlisted tools used in native condition: {forbidden_used}", file=sys.stderr)
        sys.exit(1)
    print(f"Native tools used: {sorted(tools_used & NATIVE_TOOLS)}")
    print("ISOLATION PASS: only allowlisted native tools used")
PYEOF
}

# ---------------------------------------------------------------------------
# Session capture setup
# ---------------------------------------------------------------------------
touch /tmp/.v17-run-marker
# Claude slugifies the project path by replacing every non-alphanumeric
# character (both "/" and ".") with "-".
_REPO_SLUG=$(printf '%s' "$REPO_ROOT" | sed 's|[^A-Za-z0-9]|-|g')
SESSION_DIR="${CLAUDE_SESSION_DIR:-$HOME/.claude/projects/${_REPO_SLUG}}"

# ---------------------------------------------------------------------------
# Claude invocation (with isolation flags)
# ---------------------------------------------------------------------------
echo "Starting run at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

BUDGET_FLAG=()
if [[ -n "${BENCH_MAX_BUDGET_USD:-}" ]]; then
  BUDGET_FLAG=(--max-budget-usd "$BENCH_MAX_BUDGET_USD")
fi

DISABLE_PROMPT_CACHING=1 claude \
  -p \
  --model "$MODEL" \
  --system-prompt "$SYSTEM_PROMPT" \
  $MCP_FLAGS \
  --allowedTools "$ALLOWED_TOOLS" \
  --settings '{"disableAllHooks":true}' \
  --setting-sources "project,local" \
  --dangerously-skip-permissions \
  --output-format json \
  --json-schema "$OUTPUT_SCHEMA" \
  ${BUDGET_FLAG:+"${BUDGET_FLAG[@]}"} \
  "$TASK_CONTENT" \
  > "$JSONL_FILE" \
  2> "$LOG_FILE"
echo "Run completed at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ---------------------------------------------------------------------------
# Extract structured output and telemetry
# ---------------------------------------------------------------------------
python3 - "$JSONL_FILE" "$OUTPUT_FILE" "$TELEMETRY_FILE" << 'PYEOF'
import json, sys

jsonl_file, output_file, telemetry_file = sys.argv[1], sys.argv[2], sys.argv[3]

with open(jsonl_file) as f:
    content = f.read().strip()

if not content:
    print("ERROR: JSONL file is empty", file=sys.stderr)
    sys.exit(1)

try:
    messages = json.loads(content)
    if not isinstance(messages, list):
        messages = [messages]
except json.JSONDecodeError as e:
    print(f"ERROR: could not parse JSONL file as JSON: {e}", file=sys.stderr)
    sys.exit(1)

result = next((m for m in messages if isinstance(m, dict) and m.get("type") == "result"), None)
if result is None:
    print("ERROR: no result message found in JSON output", file=sys.stderr)
    sys.exit(1)

structured_output = result.get("structured_output")
if structured_output is None:
    print("ERROR: structured_output is null or missing in result message", file=sys.stderr)
    sys.exit(1)

with open(output_file, "w") as f:
    json.dump(structured_output, f, indent=2)

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
with open(telemetry_file, "w") as f:
    json.dump(telemetry, f, indent=2)

print(f"Report:    {output_file}")
print(f"Telemetry: {telemetry_file}")
PYEOF

# ---------------------------------------------------------------------------
# Transcript archival and tool isolation validation
# ---------------------------------------------------------------------------
if [[ -d "$SESSION_DIR" ]]; then
  # Portable: avoid mapfile (bash 3 compat)
  _sessions=()
  while IFS= read -r f; do _sessions+=("$f"); done < <(find "$SESSION_DIR" -name "*.jsonl" -newer /tmp/.v17-run-marker 2>/dev/null || true)
  if [[ ${#_sessions[@]} -gt 0 ]]; then
    LATEST_SESSION=$(ls -t "${_sessions[@]}" 2>/dev/null | head -1)
    SESSION_COPY="$RUNS_DIR/${RUN_ID}-session.jsonl"
    cp "$LATEST_SESSION" "$SESSION_COPY"
    echo "Session JSONL: $SESSION_COPY"
    validate_tool_isolation "$SESSION_COPY" "$TOOL_SET"
  else
    echo "WARNING: Could not find session JSONL for archival" >&2
  fi
else
  echo "WARNING: Session directory not found: $SESSION_DIR" >&2
fi

# ---------------------------------------------------------------------------
# Output validation
# ---------------------------------------------------------------------------
if [[ -f "$OUTPUT_FILE" ]]; then
  echo "Report file: $OUTPUT_FILE"
  if python3 -c "import json,sys; json.load(open('$OUTPUT_FILE'))" 2>/dev/null; then
    echo "Output: VALID JSON"
  else
    echo "Output: INVALID JSON" >&2
  fi
else
  echo "WARNING: Report file not found at $OUTPUT_FILE" >&2
  echo "Check $LOG_FILE for agent output" >&2
fi

if [[ -f "$TELEMETRY_FILE" ]]; then
  echo "Telemetry file: $TELEMETRY_FILE"
  if python3 -c "import json,sys; json.load(open('$TELEMETRY_FILE'))" 2>/dev/null; then
    echo "Telemetry: VALID JSON"
  else
    echo "Telemetry: INVALID JSON" >&2
  fi
else
  echo "WARNING: Telemetry file not found at $TELEMETRY_FILE" >&2
fi
