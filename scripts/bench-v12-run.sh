#!/usr/bin/env bash
# v12 Benchmark Runner
# Parameterized by condition ID (A, B, C, D) and run ID.
# Condition A/B use claude-sonnet-4-6, C/D use claude-haiku-4-5.
# A/C use MCP tools, B/D use native tools.
# Validates tool isolation from session JSONL.

set -euo pipefail

# Derive paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNS_DIR="$REPO_ROOT/docs/benchmarks/v12/results/runs"
PROMPTS_DIR="$REPO_ROOT/docs/benchmarks/v12/prompts"
MCP_APTU_CODER_CONFIG="$REPO_ROOT/docs/benchmarks/v12/mcp-aptu-coder-only.json"

mkdir -p "$RUNS_DIR"

# Parse arguments
if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <CONDITION_ID> <RUN_ID>" >&2
  echo "CONDITION_ID: A, B, C, or D" >&2
  echo "RUN_ID: e.g. A-pilot-1, B-scored-2" >&2
  exit 1
fi

CONDITION_ID="$1"
RUN_ID="$2"

# Validate condition
if [[ ! "$CONDITION_ID" =~ ^[ABCD]$ ]]; then
  echo "ERROR: CONDITION_ID must be A, B, C, or D" >&2
  exit 1
fi

# Validate RUN_ID (safe filename characters only)
if [[ ! "$RUN_ID" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "ERROR: RUN_ID must contain only alphanumeric characters, dots, underscores, and hyphens" >&2
  exit 1
fi

# Dispatch condition to model and tool set
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

# Output files
OUTPUT_FILE="$RUNS_DIR/${RUN_ID}-report.json"
TELEMETRY_FILE="$RUNS_DIR/${RUN_ID}-telemetry.json"
JSONL_FILE="/tmp/bench-v12-${RUN_ID}.json"
LOG_FILE="$RUNS_DIR/${RUN_ID}.log"

# Tool isolation flags
if [[ "$TOOL_SET" == "mcp" ]]; then
  ALLOWED_TOOLS="mcp__aptu-coder__analyze_directory,mcp__aptu-coder__analyze_file,mcp__aptu-coder__analyze_module"
  MCP_FLAGS="--mcp-config $MCP_APTU_CODER_CONFIG --strict-mcp-config"
  trap 'rm -f "$JSONL_FILE"' EXIT
else
  ALLOWED_TOOLS="Bash,Glob,Grep,Read,Write,ToolSearch"
  EMPTY_MCP_CONFIG=$(mktemp /tmp/bench-v12-empty-mcp.XXXXXX.json)
  echo '{"mcpServers":{}}' > "$EMPTY_MCP_CONFIG"
  MCP_FLAGS="--mcp-config $EMPTY_MCP_CONFIG --strict-mcp-config"
  trap 'rm -f "$EMPTY_MCP_CONFIG" "${JSONL_FILE:-}"' EXIT
fi

MAX_BUDGET_USD="${BENCH_MAX_BUDGET_USD:-}"

# Define output schema for structured JSON
OUTPUT_SCHEMA='{"type":"object","properties":{"run_id":{"type":"string"},"condition":{"type":"string"},"auth_module_map":{"type":"array","items":{"type":"object"}},"migration_trace":{"type":"array","items":{"type":"string"}},"unmappable_fields":{"type":"array","items":{"type":"object","properties":{"field":{"type":"string"},"reason":{"type":"string"},"migration_strategy":{"type":"string"},"evidence":{"type":"string"}},"required":["field","reason","migration_strategy","evidence"]}},"tool_calls_total":{"type":"integer"}},"required":["run_id","condition","auth_module_map","migration_trace","unmappable_fields","tool_calls_total"]}'

# Print header
cat <<EOF
=== v12 Benchmark Run ===
CONDITION: $CONDITION_ID
RUN_ID:    $RUN_ID
MODEL:     $MODEL
TOOL_SET:  $TOOL_SET
ALLOWED:   $ALLOWED_TOOLS
BUDGET:    ${BENCH_MAX_BUDGET_USD:-unlimited} USD
OUTPUT:    $OUTPUT_FILE
EOF

# Read and substitute system prompt
TARGET_REPO="/tmp/benchmark-repos/django"
SYSTEM_PROMPT=$(sed \
  -e "s|TARGET_REPO_PATH|$TARGET_REPO|g" \
  -e "s|OUTPUT_PATH|$OUTPUT_FILE|g" \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  "$SYSTEM_PROMPT_FILE")
TASK_CONTENT=$(sed \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$CONDITION_ID|g" \
  "$PROMPTS_DIR/task.md")

# Tool isolation validation function
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
        # Look for tool_use in assistant messages
        if entry.get("type") == "assistant":
            for block in entry.get("message", {}).get("content", []):
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tools_used.add(block["name"])

print(f"Tools used: {sorted(tools_used)}")

if expected_tool_set == "mcp":
    forbidden_used = tools_used & NATIVE_TOOLS
    if forbidden_used:
        print(f"ISOLATION FAIL: native tools used in MCP condition: {forbidden_used}", file=sys.stderr)
        sys.exit(1)
    mcp_used = tools_used & MCP_TOOLS
    print(f"MCP tools used: {sorted(mcp_used)}")
    print("ISOLATION PASS: no native tools used")
elif expected_tool_set == "native":
    forbidden_used = tools_used & MCP_TOOLS
    if forbidden_used:
        print(f"ISOLATION FAIL: MCP tools used in native condition: {forbidden_used}", file=sys.stderr)
        sys.exit(1)
    native_used = tools_used & NATIVE_TOOLS
    print(f"Native tools used: {sorted(native_used)}")
    print("ISOLATION PASS: no MCP tools used")
PYEOF
}

# Session capture setup
touch /tmp/.v12-run-marker
_REPO_SLUG="${REPO_ROOT//\//-}"
SESSION_DIR="${CLAUDE_SESSION_DIR:-$HOME/.claude/projects/${_REPO_SLUG}}"

# Claude invocation
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
  --dangerously-skip-permissions \
  --output-format json \
  --json-schema "$OUTPUT_SCHEMA" \
  ${BUDGET_FLAG:+"${BUDGET_FLAG[@]}"} \
  "$TASK_CONTENT" \
  > "$JSONL_FILE" \
  2> "$LOG_FILE"
echo "Run completed at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Extract structured output and telemetry from JSON array output
python3 - "$JSONL_FILE" "$OUTPUT_FILE" "$TELEMETRY_FILE" << 'PYEOF'
import json
import sys

jsonl_file = sys.argv[1]
output_file = sys.argv[2]
telemetry_file = sys.argv[3]

result_found = False
structured_output = None
telemetry_data = {}

try:
    with open(jsonl_file) as f:
        content = f.read().strip()
        if not content:
            print("ERROR: JSONL file is empty", file=sys.stderr)
            sys.exit(1)
        
        # claude --output-format json outputs a JSON array; parse it
        try:
            messages = json.loads(content)
            if not isinstance(messages, list):
                messages = [messages]
        except json.JSONDecodeError:
            print("ERROR: Could not parse JSONL file as JSON", file=sys.stderr)
            sys.exit(1)
        
        # Find the result message
        for msg in messages:
            if isinstance(msg, dict) and msg.get("type") == "result":
                result_found = True
                structured_output = msg.get("structured_output")
                
                # Extract telemetry fields from result envelope
                if "duration_ms" in msg:
                    telemetry_data["wall_time_ms"] = msg["duration_ms"]
                if "duration_api_ms" in msg:
                    telemetry_data["api_time_ms"] = msg["duration_api_ms"]
                if "num_turns" in msg:
                    telemetry_data["num_turns"] = msg["num_turns"]
                if "total_cost_usd" in msg:
                    telemetry_data["cost_usd"] = msg["total_cost_usd"]
                
                # Extract token counts from usage object
                usage = msg.get("usage", {})
                if isinstance(usage, dict):
                    if "input_tokens" in usage:
                        telemetry_data["input_tokens"] = usage["input_tokens"]
                    if "output_tokens" in usage:
                        telemetry_data["output_tokens"] = usage["output_tokens"]
                    if "cache_read_input_tokens" in usage:
                        telemetry_data["cache_read_tokens"] = usage["cache_read_input_tokens"]
                    if "cache_creation_input_tokens" in usage:
                        telemetry_data["cache_creation_tokens"] = usage["cache_creation_input_tokens"]
                
                break

    if not result_found:
        print("ERROR: No result message found in JSON output", file=sys.stderr)
        sys.exit(1)
    
    if structured_output is None:
        print("ERROR: structured_output is null or missing in result message", file=sys.stderr)
        sys.exit(1)
    
    # Write structured output to report file
    with open(output_file, 'w') as f:
        json.dump(structured_output, f, indent=2)
    
    # Write telemetry to sidecar file
    with open(telemetry_file, 'w') as f:
        json.dump(telemetry_data, f, indent=2)

except Exception as e:
    print(f"ERROR: {e}", file=sys.stderr)
    import traceback
    traceback.print_exc(file=sys.stderr)
    sys.exit(1)
PYEOF

# Session JSONL capture and validation
if [[ -d "$SESSION_DIR" ]]; then
  mapfile -t _sessions < <(find "$SESSION_DIR" -name "*.jsonl" -newer /tmp/.v12-run-marker 2>/dev/null)
  if (( ${#_sessions[@]} > 0 )); then
    LATEST_SESSION=$(ls -t "${_sessions[@]}" 2>/dev/null | head -1)
  else
    LATEST_SESSION=""
  fi
  if [[ -n "$LATEST_SESSION" ]]; then
    SESSION_COPY="$RUNS_DIR/${RUN_ID}-session.jsonl"
    cp "$LATEST_SESSION" "$SESSION_COPY"
    echo "Session JSONL: $SESSION_COPY"
    # Run tool isolation validation
    validate_tool_isolation "$SESSION_COPY" "$TOOL_SET"
  else
    echo "WARNING: Could not find session JSONL" >&2
  fi
fi

# Output validation
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

# Telemetry validation
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
