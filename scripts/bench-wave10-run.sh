#!/usr/bin/env bash
# Wave 10 MCP Edit Profile Benchmark Runner
# Parameterized by condition ID (E, F) and run ID.
# Both conditions use MCP tools with the edit profile (4 tools only).
# Validates tool isolation from session JSONL.
#
# Usage:
#   bash scripts/bench-wave10-run.sh <CONDITION_ID> <RUN_ID>
#
# Examples:
#   bash scripts/bench-wave10-run.sh E E-pilot
#   bash scripts/bench-wave10-run.sh E E-scored-1
#   bash scripts/bench-wave10-run.sh F F-pilot
#   bash scripts/bench-wave10-run.sh F F-scored-2
#
# Environment variables:
#   BENCH_MAX_BUDGET_USD           -- cap spend per run (optional, e.g. "2.00")
#   ANTHROPIC_DEFAULT_SONNET_MODEL -- model ID for condition E
#                                     (default: claude-sonnet-4-6)
#                                     Set to a provider-qualified ID when using
#                                     Amazon Bedrock or GCP Vertex AI, e.g.
#                                     global.anthropic.claude-sonnet-4-6
#   ANTHROPIC_DEFAULT_HAIKU_MODEL  -- model ID for condition F
#                                     (default: claude-haiku-4-5)
#                                     e.g. global.anthropic.claude-haiku-4-5-20251001-v1:0
#   CARGO_TARGET_DIR               -- optional shared target directory for faster builds
#   WAVE10_WORKTREE_BASE           -- base directory for temporary run worktrees
#                                     (default: /tmp; override on systems with small tmpfs)

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNS_DIR="$REPO_ROOT/docs/benchmarks/wave10/results/runs"
PROMPTS_DIR="$REPO_ROOT/docs/benchmarks/wave10/prompts"
MCP_CONFIG_TEMPLATE="$REPO_ROOT/docs/benchmarks/wave10/mcp-aptu-coder-edit-profile.json"

mkdir -p "$RUNS_DIR"

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <CONDITION_ID> <RUN_ID>" >&2
  echo "CONDITION_ID: E or F" >&2
  echo "RUN_ID: e.g. E-pilot, E-scored-1" >&2
  exit 1
fi

CONDITION_ID="$1"
RUN_ID="$2"

if [[ ! "$CONDITION_ID" =~ ^[EF]$ ]]; then
  echo "ERROR: CONDITION_ID must be E or F" >&2
  exit 1
fi

if [[ ! "$RUN_ID" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "ERROR: RUN_ID must contain only alphanumeric characters, dots, underscores, and hyphens" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Worktree isolation
# ---------------------------------------------------------------------------
# Each run gets a fresh temporary worktree from origin/main to prevent
# cross-run contamination. Changes made by the agent are captured via git diff.
# WAVE10_WORKTREE_BASE overrides the default /tmp location (e.g. for systems
# with small tmpfs or shared CI runners where /tmp is not appropriate).
WAVE10_WORKTREE_BASE="${WAVE10_WORKTREE_BASE:-/tmp}"
RUN_WORKTREE="${WAVE10_WORKTREE_BASE}/wave10-run-${RUN_ID}"

if [[ -d "$RUN_WORKTREE" ]]; then
  git -C "$REPO_ROOT" worktree remove --force "$RUN_WORKTREE" 2>/dev/null || rm -rf "$RUN_WORKTREE"
fi

git -C "$REPO_ROOT" fetch origin main --quiet
git -C "$REPO_ROOT" worktree add "$RUN_WORKTREE" origin/main 2>&1

# ---------------------------------------------------------------------------
# Worktree preparation: strip tsx wiring before agent runs
# ---------------------------------------------------------------------------
# The task is to re-wire tsx support. We strip it first so the agent must
# add it back correctly. Stripping is idempotent (safe to run multiple times).

python3 << STRIP_TSX_EOF
def strip_tsx_language_info_arm(lines):
    """Remove the tsx arm from get_language_info using anchor-based matching.

    Anchors: opening line is '#[cfg(feature = "lang-tsx")]' immediately
    followed by '"tsx" => Some(LanguageInfo {'; closing line is '        }),'.
    No assumptions about content between anchors.
    """
    result = []
    i = 0
    while i < len(lines):
        if (lines[i].rstrip() == '        #[cfg(feature = "lang-tsx")]'
                and i + 1 < len(lines)
                and lines[i + 1].rstrip() == '        "tsx" => Some(LanguageInfo {'):
            # Skip lines until the closing arm delimiter
            i += 2
            while i < len(lines) and lines[i].rstrip() != '        }),':
                i += 1
            i += 1  # skip the closing '}),' line
        else:
            result.append(lines[i])
            i += 1
    return result


def strip_tsx_ts_language_arm(lines):
    """Remove the tsx arm from get_ts_language using anchor-based matching.

    The arm is exactly two lines:
      #[cfg(feature = "lang-tsx")]
      "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
    """
    result = []
    i = 0
    while i < len(lines):
        if (lines[i].rstrip() == '        #[cfg(feature = "lang-tsx")]'
                and i + 1 < len(lines)
                and '"tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX' in lines[i + 1]):
            i += 2  # skip both lines
        else:
            result.append(lines[i])
            i += 1
    return result


def strip_tsx_extension_map_entry(lines):
    """Remove the tsx entry from EXTENSION_MAP using anchor-based matching.

    The entry is exactly two lines:
      #[cfg(feature = "lang-tsx")]
      ("tsx", "tsx"),
    """
    result = []
    i = 0
    while i < len(lines):
        if (lines[i].rstrip() == '    #[cfg(feature = "lang-tsx")]'
                and i + 1 < len(lines)
                and lines[i + 1].rstrip() == '    ("tsx", "tsx"),'):
            i += 2
        else:
            result.append(lines[i])
            i += 1
    return result


def strip_tsx_supported_languages_entry(lines):
    """Remove the tsx entry from supported_languages using anchor-based matching.

    The entry is exactly two lines:
      #[cfg(feature = "lang-tsx")]
      "tsx",
    """
    result = []
    i = 0
    while i < len(lines):
        if (lines[i].rstrip() == '        #[cfg(feature = "lang-tsx")]'
                and i + 1 < len(lines)
                and lines[i + 1].rstrip() == '        "tsx",'):
            i += 2
        else:
            result.append(lines[i])
            i += 1
    return result


# Strip mod.rs
mod_rs_path = "$RUN_WORKTREE/crates/aptu-coder-core/src/languages/mod.rs"
with open(mod_rs_path) as f:
    lines = f.readlines()

lines = strip_tsx_language_info_arm(lines)
lines = strip_tsx_ts_language_arm(lines)

with open(mod_rs_path, "w") as f:
    f.writelines(lines)

# Strip lang.rs
lang_rs_path = "$RUN_WORKTREE/crates/aptu-coder-core/src/lang.rs"
with open(lang_rs_path) as f:
    lines = f.readlines()

lines = strip_tsx_extension_map_entry(lines)
lines = strip_tsx_supported_languages_entry(lines)

with open(lang_rs_path, "w") as f:
    f.writelines(lines)

print("TSX stripping complete")
STRIP_TSX_EOF

# Verify syntax
python3 << SYNTAX_CHECK_EOF
import ast
import sys

try:
    with open("$RUN_WORKTREE/crates/aptu-coder-core/src/languages/mod.rs") as f:
        # Not Python, but we can at least check it's readable
        content = f.read()
        if not content:
            print("ERROR: mod.rs is empty", file=sys.stderr)
            sys.exit(1)
    with open("$RUN_WORKTREE/crates/aptu-coder-core/src/lang.rs") as f:
        content = f.read()
        if not content:
            print("ERROR: lang.rs is empty", file=sys.stderr)
            sys.exit(1)
except Exception as e:
    print(f"ERROR: {e}", file=sys.stderr)
    sys.exit(1)
SYNTAX_CHECK_EOF

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  export CARGO_TARGET_DIR
fi

# ---------------------------------------------------------------------------
# Condition dispatch
# ---------------------------------------------------------------------------
# Resolve model IDs: prefer env vars set by ~/.zshrc.local (Bedrock), fall back to aliases
SONNET_MODEL="${ANTHROPIC_DEFAULT_SONNET_MODEL:-claude-sonnet-4-6}"
HAIKU_MODEL="${ANTHROPIC_DEFAULT_HAIKU_MODEL:-claude-haiku-4-5}"

case "$CONDITION_ID" in
  E)
    MODEL="$SONNET_MODEL"
    TOOL_SET="mcp"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-e-mcp-edit-sonnet.md"
    ;;
  F)
    MODEL="$HAIKU_MODEL"
    TOOL_SET="mcp"
    SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-f-mcp-edit-haiku.md"
    ;;
esac

# ---------------------------------------------------------------------------
# Tool isolation flags
# ---------------------------------------------------------------------------
# Edit profile: only 3 tools allowed
MCP_TOOLS="mcp__aptu-coder__edit_replace,mcp__aptu-coder__edit_overwrite,mcp__aptu-coder__exec_command"
ALLOWED_TOOLS="$MCP_TOOLS"

# ---------------------------------------------------------------------------
# MCP config generation
# ---------------------------------------------------------------------------
# Generate the MCP config at runtime with the metrics export file path substituted
MCP_CONFIG="/tmp/wave10-mcp-edit-profile-${RUN_ID}.json"

sed "s|METRICS_RUN_ID_PLACEHOLDER|${RUN_ID}|g" "$MCP_CONFIG_TEMPLATE" > "$MCP_CONFIG"

MCP_FLAGS=(--mcp-config "$MCP_CONFIG")

# ---------------------------------------------------------------------------
# Output files
# ---------------------------------------------------------------------------
OUTPUT_FILE="$RUNS_DIR/${RUN_ID}-output.json"
TELEMETRY_FILE="$RUNS_DIR/${RUN_ID}-telemetry.json"
VERIFICATION_FILE="$RUNS_DIR/${RUN_ID}-verification.json"
SCRATCH_FILE="/tmp/.wave10-scratch-$RUN_ID"
LOG_FILE="$RUNS_DIR/${RUN_ID}-log.txt"

# ---------------------------------------------------------------------------
# Output schema
# ---------------------------------------------------------------------------
OUTPUT_SCHEMA=$(cat <<'SCHEMA'
{
  "type": "object",
  "properties": {
    "run_id":                     { "type": "string" },
    "condition":                  { "type": "string" },
    "files_created":              { "type": "array", "items": { "type": "object" } },
    "files_modified":             { "type": "array", "items": { "type": "object" } },
    "feature_flag_name":          { "type": "string" },
    "ts_crate_used":              { "type": "string" },
    "ts_crate_version":           { "type": "string" },
    "ts_entry_point":             { "type": "string" },
    "queries_written":            { "type": "array", "items": { "type": "object" } },
    "extract_inheritance_present":{ "type": "boolean" },
    "extension_registrations":    { "type": "array", "items": { "type": "string" } },
    "test_names":                 { "type": "array", "items": { "type": "string" } },
    "compile_belief":             { "type": "string" },
    "compile_belief_reason":      { "type": "string" },
    "tool_calls_total":           { "type": "integer" }
  },
  "required": [
    "run_id",
    "condition",
    "files_created",
    "files_modified",
    "feature_flag_name",
    "ts_crate_used",
    "ts_crate_version",
    "ts_entry_point",
    "queries_written",
    "extract_inheritance_present",
    "extension_registrations",
    "test_names",
    "compile_belief",
    "compile_belief_reason",
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
ESCAPED_RUN_WORKTREE=$(escape_sed_replacement "$RUN_WORKTREE")
ESCAPED_RUN_ID=$(escape_sed_replacement "$RUN_ID")
ESCAPED_CONDITION_ID=$(escape_sed_replacement "$CONDITION_ID")

SYSTEM_PROMPT=$(sed \
  -e "s|REPO_PATH_PLACEHOLDER|$ESCAPED_RUN_WORKTREE|g" \
  "$SYSTEM_PROMPT_FILE")

TASK_CONTENT=$(sed \
  -e "s|REPO_PATH_PLACEHOLDER|$ESCAPED_RUN_WORKTREE|g" \
  -e "s|RUN_ID_PLACEHOLDER|$ESCAPED_RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$ESCAPED_CONDITION_ID|g" \
  "$PROMPTS_DIR/task.md")

# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------
cat <<EOF
=== Wave 10 MCP Edit Profile Benchmark Run ===
CONDITION:   $CONDITION_ID
RUN_ID:      $RUN_ID
MODEL:       $MODEL
TOOL_SET:    $TOOL_SET
ALLOWED:     $ALLOWED_TOOLS
WORKTREE:    $RUN_WORKTREE
BUDGET:      ${BENCH_MAX_BUDGET_USD:-unlimited} USD
OUTPUT:      $OUTPUT_FILE
TELEMETRY:   $TELEMETRY_FILE
EOF

# ---------------------------------------------------------------------------
# Cleanup trap
# ---------------------------------------------------------------------------
trap 'rm -f "$SCRATCH_FILE"; git -C "$REPO_ROOT" worktree remove --force "$RUN_WORKTREE" 2>/dev/null || true' EXIT

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
echo "Starting run at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
RUN_MARKER="/tmp/.wave10-run-marker-$RUN_ID"
touch "$RUN_MARKER"

BUDGET_FLAG=()
if [[ -n "${BENCH_MAX_BUDGET_USD:-}" ]]; then
  BUDGET_FLAG=(--max-budget-usd "$BENCH_MAX_BUDGET_USD")
fi

# Remove inherited benchmark results directory to prevent stale file confusion
rm -rf "$RUN_WORKTREE/docs/benchmarks/wave10/results/runs/"* 2>/dev/null || true

# Set metrics export file path for aptu-coder server
APTU_CODER_METRICS_EXPORT_FILE="/tmp/wave10-metrics-${RUN_ID}.json"
export APTU_CODER_METRICS_EXPORT_FILE

(cd "$RUN_WORKTREE" && DISABLE_PROMPT_CACHING=1 claude \
  -p \
  --model "$MODEL" \
  --system-prompt "$SYSTEM_PROMPT" \
  ${MCP_FLAGS:+"${MCP_FLAGS[@]}"} \
  --allowedTools "$ALLOWED_TOOLS" \
  --dangerously-skip-permissions \
  --output-format json \
  --json-schema "$OUTPUT_SCHEMA" \
  ${BUDGET_FLAG:+"${BUDGET_FLAG[@]}"} \
  "$TASK_CONTENT" \
  > "$SCRATCH_FILE" \
  2> "$LOG_FILE")

echo "Run completed at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ---------------------------------------------------------------------------
# Display server-side metrics
# ---------------------------------------------------------------------------
if [[ -f "$APTU_CODER_METRICS_EXPORT_FILE" ]]; then
  echo "=== Server-side metrics ==="
  jq '.' "$APTU_CODER_METRICS_EXPORT_FILE"
  cp "$APTU_CODER_METRICS_EXPORT_FILE" "$RUNS_DIR/metrics-${RUN_ID}.json"
else
  echo "WARNING: metrics export file not found: $APTU_CODER_METRICS_EXPORT_FILE" >&2
fi

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
# Post-run verification: grep checks for tsx re-wiring
# ---------------------------------------------------------------------------
CHANGED_FILES_STAT=$(git -C "$RUN_WORKTREE" diff --stat HEAD 2>/dev/null | tail -1 || echo "")
CHANGED_FILES_LIST=$(git -C "$RUN_WORKTREE" diff --name-only HEAD 2>/dev/null || echo "")

MOD_RS_FILE="$RUN_WORKTREE/crates/aptu-coder-core/src/languages/mod.rs"
LANG_RS_FILE="$RUN_WORKTREE/crates/aptu-coder-core/src/lang.rs"

# Five grep checks for tsx re-wiring correctness:
# 1. mod.rs has tsx arm in get_language_info with correct namespace (typescript::)
# 2. mod.rs has tsx arm in get_ts_language with LANGUAGE_TSX
# 3. lang.rs has tsx in EXTENSION_MAP
# 4. lang.rs has tsx in supported_languages
# 5. No spurious pub mod tsx in mod.rs (should only be in feature gate)

GREP_1=false
GREP_2=false
GREP_3=false
GREP_4=false
GREP_5=false

[[ -f "$MOD_RS_FILE" ]] && grep -q '"tsx" => Some(LanguageInfo {' "$MOD_RS_FILE" && grep -q 'element_query: typescript::ELEMENT_QUERY,' "$MOD_RS_FILE" && GREP_1=true || true

[[ -f "$MOD_RS_FILE" ]] && grep -q '"tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX' "$MOD_RS_FILE" && GREP_2=true || true

[[ -f "$LANG_RS_FILE" ]] && grep -q '("tsx", "tsx")' "$LANG_RS_FILE" && GREP_3=true || true

[[ -f "$LANG_RS_FILE" ]] && grep -q '"tsx",' "$LANG_RS_FILE" && GREP_4=true || true

# Check that pub mod tsx is NOT present (should only be in feature gate, not as standalone)
if [[ -f "$MOD_RS_FILE" ]]; then
  ! grep -q '^pub mod tsx;' "$MOD_RS_FILE" && GREP_5=true || true
fi

python3 -c "
import json, sys
tel_path = '$TELEMETRY_FILE'
ver_path = '$VERIFICATION_FILE'
v = {
    'grep_mod_rs_tsx_arm': '$GREP_1' == 'true',
    'grep_mod_rs_language_tsx': '$GREP_2' == 'true',
    'grep_lang_rs_extension': '$GREP_3' == 'true',
    'grep_lang_rs_supported': '$GREP_4' == 'true',
    'grep_no_spurious_pub_mod': '$GREP_5' == 'true',
    'git_diff_stat': '$CHANGED_FILES_STAT',
    'changed_files': [f for f in '''$CHANGED_FILES_LIST'''.splitlines() if f.strip()],
}
with open(ver_path, 'w') as f: json.dump(v, f, indent=2)
if __import__('os').path.exists(tel_path):
    t = json.load(open(tel_path))
    t['verification'] = v
    json.dump(t, open(tel_path, 'w'), indent=2)
all_pass = all([v['grep_mod_rs_tsx_arm'], v['grep_mod_rs_language_tsx'], v['grep_lang_rs_extension'], v['grep_lang_rs_supported'], v['grep_no_spurious_pub_mod']])
print('Verification: grep_checks=' + ('PASS' if all_pass else 'FAIL') + ' files=' + str(len(v['changed_files'])))
"

# ---------------------------------------------------------------------------
# Tool isolation validation
# ---------------------------------------------------------------------------
_WORKTREE_SLUG="${RUN_WORKTREE//\//-}"
SESSION_DIR="${CLAUDE_SESSION_DIR:-$HOME/.claude/projects/${_WORKTREE_SLUG}}"

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

# Edit profile: only 3 tools allowed
EDIT_PROFILE_TOOLS = {
    "mcp__aptu-coder__edit_replace",
    "mcp__aptu-coder__edit_overwrite",
    "mcp__aptu-coder__exec_command",
}
assert len(EDIT_PROFILE_TOOLS) == 3, f"Expected 3 edit-profile tools, got {len(EDIT_PROFILE_TOOLS)}"

# All MCP tools (for validation)
ALL_MCP_TOOLS = {
    "mcp__aptu-coder__analyze_directory",
    "mcp__aptu-coder__analyze_file",
    "mcp__aptu-coder__analyze_module",
    "mcp__aptu-coder__analyze_symbol",
    "mcp__aptu-coder__edit_overwrite",
    "mcp__aptu-coder__edit_replace",
}

# Disabled MCP tools (should not be used in edit profile)
DISABLED_MCP_TOOLS = ALL_MCP_TOOLS - EDIT_PROFILE_TOOLS

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
    # Check for native tools
    forbidden_native = tools_used & NATIVE_TOOLS
    if forbidden_native:
        print(f"ISOLATION FAIL: native tools used in MCP condition: {forbidden_native}", file=sys.stderr)
        sys.exit(1)
    
    # Check for disabled MCP tools
    forbidden_mcp = tools_used & DISABLED_MCP_TOOLS
    if forbidden_mcp:
        print(f"ISOLATION FAIL: disabled MCP tools used in edit-profile condition: {forbidden_mcp}", file=sys.stderr)
        sys.exit(1)
    
    print(f"Edit-profile tools used: {sorted(tools_used & EDIT_PROFILE_TOOLS)}")
    print("ISOLATION PASS")
else:
    print(f"ERROR: unexpected tool set: {expected_tool_set}", file=sys.stderr)
    sys.exit(1)
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
fc  = len(d.get('files_created', []))
fm  = len(d.get('files_modified', []))
qw  = len(d.get('queries_written', []))
tc  = d.get('tool_calls_total', '?')
cb  = d.get('compile_belief', '?')
er  = ','.join(d.get('extension_registrations', []))
print(f'  files_created={fc}  files_modified={fm}  queries={qw}  extensions={er}  compile_belief={cb}  tool_calls={tc}')
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
if [[ -f "$VERIFICATION_FILE" ]]; then
  echo "Verification: $VERIFICATION_FILE"
  python3 -c "
import json
v = json.load(open('$VERIFICATION_FILE'))
print(f'  cargo_test=' + ('PASS' if v.get('cargo_test_passed') else 'FAIL') + f'  spdx={v.get(\"spdx_header_present\")}  files={len(v.get(\"changed_files\",[]))}')
"
fi
if [[ -s "$LOG_FILE" ]]; then
  echo "Log:       $LOG_FILE"
fi
