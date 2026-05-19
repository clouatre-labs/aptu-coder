List the directory tree of gitlab-org/gitlab on GitLab at depth=2 from the root.

Respond with a JSON object and nothing else:

```json
{
  "run_id": "RUN_ID_PLACEHOLDER",
  "condition": "CONDITION_PLACEHOLDER",
  "target_id": "T4",
  "content_correct": true or false,
  "decode_required": true or false,
  "entry_count": integer (total entries returned across all pages/levels),
  "first_entry_seen": "name of first top-level entry",
  "tool_calls_total": integer
}
```

**Scoring:**
- `content_correct`: true if the listing is non-empty and contains at least one of: app/, lib/, spec/, config/, doc/
- `decode_required`: true if you had to base64-decode or parse a JSON envelope
- `entry_count`: the total number of entries returned (including subdirectory entries)
- `first_entry_seen`: the name of the first top-level entry
- `tool_calls_total`: the total number of tool invocations (may be >1 for depth=2 pagination)

**Note:** T4 may exceed 30 seconds wall-clock time; if so, the run will be discarded and must be re-run.
