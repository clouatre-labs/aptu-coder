ERROR_TYPE_PLACEHOLDER determines the error to trigger:
- missing_token: attempt to fetch gtk/gtkwidget.c from gnome/gtk with no GITLAB_TOKEN set (unset the env var before the call if possible, or pass an empty string as the token)
- not_found: attempt to fetch a nonexistent file path (nonexistent/path/does_not_exist.xyz) from gnome/gtk

Use TOOL_OR_COMMAND_PLACEHOLDER to make the fetch attempt.

Respond with a JSON object and nothing else:

```json
{
  "run_id": "RUN_ID_PLACEHOLDER",
  "condition": "CONDITION_PLACEHOLDER",
  "error_type": "ERROR_TYPE_PLACEHOLDER",
  "error_graceful": true or false,
  "error_message": "the error message returned (truncated to 200 chars)"
}
```

**Scoring:**
- `error_graceful`: true if the tool/command returned a clear, actionable error without crashing or hanging
- `error_message`: the error message you received (truncated to 200 characters)

**Expected errors:**
- missing_token: "401 Unauthorized" or "GITLAB_TOKEN not set" or similar
- not_found: "404 Not Found" or "file not found" or similar
