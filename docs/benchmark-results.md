# Archiva Benchmark Results

Date: 2026-04-29

Harness: Terminal-Bench 0.2.18 with `terminal-bench-core==0.1.1`

Agent: Claude Code 2.1.121

Model: `claude-sonnet-4-6`

Package under test: `@jalkarna/archiva@0.1.1` from npm

## Summary

This is a smoke-scale benchmark run, not a leaderboard claim. It verifies that Archiva can be installed into a real Terminal-Bench task container, exposed to Claude Code as an MCP server, called by the agent, and still pass the benchmark tests.

On the one clean A/B task, both baseline and Archiva passed. Archiva added overhead on this simple one-shot conversion task, as expected, because there was no prior decision memory to exploit.

## Terminal-Bench Results

| Run | Task | Agent setup | Resolved | Parser results | Trial time | Agent time | Claude turns | Claude cost |
|---|---|---|---:|---|---:|---:|---:|---:|
| `archiva-baseline-csv-20260429003301` | `csv-to-parquet` | Claude Code only | yes | `test_parquet_exists`, `test_data_matches` passed | 128.6s | 76.1s | 9 | $0.1197 |
| `archiva-mcp-csv-20260429004319` | `csv-to-parquet` | Claude Code + Archiva MCP | yes | `test_parquet_exists`, `test_data_matches` passed | 177.6s | 146.8s | 20 | $0.2028 |
| `archiva-baseline-fix-git-20260429003119` | `fix-git` | Claude Code only | no | `test_layout_file` passed, `test_about_file` failed | 72.2s | 48.9s | unavailable | unavailable |

## Archiva MCP Trace Evidence

The Archiva-enabled `csv-to-parquet` run showed:

- Claude Code started with `mcp_servers: [{ name: "archiva", status: "connected" }]`.
- Available MCP tools included `mcp__archiva__why`, `mcp__archiva__write_decision`, and `mcp__archiva__ghost_check`.
- The agent ran `archiva init` after `archiva init --yes` was rejected by the published CLI.
- The agent called `mcp__archiva__why` before changing task output.
- The agent called `mcp__archiva__write_decision` after producing `/app/data.parquet`.
- Archiva returned `Recorded dec_001.`
- Terminal-Bench validation passed both output tests.

Decision recorded by the benchmark agent:

```text
dec_001: Used pandas read_csv + to_parquet (via pyarrow engine) to convert data.csv to data.parquet.
Rejected: direct pyarrow CSV reader, fastparquet engine.
```

## Interpretation

What this proves:

- The npm package installs inside a benchmark task container.
- `archiva mcp` works as a stdio MCP server under Claude Code.
- Claude Code can discover and call Archiva MCP tools.
- A benchmark task can still pass after Archiva is initialized and a decision is written.

What this does not prove:

- It does not show a quality lift yet. The successful task had no prior decision memory, so Archiva could only add bookkeeping overhead.
- It does not justify broad claims like "improves accuracy" or "reduces tokens" for agent benchmarks.
- It does not measure multi-session memory benefits. That needs an Archiva-seeded benchmark variant or a two-pass task where the second run must respect decisions from the first.

## Harness Notes

The first Archiva MCP attempt used Claude Code's `--dangerously-skip-permissions` flag inside a root Terminal-Bench container. Claude Code rejected that mode for root, the harness waited until timeout, and the run failed before doing useful work. The adapter was corrected to use explicit `--allowedTools` for built-in and MCP tools.

Docker Compose v2 was required for Terminal-Bench. This machine initially had Docker and legacy `docker-compose`, but not the `docker compose` plugin. Installing `docker-compose-v2` fixed the harness.
