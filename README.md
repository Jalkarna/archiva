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

Archiva keeps full decision detail available when an agent asks for it, while `.dmap` files provide compact startup context for session hooks.

## How it compares

Most AI memory tools are broad recall systems. They store preferences, notes, conversations, embeddings, graph entities, or whole session traces so an agent can remember more across tools.

Archiva is deliberately narrower: it stores **why a specific piece of code exists**. The unit of memory is not a chat message or semantic note. It is a decision record attached to a source file, anchor, line range, fingerprint, and supersession chain.

| Tool category | What it is good at | How Archiva is different |
|---|---|---|
| [Supermemory](https://supermemory.ai/docs/supermemory-mcp/introduction) | Cross-app personal memory through a hosted memory API and MCP server. | Archiva is repo-local and git-native. It does not try to remember everything about the user; it records engineering decisions beside the code. |
| [OpenMemory](https://mem0.ai/openmemory) / Mem0 | Project-scoped coding preferences and automatically retrieved agent context. | Archiva does not infer broad preferences. It captures explicit implementation rationale, rejected alternatives, and decision history for code anchors. |
| [ICM](https://www.rtk-ai.app/icm.html) | A full memory runtime with hybrid search, temporal decay, hooks, and many MCP tools across editors. | Archiva is small and purpose-built: no vector DB, no semantic memory engine, no global recall layer. It is a decision log and drift checker for repositories. |
| [Basic Memory](https://docs.basicmemory.com/reference/mcp-tools-reference) | Markdown knowledge management, notes, search, and project knowledge bases. | Archiva is structured around code files and anchors, with `.dlog` and `.dmap` files that can be linted, re-anchored, and committed with source changes. |
| [Zep / Graphiti MCP](https://www.getzep.com/product/knowledge-graph-mcp/) and graph memory tools | Temporal knowledge graphs, entity relationships, semantic retrieval, and evolving context. | Archiva avoids building a general knowledge graph. It answers a narrower question agents hit constantly: "what decision explains this code, and what should I not repeat?" |
| ADRs and docs | Human-written architecture records and design notes. | Archiva is closer to the code path agents actually touch. Decisions are queryable by file/anchor, can be marked stale when fingerprints drift, and can be superseded over time. |

Archiva is best used alongside broad memory tools, not necessarily instead of them. Use a general memory layer for user preferences, session summaries, and cross-project recall. Use Archiva for code-level decision provenance that should move with the repository.

## Features

- Decision memory for agents: tools can ask why code exists before changing it
- Rejected alternatives preserved: failed approaches stop getting rediscovered every session
- Code-anchored rationale: decisions attached to functions, classes, exports, and blocks, not brittle line numbers
- Low-context session hints: agents load compact `.dmap` entries instead of full YAML logs
- Local-first: no account, daemon, or hosted service
- MCP-native: works with any MCP-capable agent using a stdio server
## Install

```sh
npm install -g @jalkarna/archiva
archiva --version
```

The installed CLI is a native Rust binary. Node.js is only required when
installing through npm or working on the TypeScript compatibility harness in
this repository; the `archiva` command itself runs as a native binary after
installation.

The npm package selects a platform-specific native package at install time:

| Platform | Architectures |
|---|---|
| Linux glibc | x64, arm64 |
| Linux musl | x64, arm64 |
| macOS | x64, arm64 |
| Windows MSVC | x64 |

Install with optional dependencies and lifecycle scripts enabled. npm options
such as `--omit=optional` or `--ignore-scripts` prevent the native binary from
being selected.

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

### Compatibility notes

The native Rust CLI intentionally validates project-relative file paths more
strictly than the original TypeScript implementation. Common tool spellings
such as `./src/a.ts`, `.//src/a.ts`, and `src\a.ts` are normalized to the same
decision identity, while traversal, internal dot segments, absolute paths,
Windows drive or UNC prefixes, reserved Windows device names, Windows-invalid
characters, and trailing dot or space segments are rejected. This keeps
decision reads and writes inside the project and makes stored paths portable
across platforms.

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

Under Claude Code the `post-tool-use` hook is installed with no arguments and
receives the tool payload as JSON on stdin; Archiva reads `tool_input.file_path`
from it and re-anchors that file automatically. Payloads for non-file tools, or
files outside the project, are a clean no-op so the hook never disrupts the
agent.

### Diagnostics

Archiva is silent by default. To trace automatic recovery (corrupt-file skips,
`.dmap` repair, stale-lock takeover, git-baseline fallback), raise the log level
— diagnostics always go to stderr, never stdout, so they never corrupt command
output or the MCP JSON-RPC stream:

```sh
archiva --verbose status          # most verbose (trace)
ARCHIVA_LOG=warn archiva status   # error | warn | info | debug | trace
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

Omit `anchor` to get all decisions for the file. Pass `line` (a positive
integer) instead of `anchor` to look up the decision covering that line:

```json
{
  "file": "src/auth/session.ts",
  "line": 47
}
```

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
      - uses: actions/checkout@v7
      - uses: actions/setup-node@v6
        with:
          node-version: "20"
      - run: npx @jalkarna/archiva lint
```

## Development

```sh
npm install
npm run build:rust
npm run check
npm test
npm run build
target/release/archiva --help
```

Development requires Rust 1.96.0 with `rustfmt`.

CI runs the Rust crate on Ubuntu, macOS, and Windows, then builds and smoke
tests the npm native package matrix. Release publishing runs heavy validation
before publishing native packages, publishes the meta package only after those
native packages are available, and smoke tests the published install across the
supported glibc, musl, macOS, and Windows targets.

Architecture and validation records:

- [Archiva v2 Rust Architecture](docs/archiva-v2-architecture.md)
- [Archiva v2 Review Status](docs/archiva-v2-review-status.md)

### Heavy validation

These commands are intended for release or port-validation work rather than the
fast inner loop:

```sh
npm run differential:release
npm run stress:soak
npm run benchmark:compare
npm run scale:smoke
ARCHIVA_SCALE_CORPUS_ROOT=/path/to/repo npm run scale:corpus
npm run audit:v2
```

`scale:smoke` generates synthetic projects for a larger Rust-only profile and
a smaller TypeScript-vs-Rust artifact parity profile. Tune the main profile
with `ARCHIVA_SCALE_FILES`, `ARCHIVA_SCALE_DECISIONS`,
`ARCHIVA_SCALE_DECISIONS_PER_FILE`, and `ARCHIVA_SCALE_MUTATE_FILES`. Tune the
parity profile with `ARCHIVA_SCALE_PARITY_FILES`,
`ARCHIVA_SCALE_PARITY_DECISIONS`,
`ARCHIVA_SCALE_PARITY_DECISIONS_PER_FILE`, and
`ARCHIVA_SCALE_PARITY_MUTATE_FILES`. Increasing decisions per file exercises
dense `.dlog` and `.dmap` rewrites without requiring the same number of source
files.

Set `ARCHIVA_SCALE_SEEDED=1` to add a Rust-only seeded read/reanchor scale
profile. Seeded mode writes compatible synthetic `.dlog` and `.dmap` artifacts
directly, then measures the native binary on `post-tool-use`, `lint`, `status`,
`hooks session-start`, and `why`. Tune it with `ARCHIVA_SCALE_SEEDED_FILES`,
`ARCHIVA_SCALE_SEEDED_DECISIONS`,
`ARCHIVA_SCALE_SEEDED_DECISIONS_PER_FILE`,
`ARCHIVA_SCALE_SEEDED_MUTATE_FILES`, and
`ARCHIVA_SCALE_COMMAND_MAX_BUFFER_MB`.

The release workflow uses bounded defaults so publishing remains practical.
Large seeded profiles and ignored soak tests are manual release evidence when
raising confidence beyond the normal publish gate.

`audit:v2` checks that the local repository still exposes the required Rust
crate, zero-runtime-dependency package surface, exact validation script wiring,
CLI/MCP behavior surface, long-horizon evidence producers, archived external
validation evidence, and archived publish evidence. With
`--evidence-dir <dir>`, it also validates collected heavy-validation and
long-horizon JSON artifacts; this can point directly at a directory produced by
`gh run download --dir <dir>`. It is an honesty gate: it passes when local and
artifact evidence are internally consistent, and strict completion is allowed
after publish and post-publish smoke evidence is archived.

`scale:corpus` copies a bounded subset of real source files into temporary
projects, writes decisions, mutates files, runs hooks, lint, status,
session-start, and why, then validates the resulting `.dlog` and `.dmap`
artifacts. TypeScript/JavaScript corpora compare the TypeScript baseline
against Rust. Rust and C/C++ corpora run the native Rust binary only, because
the TypeScript baseline cannot parse those source languages. Tune it with
`ARCHIVA_SCALE_CORPUS_FILES`, `ARCHIVA_SCALE_CORPUS_DECISIONS`,
`ARCHIVA_SCALE_CORPUS_MUTATE_FILES`, and `ARCHIVA_SCALE_CORPUS_LANGUAGE`
(`auto`, `typescript`, `rust`, or `c/cpp`).

## Current scope

Supports today:

- TypeScript, JavaScript, Rust, and C/C++ anchor extraction
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
