#!/usr/bin/env bash
# wave11 benchmark runner
# Usage: ./docs/benchmarks/wave11/run.sh <RUN_ID>
# Example: ./docs/benchmarks/wave11/run.sh G-pilot
#
# Conditions: G (sonnet + full 7 tools) and H (haiku + full 7 tools)
# No APTU_CODER_PROFILE -- all 7 MCP tools available.
# Runner: claude CLI, --strict-mcp-config (no native tools)
# CWD: worktree at /tmp/wave11-run-<RUN_ID> (isolates from repo AGENTS.md)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RUNS_DIR="$SCRIPT_DIR/results/runs"
PROMPTS_DIR="$SCRIPT_DIR/prompts"

mkdir -p "$RUNS_DIR"

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <RUN_ID>   (e.g. G-pilot, H-scored-1)" >&2
  exit 1
fi

RUN_ID="$1"
CONDITION="${RUN_ID:0:1}"

case "$CONDITION" in
  G) SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-g-mcp-full-sonnet.md"
     MODEL="global.anthropic.claude-sonnet-4-6" ;;
  H) SYSTEM_PROMPT_FILE="$PROMPTS_DIR/condition-h-mcp-full-haiku.md"
     MODEL="global.anthropic.claude-haiku-4-5-20251001-v1:0" ;;
  *) echo "Unknown condition '$CONDITION'. RUN_ID must start with G or H." >&2; exit 1 ;;
esac

WORKTREE="/tmp/wave11-run-${RUN_ID}"
METRICS_FILE="/tmp/wave11-metrics-${RUN_ID}.json"
MCP_CONFIG_FILE="/tmp/wave11-mcp-config-${RUN_ID}.json"

LOG_FILE="$RUNS_DIR/${RUN_ID}-log.txt"
OUTPUT_FILE="$RUNS_DIR/${RUN_ID}-output.json"
TELEMETRY_FILE="$RUNS_DIR/${RUN_ID}-telemetry.json"
VERIFICATION_FILE="$RUNS_DIR/${RUN_ID}-verification.json"

MOD_RS="$WORKTREE/crates/aptu-coder-core/src/languages/mod.rs"
LANG_RS="$WORKTREE/crates/aptu-coder-core/src/lang.rs"

echo "=== wave11: $RUN_ID (condition $CONDITION, model $MODEL) ==="

# ---- 1. Fresh worktree ----
git -C "$REPO_ROOT" fetch -q origin
[[ -d "$WORKTREE" ]] && git -C "$REPO_ROOT" worktree remove --force "$WORKTREE" 2>/dev/null || true
git -C "$REPO_ROOT" worktree add "$WORKTREE" origin/main -q
echo "worktree HEAD: $(git -C "$WORKTREE" rev-parse --short HEAD)"

# ---- 2. Strip tsx wiring ----
python3 - "$MOD_RS" <<'PYEOF'
import sys, re
path = sys.argv[1]
text = open(path).read()
text = re.sub(
    r'\s+#\[cfg\(feature = "lang-tsx"\)\]\s+"tsx" => Some\(LanguageInfo \{[^}]+extract_inheritance: Some\(typescript::extract_inheritance\),\s+\}\),',
    '', text)
text = re.sub(
    r'\s+#\[cfg\(feature = "lang-tsx"\)\]\s+"tsx" => Some\(tree_sitter_typescript::LANGUAGE_TSX\.into\(\)\),',
    '', text)
open(path, 'w').write(text)
PYEOF
python3 - "$LANG_RS" <<'PYEOF'
import sys, re
path = sys.argv[1]
text = open(path).read()
text = re.sub(r'\s+#\[cfg\(feature = "lang-tsx"\)\]\s+\("tsx", "tsx"\),', '', text)
text = re.sub(r'\s+#\[cfg\(feature = "lang-tsx"\)\]\s+"tsx",', '', text)
open(path, 'w').write(text)
PYEOF
grep -q '"tsx" => Some(LanguageInfo'                        "$MOD_RS" && { echo "ERROR: mod.rs strip failed (LanguageInfo arm)" >&2; exit 1; } || true
grep -q '"tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX' "$MOD_RS" && { echo "ERROR: mod.rs strip failed (LANGUAGE_TSX arm)" >&2; exit 1; } || true
grep -q '("tsx", "tsx")'                                    "$LANG_RS" && { echo "ERROR: lang.rs strip failed (extension map)" >&2; exit 1; } || true
echo "strip OK"

# ---- 3. Per-run MCP config (no APTU_CODER_PROFILE -- full 7 tools) ----
cat > "$MCP_CONFIG_FILE" <<EOF
{
  "mcpServers": {
    "aptu-coder": {
      "type": "stdio",
      "command": "aptu-coder",
      "args": [],
      "env": {
        "APTU_CODER_METRICS_EXPORT_FILE": "$METRICS_FILE"
      }
    }
  }
}
EOF

SYSTEM_PROMPT=$(sed "s|REPO_PATH_PLACEHOLDER|$WORKTREE|g" "$SYSTEM_PROMPT_FILE")
TASK_PROMPT=$(sed \
  -e "s|REPO_PATH_PLACEHOLDER|$WORKTREE|g" \
  -e "s|RUN_ID_PLACEHOLDER|$RUN_ID|g" \
  -e "s|CONDITION_PLACEHOLDER|$CONDITION|g" \
  "$PROMPTS_DIR/task.md")

# ---- 4. Run ----
echo "started: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
touch /tmp/.wave11-marker-${RUN_ID}

(cd "$WORKTREE" && DISABLE_PROMPT_CACHING=1 claude \
  -p \
  --output-format stream-json \
  --model "$MODEL" \
  --mcp-config "$MCP_CONFIG_FILE" \
  --strict-mcp-config \
  --dangerously-skip-permissions \
  --system-prompt "$SYSTEM_PROMPT" \
  "$TASK_PROMPT") > "$LOG_FILE" 2>&1

echo "completed: $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ---- 5. Telemetry from result event ----
python3 - "$LOG_FILE" "$TELEMETRY_FILE" <<'PYEOF'
import sys, json
log_path, out_path = sys.argv[1], sys.argv[2]
result = {}
for line in open(log_path):
    try:
        o = json.loads(line.strip())
        if isinstance(o, dict) and o.get("type") == "result":
            result = o
    except Exception:
        pass
usage = result.get("usage", {})
t = {
    "wall_time_ms":          result.get("duration_ms", 0),
    "api_time_ms":           result.get("duration_api_ms", 0),
    "num_turns":             result.get("num_turns", 0),
    "cost_usd":              result.get("total_cost_usd", 0.0),
    "input_tokens":          usage.get("input_tokens", 0),
    "output_tokens":         usage.get("output_tokens", 0),
    "cache_read_tokens":     usage.get("cache_read_input_tokens", 0),
    "cache_creation_tokens": usage.get("cache_creation_input_tokens", 0),
}
open(out_path, "w").write(json.dumps(t, indent=2))
print(f"turns={t['num_turns']} input={t['input_tokens']} output={t['output_tokens']} cost=${t['cost_usd']:.4f}")
PYEOF

# ---- 6. Verification ----
V_ARM=false; V_LANG=false; V_EXT=false; V_SUP=false; V_NO_SPURIOUS=true
grep -q '"tsx" => Some(LanguageInfo {'                "$MOD_RS" && grep -q 'typescript::ELEMENT_QUERY' "$MOD_RS" && V_ARM=true || true
grep -q '"tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX' "$MOD_RS" && V_LANG=true || true
grep -q '("tsx", "tsx")'                              "$LANG_RS" && V_EXT=true  || true
grep -q '"tsx",'                                      "$LANG_RS" && V_SUP=true  || true
grep -q '^pub mod tsx;'                               "$MOD_RS"  && V_NO_SPURIOUS=false || true

DIFF_STAT=$(git -C "$WORKTREE" diff --stat HEAD 2>/dev/null || echo "")
CHANGED=$(git -C "$WORKTREE" diff --name-only HEAD 2>/dev/null | python3 -c "import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))")

cat > "$VERIFICATION_FILE" <<EOF
{
  "grep_mod_rs_tsx_arm": $V_ARM,
  "grep_mod_rs_language_tsx": $V_LANG,
  "grep_lang_rs_extension": $V_EXT,
  "grep_lang_rs_supported": $V_SUP,
  "grep_no_spurious_pub_mod": $V_NO_SPURIOUS,
  "git_diff_stat": $(echo "$DIFF_STAT" | python3 -c "import sys,json; print(json.dumps(sys.stdin.read().strip()))"),
  "changed_files": $CHANGED
}
EOF

ALL_PASS=false
[[ "$V_ARM" == "true" && "$V_LANG" == "true" && "$V_EXT" == "true" && "$V_SUP" == "true" && "$V_NO_SPURIOUS" == "true" ]] && ALL_PASS=true
echo "all_pass=$ALL_PASS"
cat "$VERIFICATION_FILE"

# ---- 7. Agent output JSON from session JSONL ----
_REAL_WORKTREE="$(cd "$WORKTREE" 2>/dev/null && pwd -P || echo "$WORKTREE")" || true
_SLUG="${_REAL_WORKTREE//\//-}"
SESSION_DIR="$HOME/.claude/projects/${_SLUG}"
LATEST_JSONL=""
[[ -d "$SESSION_DIR" ]] && LATEST_JSONL=$(find "$SESSION_DIR" -name "*.jsonl" -newer "/tmp/.wave11-marker-${RUN_ID}" 2>/dev/null | xargs ls -t 2>/dev/null | head -1) || true

if [[ -n "$LATEST_JSONL" ]]; then
  cp "$LATEST_JSONL" "$RUNS_DIR/${RUN_ID}-session.jsonl"
  python3 - "$LATEST_JSONL" "$OUTPUT_FILE" "$RUN_ID" "$CONDITION" <<'PYEOF'
import sys, json, re
jsonl_path, out_path, run_id, condition = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
texts = []
for line in open(jsonl_path):
    try:
        obj = json.loads(line)
        if obj.get("type") == "assistant":
            for block in obj.get("message", {}).get("content", []):
                if isinstance(block, dict) and block.get("type") == "text":
                    texts.append(block.get("text", ""))
    except Exception:
        pass
full = "\n".join(texts)
best = None
for m in re.finditer(r'\{', full):
    s = m.start(); depth = 0
    for i, ch in enumerate(full[s:]):
        if ch == '{': depth += 1
        elif ch == '}':
            depth -= 1
            if depth == 0:
                try:
                    obj = json.loads(full[s:s+i+1])
                    if isinstance(obj, dict) and "tsx_wiring_complete" in obj:
                        best = obj
                except Exception:
                    pass
                break
if best is None:
    best = {"run_id": run_id, "condition": condition, "tsx_wiring_complete": False, "parse_error": "no output JSON"}
open(out_path, "w").write(json.dumps(best, indent=2))
PYEOF
else
  echo "WARNING: no session JSONL found in $SESSION_DIR" >&2
  echo '{"run_id":"'"$RUN_ID"'","condition":"'"$CONDITION"'","tsx_wiring_complete":false,"parse_error":"no session jsonl"}' > "$OUTPUT_FILE"
fi

# ---- Cleanup ----
git -C "$REPO_ROOT" worktree remove --force "$WORKTREE" 2>/dev/null || rm -rf "$WORKTREE"
rm -f "$MCP_CONFIG_FILE" "/tmp/.wave11-marker-${RUN_ID}"

echo ""
echo "=== $RUN_ID done: all_pass=$ALL_PASS ==="
