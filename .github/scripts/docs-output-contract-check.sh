#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 aptu-coder contributors
# SPDX-License-Identifier: Apache-2.0
# Denylist check for stale MCP tool-surface claims in docs.
# Advisory in CI (see docs-output-contract job in ci.yml): the denylist
# is maintained via the PR template's tool-surface checklist item.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

stale=0

check_phrase() {
  local phrase="$1"
  local skip_context="${2:-}"
  local file line_no matched_text
  while IFS= read -r line; do
    file="${line%%:*}"
    line_no="${line#*:}"
    line_no="${line_no%%:*}"
    matched_text="${line#*:}"
    matched_text="${matched_text#*:}"
    if [[ -n "$skip_context" ]] && grep -Eiq "$skip_context" <<<"$matched_text"; then
      continue
    fi
    echo "ERROR: $file:$line_no: stale tool-surface claim: \"$phrase\" -> $matched_text" >&2
    stale=1
  done < <(grep -RniF -- "$phrase" "$REPO_ROOT/README.md" "$REPO_ROOT/docs" --include='*.md' 2>/dev/null || true)
}

check_phrase "structuredContent mirrors"
check_phrase "both text and structuredContent"
check_phrase "populates three fields"
check_phrase "follow the 4-step"
check_phrase "per-tool output schema" 'removed|historical|later'

if [[ "$stale" -ne 0 ]]; then
  echo "Docs contain stale MCP tool-surface claims. Update README.md and docs/ to match the current tool surface, or reword to removed/historical context." >&2
  exit 1
fi

echo "OK: no stale tool-surface claims found in docs."
