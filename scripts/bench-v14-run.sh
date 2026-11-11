#!/usr/bin/env bash
# v14 Benchmark Runner
# Parameterized by condition ID (A, B, C, D) and run ID.
# Condition A/B use claude-sonnet-4-6, C/D use claude-haiku-4-5.
# A/C use MCP tools only, B/D use native tools only.
# Validates tool isolation from session JSONL.
#
# Usage:
#   bash scripts/bench-v14-run.sh <CONDITION_ID> <RUN_ID>
#
# Examples:
#   bash scripts/bench-v14-run.sh A A-pilot
#   bash scripts/bench-v14-run.sh B B-scored-1
#   bash scripts/bench-v14-run.sh C C-scored-2
#   bash scripts/bench-v14-run.sh D D-pilot
#
# Environment variables:
#   BENCH_MAX_BUDGET_USD           -- cap spend per run (optional, e.g. "2.00")
#   RIPGREP_REPO                   -- local path to ripgrep clone
#                                     (default: /tmp/ripgrep-benchmark)
#   ANTHROPIC_DEFAULT_SONNET_MODEL -- model ID for conditions A/B
#                                     (default: claude-sonnet-4-6)
#                                     Set to a provider-qualified ID when using
#                                     Amazon Bedrock or GCP Vertex AI, e.g.
#                                     global.anthropic.claude-sonnet-4-6
#   ANTHROPIC_DEFAULT_HAIKU_MODEL  -- model ID for conditions C/D
#                                     (default: claude-haiku-4-5)
#                                     e.g. global.anthropic.claude-haiku-4-5-20251001-v1:0

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNS_DIR="$REPO_ROOT/docs/benchmarks/v14/results/runs"
PROMPTS_DIR="$REPO_ROOT/docs/benchmarks/v14/prompts"
MCP_CONFIG="$REPO_ROOT/docs/benchmarks/v14/mcp-aptu-coder-only.json"

mkdir -p "$RUNS_DIR"

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <CONDITION_ID> <RUN_ID>" >&2
  echo "CONDITION_ID: A, B, C, or D" >&2
  echo "RUN_ID: e.g. A-pilot, B-scored-1" >&2
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
# ripgrep repo setup
# ---------------------------------------------------------------------------
RIPGREP_REPO="${RIPGREP_REPO:-/tmp/ripgrep-benchmark}"
RIPGREP_COMMIT="${RIPGREP_COMMIT:-4649aa9700619f94cf9c66876e9549d83420e16c}"

if [[ -d "$RIPGREP_REPO" ]] && { find "$RIPGREP_REPO" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null | grep -q .; }; then
  # Non-empty directory: verify it is a git repo pointing at ripgrep
  if ! git -C "$RIPGREP_REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "ERROR: RIPGREP_REPO ('$RIPGREP_REPO') exists but is not a git repository." >&2
    echo "       Remove the directory or set RIPGREP_REPO to an empty/absent path." >&2
    exit 1
  fi
  # Check remote URL contains 'ripgrep'
  REMOTE_URL=$(git -C "$RIPGREP_REPO" remote get-url origin 2>/dev/null || echo "")
  REMOTE_URL_LOWER=$(echo "$REMOTE_URL" | tr '[:upper:]' '[:lower:]')
  if [[ -z "$REMOTE_URL" ]]; then
    echo "WARNING: RIPGREP_REPO has no origin remote. Proceeding with local-only repo." >&2
  elif [[ "$REMOTE_URL_LOWER" != *ripgrep* ]]; then
    echo "ERROR: RIPGREP_REPO remote URL ('$REMOTE_URL') does not contain 'ripgrep'." >&2
    echo "       This does not appear to be the ripgrep repository." >&2
    exit 1
  fi
elif [[ ! -d "$RIPGREP_REPO/crates/searcher" ]]; then
  echo "Cloning ripgrep (shallow) into $RIPGREP_REPO ..."
  git clone --depth=1 https://github.com/BurntSushi/ripgrep.git "$RIPGREP_REPO"
fi

# Ensure we are on the pinned commit; fetch it if the shallow clone doesn't have it
if ! git -C "$RIPGREP_REPO" rev-parse --verify "${RIPGREP_COMMIT}^{commit}" >/dev/null 2>&1; then
  echo "Fetching pinned ripgrep commit $RIPGREP_COMMIT ..." >&2
  if ! git -C "$RIPGREP_REPO" fetch --depth=1 origin "$RIPGREP_COMMIT" 2>/dev/null; then
    if [[ "$RUN_ID" == *scored* ]]; then
      echo "ERROR: Failed to fetch pinned commit $RIPGREP_COMMIT for scored run $RUN_ID." >&2
      exit 1
    else
      echo "WARNING: Failed to fetch pinned commit $RIPGREP_COMMIT; proceeding with existing clone." >&2
    fi
  fi
fi

if git -C "$RIPGREP_REPO" rev-parse --verify "${RIPGREP_COMMIT}^{commit}" >/dev/null 2>&1; then
  git -C "$RIPGREP_REPO" -c advice.detachedHead=false checkout "$RIPGREP_COMMIT" >/dev/null 2>&1 || true
fi

ACTUAL_COMMIT=$(git -C "$RIPGREP_REPO" rev-parse HEAD)
if [[ "$ACTUAL_COMMIT" != "$RIPGREP_COMMIT" ]]; then
  if [[ "$RUN_ID" == *scored* ]]; then
    echo "ERROR: ripgrep HEAD is $ACTUAL_COMMIT, expected $RIPGREP_COMMIT." >&2
    echo "       Scored runs require the pinned commit for reproducibility." >&2
    exit 1
  else
    echo "WARNING: ripgrep HEAD is $ACTUAL_COMMIT, expected $RIPGREP_COMMIT." >&2
    echo "         Pilot runs may proceed; scored runs must use the pinned commit." >&2
  fi
fi

# ---------------------------------------------------------------------------
# Condition dispatch
# ---------------------------------------------------------------------------
# Resolve model IDs: prefer env vars set by ~/.zshrc.local (Bedrock), fall back to aliases
SONNET_MODEL="${ANTHROPIC_DEFAULT_SONNET_MODEL:-claude-sonnet-4-6}"
HAIKU_MODEL="${ANTHROPIC_DEFAULT_HAIKU_MODEL:-claude-haiku-4-5}"

case "$CONDITION_ID" in
  A)
    MODEL="$SONNET_MODEL"
    TOOL_SET="mcp"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-a-mcp-sonnet.md"
    ;;
  B)
    MODEL="$SONNET_MODEL"
    TOOL_SET="native"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-b-native-sonnet.md"
    ;;
  C)
    MODEL="$HAIKU_MODEL"
    TOOL_SET="mcp"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-c-mcp-haiku.md"
    ;;
  D)
    MODEL="$HAIKU_MODEL"
    TOOL_SET="native"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-d-native-haiku.md"
    ;;
esac

# ---------------------------------------------------------------------------
# Output files
# ---------------------------------------------------------------------------
OUTPUT_FILE="$RUNS_DIR/${RUN_ID}-report.json"
TELEMETRY_FILE="$RUNS_DIR/${RUN_ID}-telemetry.json"
LOG_FILE="$RUNS_DIR/${RUN_ID}.log"
SCRATCH_FILE=$(mktemp /tmp/bench-v14-scratch-XXXXXX.json)

# ---------------------------------------------------------------------------
# Tool isolation flags
# ---------------------------------------------------------------------------
MCP_TOOLS="mcp__aptu-coder__analyze_directory,mcp__aptu-coder__analyze_file,mcp__aptu-coder__analyze_module"
NATIVE_TOOLS="Bash,Glob,Grep,Read,Write,ToolSearch"

if [[ "$TOOL_SET" == "mcp" ]]; then
  ALLOWED_TOOLS="$MCP_TOOLS"
  MCP_FLAGS=(--mcp-config "$MCP_CONFIG" --strict-mcp-config)
  trap 'rm -f "$SCRATCH_FILE" "$RUN_MARKER"' EXIT
else
  ALLOWED_TOOLS="$NATIVE_TOOLS"
  EMPTY_MCP=$(mktemp /tmp/bench-v14-empty-mcp.XXXXXX.json)
  echo '{"mcpServers":{}}' > "$EMPTY_MCP"
  MCP_FLAGS=(--mcp-config "$EMPTY_MCP" --strict-mcp-config)
  trap 'rm -f "$SCRATCH_FILE" "$EMPTY_MCP" "$RUN_MARKER"' EXIT
fi

# ---------------------------------------------------------------------------
# Output schema
# ---------------------------------------------------------------------------
OUTPUT_SCHEMA=$(cat <<'SCHEMA'
{
  "type": "object",
  "properties": {
    "run_id":            { "type": "string" },
    "condition":         { "type": "string" },
    "sink_impls":        { "type": "array", "items": { "type": "object" } },
    "call_chain":        { "type": "array", "items": { "type": "object" } },
    "change_impact_map": { "type": "array", "items": { "type": "object" } },
    "tool_calls_total":  { "type": "integer" }
  },
  "required": [
    "run_id",
    "condition",
    "sink_impls",
    "call_chain",
    "change_impact_map",
    "tool_calls_total"
  ]
}
SCHEMA
)

# Escape values for safe use in sed replacement strings.
# Escapes backslashes, '&', and the '|' delimiter used below.
escape_sed_replacement() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//&/\\&}
  s=${s//|/\\|}
  printf '%s' "$s"
}

# ---------------------------------------------------------------------------
# Build prompts (substitute placeholders)
# ---------------------------------------------------------------------------
ESCAPED_RIPGREP_REPO=$(escape_sed_replacement "$RIPGREP_REPO")
ESCAPED_RUN_ID=$(escape_sed_replacement "$RUN_ID")
ESCAPED_CONDITION_ID=$(escape_sed_replacement "$CONDITION_ID")

SYSTEM_PROMPT=$(sed \
  -e "s|<repo>|$ESCAPED_RIPGREP_REPO|g" \
  -e "s|REPO_PATH_PLACEHOLDER|$ESCAPED_RIPGREP_REPO|g" \
  -e "s|RUN_ID_PLACEHOLDER|$ESCAPED_RUN_ID|g" \
  "$SYSTEM_PROMPT_FILE")

TASK_CONTENT=$(sed \
  -e "s|RUN_ID_PLACEHOLDER|$ESCAPED_RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$ESCAPED_CONDITION_ID|g" \
  "$PROMPTS_DIR/task.md")

# Append repo path to task so the agent knows where to point tools
TASK_CONTENT="$TASK_CONTENT

Repository is cloned at: $RIPGREP_REPO
All tool paths must use this absolute prefix."

# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------
cat <<EOF
=== v14 Benchmark Run ===
CONDITION:   $CONDITION_ID
RUN_ID:      $RUN_ID
MODEL:       $MODEL
TOOL_SET:    $TOOL_SET
ALLOWED:     $ALLOWED_TOOLS
RIPGREP:     $RIPGREP_REPO ($ACTUAL_COMMIT)
BUDGET:      ${BENCH_MAX_BUDGET_USD:-unlimited} USD
OUTPUT:      $OUTPUT_FILE
TELEMETRY:   $TELEMETRY_FILE
EOF

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
echo "Starting run at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
RUN_MARKER="/tmp/.v14-run-marker-$RUN_ID"
touch "$RUN_MARKER"

BUDGET_FLAG=()
if [[ -n "${BENCH_MAX_BUDGET_USD:-}" ]]; then
  BUDGET_FLAG=(--max-budget-usd "$BENCH_MAX_BUDGET_USD")
fi

DISABLE_PROMPT_CACHING=1 claude \
  -p \
  --model "$MODEL" \
  --system-prompt "$SYSTEM_PROMPT" \
  "${MCP_FLAGS[@]}" \
  --allowedTools "$ALLOWED_TOOLS" \
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
_REPO_SLUG="${REPO_ROOT//\//-}"
SESSION_DIR="${CLAUDE_SESSION_DIR:-$HOME/.claude/projects/${_REPO_SLUG}}"

# Use portable find (bash 3 compat; mapfile is bash 4+ only)
_sessions=()
while IFS= read -r f; do _sessions+=("$f"); done < <(find "$SESSION_DIR" -name "*.jsonl" -newer "$RUN_MARKER" 2>/dev/null || true)
if [[ ${#_sessions[@]} -gt 0 ]]; then
  LATEST_SESSION=$(ls -t "${_sessions[@]}" 2>/dev/null | head -1)
  SESSION_COPY="$RUNS_DIR/${RUN_ID}-session.jsonl"
  cp "$LATEST_SESSION" "$SESSION_COPY"
  echo "Session JSONL: $SESSION_COPY"

  python3 - "$SESSION_COPY" "$TOOL_SET" << 'PYEOF'
import json, sys

session_file, expected_tool_set = sys.argv[1], sys.argv[2]

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
        if entry.get("type") == "assistant":
            for block in entry.get("message", {}).get("content", []):
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tools_used.add(block["name"])

print(f"Tools used: {sorted(tools_used)}")

if expected_tool_set == "mcp":
    forbidden = tools_used & NATIVE_TOOLS
    if forbidden:
        print(f"ISOLATION FAIL: native tools used in MCP condition: {forbidden}", file=sys.stderr)
        sys.exit(1)
    print(f"MCP tools used: {sorted(tools_used & MCP_TOOLS)}")
    print("ISOLATION PASS")
else:
    forbidden = tools_used & MCP_TOOLS
    if forbidden:
        print(f"ISOLATION FAIL: MCP tools used in native condition: {forbidden}", file=sys.stderr)
        sys.exit(1)
    print(f"Native tools used: {sorted(tools_used & NATIVE_TOOLS)}")
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
si  = len(d.get('sink_impls', []))
cc  = len(d.get('call_chain', []))
cim = len(d.get('change_impact_map', []))
tc  = d.get('tool_calls_total', '?')
print(f'  sink_impls={si}  call_chain={cc}  change_impact_map={cim}  tool_calls={tc}')
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
