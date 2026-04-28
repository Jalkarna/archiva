# Archiva

Git-native decision memory for AI coding agents.

Every agent session starts cold. It doesn't know why that auth check is there, which approaches were tried and dropped, or that the team explicitly decided against the obvious solution two months ago. Archiva stores those decisions beside the code so agents can read them before touching anything.

It runs as a CLI and MCP server. Tools like Claude Code, Codex, Cursor, and other MCP-capable agents can call `why` before editing a file and `write_decision` after making a meaningful choice.

## Why agents need it

Git remembers what changed. It doesn't remember why.

That gap causes predictable problems:

- the next session reopens the same problem from scratch
- deliberate constraints look like accidental complexity
- rejected approaches get tried again
- ADRs drift away from the code they describe
- multi-agent work loses the thread between handoffs

Archiva stores reasoning in `.decisions/` beside the code, indexes it by source anchors, and exposes it through `archiva why`, `archiva history`, `archiva lint`, and MCP tools.

## What it does

Decision records live in your repo, versioned with the code they describe, queryable by file and anchor. The design is intentionally repo-native: no daemon, no hosted service, no account.

Measured:

- **94.4% smaller decision context** with compact `.dmap` startup maps versus full `.dlog` records
- **58.9% smaller session-start context** on the included fixture, with full decision detail still available on demand
- **100% pass rate preserved** on a real Terminal-Bench smoke run where Claude Code used Archiva MCP, called `why`, wrote `dec_001`, and passed task validation

For decision-heavy agent work, the expected gains are fewer repeated-decision failures, lower token waste, and better patch consistency because agents can see rejected alternatives before reintroducing them. Those are targets for the next seeded SWE-bench/Terminal-Bench runs, not current leaderboard claims. The measured claim today is integration correctness and context reduction.

## Features

- Decision memory for agents: tools can ask why code exists before changing it
- Rejected alternatives preserved: failed approaches stop getting rediscovered every session
- Code-anchored rationale: decisions attached to functions, classes, exports, and blocks, not brittle line numbers
- Low-context session hints: agents load compact `.dmap` entries instead of full YAML logs
- Local-first: no account, daemon, or hosted service
- MCP-native: works with any MCP-capable agent using a stdio server

## Benchmarks

Archiva should be judged on outcomes: does the same agent finish more tasks, avoid more regressions, and spend less context/tool budget when it can read and write decision memory?

Public benchmarks that fit this question:

- **Terminal-Bench** for end-to-end terminal tasks with task-specific tests
- **SWE-bench / SWE-bench Verified** for real GitHub issue-resolution tasks with execution-based grading

Actual Terminal-Bench smoke result (`csv-to-parquet`, Claude Code 2.1.121, `claude-sonnet-4-6`):

| | passed | trial time | cost |
|---|---|---|---|
| Baseline Claude Code | yes | 128.6s | $0.1197 |
| Claude Code + Archiva MCP | yes | 177.6s | $0.2028 |

Archiva MCP was connected, `why` was called before work, `write_decision` recorded `dec_001`, and Terminal-Bench validation passed. See [docs/benchmark-results.md](docs/benchmark-results.md) for raw run IDs and interpretation.

The smoke result proves Archiva works inside a real benchmark harness. It doesn't show an accuracy lift because the task had no prior decision memory to exploit. The next target is a seeded A/B where the treatment repo contains useful `.decisions/` history.

The clean A/B design:

```text
same benchmark + same model + same agent + same timeout

baseline:  Archiva disabled
treatment: Archiva MCP enabled, AGENTS.md includes Archiva instructions,
           tasks use repos with prior decisions across sessions
```

Useful metrics: resolved task rate, token/tool calls per resolved task, regression count, decision reads/writes per task, stale/orphan decisions after completion.

See [docs/benchmarks.md](docs/benchmarks.md) for the full protocol.

Archiva also includes a context-footprint benchmark that measures how compactly it exposes decision memory, not model quality:

```sh
npm run benchmark
```

Current fixture result:

```text
decisions: 3
full .dlog bytes: 1361
compact .dmap bytes: 76
session-start bytes: 559
.dmap vs .dlog byte reduction: 94.4%
session context vs .dlog byte reduction: 58.9%
average .dmap bytes per decision: 25.3
```

`.dlog` keeps the full record in git. `.dmap` gives agents a tiny index at session start. `why` fetches the full record only when the agent is about to touch a relevant anchor. That's the core design tradeoff: durable reasoning in git, small default context for the model.

## Install

```sh
npm install -g @jalkarna/archiva
archiva --version
```

Requires Node.js 20 or newer.

## Quick start

```sh
archiva init
```

For Claude Code, also register the MCP server:

```sh
claude mcp add -s local archiva -- archiva mcp
```

Agents can then call Archiva through MCP:

- before editing, call `why`
- after a meaningful implementation choice, call `write_decision`
- when checking drift, call `ghost_check`

The intended loop:

```text
read map -> ask why -> edit code -> write decision -> lint drift
```

## Usage

### Initialize a project

```sh
archiva init
```

Creates:

- `.decisions/`
- `.claude/settings.json` with Archiva hooks and MCP config
- an `AGENTS.md` decision logging instruction block

Decision files are committed with the code by default. For local-only logs:

```sh
archiva init --gitignore-decisions
```

### Check decision health

```sh
archiva status
```

Shows decision counts and drift/orphan health across the repo.

### Ask why code exists

By line:

```sh
archiva why src/auth/session.ts 52
```

By anchor:

```sh
archiva why src/auth/session.ts fn:processCheckout
```

### View decision history

```sh
archiva history src/auth/session.ts fn:processCheckout
```

Shows the supersession chain for an anchor.

### Lint decision state

```sh
archiva lint
```

Checks for:

- stale decisions when code fingerprints change
- orphan decisions when anchors disappear
- complex undecided functions
- stale decisions that were not superseded

Safe orphan cleanup:

```sh
archiva lint --fix
```

### Run the MCP server

```sh
archiva mcp
```

Starts the stdio MCP server. You typically don't run this manually; MCP clients launch it from config.

### Run hooks manually

Session context injection:

```sh
archiva hooks session-start
```

Re-anchor a file after edits:

```sh
ARCHIVA_FILE=src/auth/session.ts archiva hooks post-tool-use
# or
archiva hooks post-tool-use src/auth/session.ts
```

## MCP configuration

For MCP-capable tools that accept stdio servers:

```json
{
  "mcpServers": {
    "archiva": {
      "command": "archiva",
      "args": ["mcp"]
    }
  }
}
```

For tools that prefer a server object only:

```json
{
  "command": "archiva",
  "args": ["mcp"]
}
```

`archiva init` writes the following to `.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "archiva hooks session-start" }] }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [{ "type": "command", "command": "archiva hooks post-tool-use" }]
      }
    ]
  },
  "mcpServers": {
    "archiva": { "command": "archiva", "args": ["mcp"] }
  }
}
```

## MCP tools

### `write_decision`

Records a decision for a file and anchor:

```json
{
  "file": "src/auth/session.ts",
  "anchor": "fn:processCheckout",
  "lines": [42, 67],
  "chose": "optimistic locking via version field increment",
  "because": "checkout and inventory deduction can race under concurrent carts",
  "rejected": [
    {
      "approach": "SELECT FOR UPDATE",
      "reason": "deadlocks under concurrent carts touching the same SKU"
    }
  ],
  "expires_if": "inventory service migrates to event sourcing",
  "supersedes": "dec_001"
}
```

`supersedes` is optional. When present, it must reference an existing decision id from `why`.

### `why`

Reads decision memory before editing:

```json
{
  "file": "src/auth/session.ts",
  "anchor": "fn:processCheckout"
}
```

Omit `anchor` to get all decisions for the file.

### `ghost_check`

Checks for stale or orphaned decisions:

```json
{
  "file": "src/auth/session.ts"
}
```

## File format

For `src/auth/session.ts`, Archiva writes:

```text
.decisions/src/auth/session.ts.dlog
.decisions/src/auth/session.ts.dmap
```

`.dlog` is the full YAML decision log. `.dmap` is a compact spatial map for low-token context injection.

Example `.dmap`:

```text
42-67:fn:processCheckout
89-94:block:if_version_mismatch:STALE
```

## Agent instructions

Archiva works best when agents are explicitly told to use it. Add this to `AGENTS.md` or your system prompt:

```md
## Decision logging (Archiva)

Before modifying a file, read the decision map injected at session start or call the `why` MCP tool.

After any non-trivial implementation choice, call `write_decision` with:
- `file` and `anchor`
- `chose`
- `because`
- `rejected`

If changing code with an existing decision:
- preserve the decision if the reasoning still holds
- call `write_decision` with `supersedes` if the reasoning changed
```

`archiva init` adds a fuller version of this to `AGENTS.md`.

## CI

```yaml
name: Decision health

on:
  pull_request:
  push:
    branches: [main]

jobs:
  archiva:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: "20"
      - run: npx @jalkarna/archiva lint
```

## Development

```sh
npm install
npm run check
npm test
npm run build
node bin/archiva.js --help
```

## Current scope

Supports today:

- TypeScript and JavaScript anchor extraction
- YAML `.dlog` files and compact `.dmap` files
- local re-anchoring
- linting
- Claude Code hooks
- stdio MCP tools

Planned:

- Python anchors
- richer inline `@decision` comment management
- setup docs for more IDEs and agent CLIs

## License

MIT
