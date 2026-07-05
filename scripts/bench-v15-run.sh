#!/bin/bash
set -euo pipefail

# v15 Benchmark Harness: MCP remote_file/remote_tree vs curl
# Usage: bash scripts/bench-v15-run.sh <CONDITION_ID> <TARGET_ID> <RUN_ID> [--error-type <type>]
# CONDITION_ID: E (MCP) or F (curl)
# TARGET_ID: T1, T1b, T2, T3, T4, error
# RUN_ID: e.g. E-T1-pilot, F-T1-scored-1
# --error-type: missing_token or not_found (for error target only)

# ============================================================================
# Argument Validation
# ============================================================================

if [[ $# -lt 3 ]]; then
  echo "Usage: $0 <CONDITION_ID> <TARGET_ID> <RUN_ID> [--error-type <type>]" >&2
  exit 1
fi

CONDITION_ID="$1"
TARGET_ID="$2"
RUN_ID="$3"
ERROR_TYPE=""

if [[ $# -ge 5 && "$4" == "--error-type" ]]; then
  ERROR_TYPE="$5"
fi

# Validate condition
if [[ "$CONDITION_ID" != "E" && "$CONDITION_ID" != "F" ]]; then
  echo "ERROR: CONDITION_ID must be E or F, got: $CONDITION_ID" >&2
  exit 1
fi

# Validate target
if [[ ! "$TARGET_ID" =~ ^(T1|T1b|T2|T3|T4|error)$ ]]; then
  echo "ERROR: TARGET_ID must be T1, T1b, T2, T3, T4, or error, got: $TARGET_ID" >&2
  exit 1
fi

# Validate error type if error target
if [[ "$TARGET_ID" == "error" ]]; then
  if [[ -z "$ERROR_TYPE" ]]; then
    echo "ERROR: --error-type required for error target" >&2
    exit 1
  fi
  if [[ ! "$ERROR_TYPE" =~ ^(missing_token|not_found)$ ]]; then
    echo "ERROR: --error-type must be missing_token or not_found, got: $ERROR_TYPE" >&2
    exit 1
  fi
fi

# ============================================================================
# Environment Setup
# ============================================================================

# GITLAB_TOKEN check: fail fast unless this is a missing_token error sub-task.
# Missing-token error sub-task: intentionally omits GITLAB_TOKEN to test tool error handling.
if [[ ! ("$TARGET_ID" == "error" && "$ERROR_TYPE" == "missing_token") ]]; then
  if [[ -z "${GITLAB_TOKEN:-}" ]]; then
    echo "ERROR: GITLAB_TOKEN environment variable is not set" >&2
    exit 1
  fi
fi

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BENCH_DIR="$REPO_ROOT/docs/benchmarks/v15"
RESULTS_DIR="$BENCH_DIR/results"
PROMPTS_DIR="$BENCH_DIR/prompts"

# Create results directory if needed
mkdir -p "$RESULTS_DIR"

# ============================================================================
# Timing Functions
# ============================================================================

# millis_now() returns current time in milliseconds using the most portable method available.
# Fallback chain: (1) bash $EPOCHREALTIME (bash 5+, Linux/macOS), (2) date +%s%N with awk
# division (Linux nanoseconds, no coreutils needed), (3) gdate +%s%3N (macOS with coreutils).
millis_now() {
  if [[ -n "${EPOCHREALTIME:-}" ]]; then
    # bash 5+ provides $EPOCHREALTIME as seconds.nanoseconds
    # Convert to integer milliseconds: seconds*1000 + nanoseconds/1000000
    awk "BEGIN { print int(${EPOCHREALTIME} * 1000) }"
  elif date +%s%N &>/dev/null; then
    # Linux: date +%s%N returns seconds and nanoseconds concatenated
    # Divide by 1000000 to convert nanoseconds to milliseconds
    date +%s%N | awk '{ print int($1 / 1000000) }'
  elif command -v gdate &>/dev/null; then
    # macOS with coreutils: gdate +%s%3N returns seconds and milliseconds
    gdate +%s%3N
  else
    # Fallback: return 0 if no timing method available
    echo "0"
  fi
}

# ============================================================================
# Load Prompts
# ============================================================================

load_system_prompt() {
  local condition="$1"
  if [[ "$condition" == "E" ]]; then
    cat "$PROMPTS_DIR/condition-e-mcp.md"
  else
    cat "$PROMPTS_DIR/condition-f-curl.md"
  fi
}

load_task_prompt() {
  local target="$1"
  case "$target" in
    T1|T1b)
      cat "$PROMPTS_DIR/task-T1.md"
      ;;
    T2|T3)
      cat "$PROMPTS_DIR/task-T2.md"
      ;;
    T4)
      cat "$PROMPTS_DIR/task-T4.md"
      ;;
    error)
      cat "$PROMPTS_DIR/task-error.md"
      ;;
    *)
      echo "ERROR: Unknown target: $target" >&2
      exit 1
      ;;
  esac
}

# ============================================================================
# Placeholder Substitution
# ============================================================================

substitute_placeholders() {
  local text="$1"
  local target="$2"
  local run_id="$3"
  local condition="$4"
  local error_type="${5:-}"

  # Substitute TARGET_ID_PLACEHOLDER
  text="${text//TARGET_ID_PLACEHOLDER/$target}"

  # Substitute RUN_ID_PLACEHOLDER
  text="${text//RUN_ID_PLACEHOLDER/$run_id}"

  # Substitute CONDITION_PLACEHOLDER
  text="${text//CONDITION_PLACEHOLDER/$condition}"

  # Substitute ERROR_TYPE_PLACEHOLDER
  if [[ -n "$error_type" ]]; then
    text="${text//ERROR_TYPE_PLACEHOLDER/$error_type}"
  fi

  # Substitute TOOL_OR_COMMAND_PLACEHOLDER
  if [[ "$condition" == "E" ]]; then
    text="${text//TOOL_OR_COMMAND_PLACEHOLDER/mcp__aptu-coder__remote_file}"
  else
    text="${text//TOOL_OR_COMMAND_PLACEHOLDER/curl}"
  fi

  echo "$text"
}

# ============================================================================
# JSON Extraction
# ============================================================================

extract_json_from_output() {
  local output_file="$1"
  
  # Use Python to extract the last valid JSON object from the output
  python3 << 'PYTHON_EOF'
import json
import re
import sys

try:
    with open(sys.argv[1], 'r') as f:
        output = f.read()
except Exception as e:
    print(f"ERROR: Could not read output file: {e}", file=sys.stderr)
    sys.exit(1)

# Find all {...} blocks that might be JSON
pattern = r'\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}'
matches = list(re.finditer(pattern, output, re.DOTALL))

# Try to parse from the end (most recent JSON)
for m in reversed(matches):
    try:
        data = json.loads(m.group())
        # Verify it has the expected structure (at least run_id, condition, target_id)
        if 'run_id' in data and 'condition' in data and 'target_id' in data:
            print(json.dumps(data))
            sys.exit(0)
    except json.JSONDecodeError:
        continue

print("ERROR: No valid JSON report found in output", file=sys.stderr)
sys.exit(1)
PYTHON_EOF
  
  return $?
}

# ============================================================================
# Goose Profile Creation
# ============================================================================

create_goose_profile() {
  local condition="$1"
  local profile_file="/tmp/bench-v15-profile-${condition}-$$.yaml"

  if [[ "$condition" == "E" ]]; then
    # MCP profile with aptu-coder remote tools
    cat > "$profile_file" << 'PROFILE_EOF'
provider: gcp_vertex_ai
model: claude-haiku-4-5@20251001
extensions:
  - type: stdio
    name: aptu-coder
    cmd: aptu-coder
    args: []
    env_keys: []
PROFILE_EOF
  else
    # Bash-only profile (no MCP)
    cat > "$profile_file" << 'PROFILE_EOF'
provider: gcp_vertex_ai
model: claude-haiku-4-5@20251001
extensions: []
PROFILE_EOF
  fi

  echo "$profile_file"
}

# ============================================================================
# Curl Pre-Timing (Condition F only)
# ============================================================================

run_curl_baseline() {
  local target="$1"
  local start_ms end_ms latency_ms
  
  start_ms=$(millis_now)
  
  # Run a simple curl to gitlab.com to measure baseline latency
  # This is a HEAD request to minimize data transfer
  curl -s -I -H "PRIVATE-TOKEN: $GITLAB_TOKEN" \
    "https://gitlab.com/api/v4/projects/gnome%2Fgtk/repository/files/gtk%2Fgtkwidget.c?ref=HEAD" \
    > /dev/null 2>&1 || true
  
  end_ms=$(millis_now)
  latency_ms=$(( end_ms - start_ms ))
  
  echo "$latency_ms"
}

# ============================================================================
# Main Run
# ============================================================================

main() {
  local start_time_ms end_time_ms total_latency_ms
  local system_prompt task_prompt combined_prompt
  local profile_file log_file output_file report_file harness_file
  local json_output

  # Load prompts
  system_prompt=$(load_system_prompt "$CONDITION_ID")
  task_prompt=$(load_task_prompt "$TARGET_ID")

  # Substitute placeholders
  task_prompt=$(substitute_placeholders "$task_prompt" "$TARGET_ID" "$RUN_ID" "$CONDITION_ID" "$ERROR_TYPE")

  # Combine prompts
  combined_prompt="$system_prompt

$task_prompt"

  # Create goose profile
  profile_file=$(create_goose_profile "$CONDITION_ID")
  trap "rm -f '$profile_file'" EXIT

  # Output files
  log_file="$RESULTS_DIR/${RUN_ID}.log"
  output_file="$RESULTS_DIR/${RUN_ID}-output.txt"
  report_file="$RESULTS_DIR/${RUN_ID}-report.json"
  harness_file="$RESULTS_DIR/${RUN_ID}-harness.json"

  # Record start time
  start_time_ms=$(millis_now)

  # Run goose
  DISABLE_PROMPT_CACHING=1 goose run \
    --profile "$profile_file" \
    --text "$combined_prompt" \
    --no-session \
    > "$output_file" 2> "$log_file" || true

  # Record end time
  end_time_ms=$(millis_now)
  total_latency_ms=$(( end_time_ms - start_time_ms ))

  # Check for timeout (>30s)
  if [[ $total_latency_ms -gt 30000 ]]; then
    echo "DISCARD: run exceeded 30s (${total_latency_ms}ms), re-run required" >&2
    exit 2
  fi

  # Extract JSON from output
  if ! json_output=$(extract_json_from_output "$output_file"); then
    echo "ERROR: Failed to extract JSON from goose output" >&2
    cat "$output_file" >&2
    exit 1
  fi

  # Save report
  echo "$json_output" > "$report_file"

  # Create harness telemetry
  local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  local content_chars=$(echo "$json_output" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('content_chars', 0))" 2>/dev/null || echo "0")
  local est_tokens=$(( content_chars / 4 ))

  cat > "$harness_file" << HARNESS_JSON
{
  "run_id": "$RUN_ID",
  "condition": "$CONDITION_ID",
  "target_id": "$TARGET_ID",
  "latency_ms": $total_latency_ms,
  "raw_bytes": 0,
  "content_chars": $content_chars,
  "est_tokens": $est_tokens,
  "timestamp": "$timestamp",
  "timing_method": "$TIMING_METHOD"
}
HARNESS_JSON

  # Print summary
  echo "=========================================="
  echo "Run: $RUN_ID"
  echo "Condition: $CONDITION_ID"
  echo "Target: $TARGET_ID"
  echo "Latency: ${total_latency_ms}ms"
  echo "Content chars: $content_chars"
  echo "Est. tokens: $est_tokens"
  echo "Report: $report_file"
  echo "Harness: $harness_file"
  echo "=========================================="

  # Print extracted JSON for verification
  echo "Agent output:"
  echo "$json_output" | python3 -m json.tool 2>/dev/null || echo "$json_output"
}

main "$@"
