## Decision Logging (Archiva)

This project uses Archiva for decision tracking.

### Before modifying any file
- Read the decision map injected at session start (prefixed `[Archiva]`)
- Or call the `why` MCP tool: `why(file, anchor)`
- Do NOT modify code marked with a decision without reading it first

### After any non-trivial implementation choice
Call `write_decision` with:
- `file` and `anchor` (function or block name)
- `chose` - what approach you selected
- `because` - the specific reason, not a generic description
- `rejected` - every alternative you considered, with specific disqualifying reasons

Required for: algorithm choices, concurrency patterns, error handling strategies,
any point where you weighed 2+ approaches.

Not required for: imports, type declarations, formatting, variable names.

If changing code that has an existing decision:
- If your change preserves the reasoning -> keep the decision, update `lines_hint`
- If your change invalidates the reasoning -> call `write_decision` with `supersedes: <id>`
