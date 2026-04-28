# Archiva

Git-native decision memory for AI coding agents.

Archiva gives coding agents a memory of engineering intent. It records the reasoning behind meaningful code changes: what was chosen, why it was chosen, what alternatives were rejected, and which AST anchor the decision belongs to. The record lives in the repo beside the code, so the next agent, model, or developer can recover the context before editing.

Instead of hoping the next session reads a stale ADR or reverse-engineers a design choice from a diff, Archiva makes the decision trail queryable through a CLI and MCP.

## What You Get

- **Decision memory for agents**: tools can ask why code exists before changing it.
- **Rejected alternatives preserved**: failed approaches stop getting rediscovered every session.
- **Code-anchored rationale**: decisions are attached to functions, classes, exports, and blocks, not brittle line numbers.
- **Low-context session hints**: agents can load compact `.dmap` entries instead of full YAML logs.
- **Local-first storage**: no account, daemon, or hosted service required.
- **MCP-native integration**: works with MCP-capable agent tools using a stdio server.

## Why

Git remembers what changed. It does not remember why an implementation won over the alternatives.

That missing reasoning is especially painful in agentic codebases:

- a later agent sees code but not the constraints behind it
- deliberate choices look accidental
- rejected approaches get retried
- ADRs drift away from the code they explain

Archiva gives coding agents a small local memory layer. It stores decision records in `.decisions/`, indexes them by code anchor instead of fragile line numbers, and exposes the memory through a CLI and MCP tools.

## Benchmarks

Archiva is not claiming model accuracy improvements without a task-specific evaluation. What it can measure directly is context footprint: how much decision state needs to be injected or queried.

Run the included benchmark:

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

Interpretation:

- `.dlog` keeps the full decision record for durable memory.
- `.dmap` gives agents a tiny index for session startup.
- `why` fetches full detail only when the agent is about to touch a relevant anchor.

This is the main design tradeoff: keep durable reasoning in git, but keep default model context small.

## Install

```sh
npm install -g @jalkarna/archiva
```

Verify the CLI:

```sh
archiva --version
```

Archiva requires Node.js 20 or newer.

## Quick Start

Initialize a repository:

```sh
archiva init
```

For Claude Code, also register the MCP server:

```sh
claude mcp add -s local archiva -- archiva mcp
```

After that, agents can call Archiva through MCP:

- before editing, call `why`
- after a meaningful implementation choice, call `write_decision`
- when checking drift, call `ghost_check`

The intended agent loop is simple:

```text
read map -> ask why -> edit code -> write decision -> lint drift
```

## Usage

### Initialize A Project

```sh
archiva init
```

Creates:

- `.decisions/`
- `.claude/settings.json` with Archiva hooks and MCP config
- an `AGENTS.md` decision logging instruction block

By default, decision files are intended to be committed with the code. If a project wants local-only decision logs, run:

```sh
archiva init --gitignore-decisions
```

### Check Decision Health

```sh
archiva status
```

Shows decision counts and drift/orphan health across the repo.

### Ask Why Code Exists

By line:

```sh
archiva why src/auth/session.ts 52
```

By anchor:

```sh
archiva why src/auth/session.ts fn:processCheckout
```

### View Decision History

```sh
archiva history src/auth/session.ts fn:processCheckout
```

Shows the supersession chain for an anchor.

### Lint Decision State

```sh
archiva lint
```

Rules include:

- stale decisions when code fingerprints change
- orphan decisions when anchors disappear
- complex undecided functions
- stale decisions that were not superseded

Safe orphan cleanup:

```sh
archiva lint --fix
```

### Run MCP Server

```sh
archiva mcp
```

This starts the stdio MCP server. Most users do not run it manually; MCP clients launch it from config.

### Run Hooks Manually

Session context injection:

```sh
archiva hooks session-start
```

Re-anchor a file after edits:

```sh
ARCHIVA_FILE=src/auth/session.ts archiva hooks post-tool-use
```

or:

```sh
archiva hooks post-tool-use src/auth/session.ts
```

## MCP Configuration

Use this config in MCP-capable tools that accept stdio servers:

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

For project-local Claude settings, `archiva init` writes:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "archiva hooks session-start"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "archiva hooks post-tool-use"
          }
        ]
      }
    ]
  },
  "mcpServers": {
    "archiva": {
      "command": "archiva",
      "args": ["mcp"]
    }
  }
}
```

## MCP Tools

### `write_decision`

Records a decision for a file and anchor.

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

`supersedes` is optional. When present, it must reference an existing decision id returned by `why`.

### `why`

Reads decision memory before editing.

```json
{
  "file": "src/auth/session.ts",
  "anchor": "fn:processCheckout"
}
```

If `anchor` is omitted, Archiva returns all decisions for the file.

### `ghost_check`

Checks a file for stale or orphaned decision state.

```json
{
  "file": "src/auth/session.ts"
}
```

## File Format

For this source file:

```text
src/auth/session.ts
```

Archiva writes:

```text
.decisions/src/auth/session.ts.dlog
.decisions/src/auth/session.ts.dmap
```

`.dlog` is the full YAML decision log. `.dmap` is a compact spatial map used for low-token context injection.

Example `.dmap`:

```text
42-67:fn:processCheckout
89-94:block:if_version_mismatch:STALE
```

## Agent Instructions

Archiva works best when agents are explicitly told to use it:

```md
## Decision Logging (Archiva)

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

`archiva init` adds a fuller version of this block to `AGENTS.md`.

## CI

```yaml
name: Decision Health

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

## Current Scope

Archiva currently supports:

- TypeScript and JavaScript anchor extraction
- YAML `.dlog` files
- compact `.dmap` files
- local re-anchoring
- linting
- Claude Code hooks
- stdio MCP tools

Future hardening work:

- Python anchors
- richer inline `@decision` comment management
- first-class setup docs for more IDEs and agent CLIs

## License

MIT
