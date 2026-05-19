Fetch the file gtk/gtkwidget.c from the gnome/gtk repository on GitLab (HEAD ref).

TARGET_ID_PLACEHOLDER determines the fetch mode:
- T1: fetch the full file
- T1b: fetch lines 1-50 only (use line_range="1-50" for MCP; pipe curl output through `sed -n '1,50p'` for curl)

Respond with a JSON object and nothing else:

```json
{
  "run_id": "RUN_ID_PLACEHOLDER",
  "condition": "CONDITION_PLACEHOLDER",
  "target_id": "TARGET_ID_PLACEHOLDER",
  "content_correct": true or false,
  "decode_required": true or false,
  "first_line_seen": "first non-empty line of the fetched content (truncated to 80 chars)",
  "content_chars": integer (character count of fetched content as seen by agent),
  "tool_calls_total": integer
}
```

**Scoring:**
- `content_correct`: true if the fetched content begins with (after stripping BOM/whitespace): `/* GTK - The GIMP Toolkit`
- `decode_required`: true if you had to base64-decode or manually parse a JSON envelope to access the text content
- `first_line_seen`: the first non-empty line you observed (truncated to 80 characters)
- `content_chars`: the number of characters in the content as you received it (after any decoding)
- `tool_calls_total`: the total number of tool invocations (1 for MCP remote_file; 1+ for curl + decode)
