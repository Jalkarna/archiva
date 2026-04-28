# Archiva

Git-native decision memory for AI coding agents.

Archiva records what was chosen, why it was chosen, what was rejected, and which AST anchor the decision belongs to. It gives MCP-capable coding agents a local memory layer that lives beside the code instead of in a chat transcript.

## Install

```sh
npm install -g archiva
```

For local development:

```sh
npm install
npm run build
node bin/archiva.js --help
```

## Commands

```sh
archiva init
archiva status
archiva why src/auth/session.ts 52
archiva why src/auth/session.ts fn:processCheckout
archiva history src/auth/session.ts fn:processCheckout
archiva lint
archiva hooks session-start
ARCHIVA_FILE=src/auth/session.ts archiva hooks post-tool-use
archiva mcp
```

`archiva init` creates `.decisions/`, writes Claude hook/MCP settings, and appends Archiva instructions to `AGENTS.md`. Decision logs are intended to be tracked in git by default. Use `--gitignore-decisions` if the project explicitly wants local-only decisions.

## Claude Code

```sh
archiva init
claude mcp add -s local archiva -- archiva mcp
```

Any MCP client can use the same stdio server command:

```sh
archiva mcp
```

## Decision Files

For a source file:

```text
src/auth/session.ts
```

Archiva writes:

```text
.decisions/src/auth/session.ts.dlog
.decisions/src/auth/session.ts.dmap
```

The `.dlog` file is YAML and contains full decision records. The `.dmap` file is a compact line map used for low-token session context.

## MCP Tools

Archiva exposes three stdio MCP tools:

- `write_decision`
- `why`
- `ghost_check`

Example input for `write_decision`:

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
  ]
}
```

## CI

```yaml
- name: Check decision health
  run: npx archiva lint
```

## Current Scope

The first implementation supports TypeScript and JavaScript anchors, YAML decision logs, compact maps, local re-anchoring, linting, Claude hooks, and stdio MCP tools. Python support and richer inline `@decision` comment management remain future hardening work.
