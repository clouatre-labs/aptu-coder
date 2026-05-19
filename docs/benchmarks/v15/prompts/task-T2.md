List the directory tree of a GitLab repository.

TARGET_ID_PLACEHOLDER determines the target:
- T2: repo=gnome/gtk, path=/ (root), depth=1 (expect ~14 entries)
- T3: repo=gnome/gtk, path=gtk/, depth=1 (expect ~101 entries)

Respond with a JSON object and nothing else:

```json
{
  "run_id": "RUN_ID_PLACEHOLDER",
  "condition": "CONDITION_PLACEHOLDER",
  "target_id": "TARGET_ID_PLACEHOLDER",
  "content_correct": true or false,
  "decode_required": true or false,
  "entry_count": integer (number of entries in the listing),
  "first_entry_seen": "name of first entry in listing",
  "tool_calls_total": integer
}
```

**Scoring:**
- `content_correct`: true if the listing is non-empty and contains expected entries:
  - T2: listing contains at least one of: README.md, meson.build, gtk/, gdk/, meson.options
  - T3: listing contains at least one of: gtkwidget.c, gtkbutton.c, gtklabel.c
- `decode_required`: true if you had to base64-decode or parse a JSON envelope to access the listing
- `entry_count`: the total number of entries returned
- `first_entry_seen`: the name of the first entry in the listing
- `tool_calls_total`: the total number of tool invocations
