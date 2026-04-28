# Benchmarking Archiva

Archiva is decision memory infrastructure. It should be evaluated as an agent aid, not as a standalone code generator.

The right benchmark question is:

> Does the same agent complete more tasks, make fewer regressions, or spend less context/tool budget when it can read and write decision memory?

## Recommended Public Benchmarks

### SWE-bench / SWE-bench Verified

Use SWE-bench when you want real GitHub issue-resolution tasks with execution-based grading. SWE-bench Verified is the smaller human-validated subset commonly used for agent comparisons.

Best fit for Archiva:

- multi-file bug fixes
- issue resolution where prior design constraints matter
- regression-prone tasks where rejected approaches should not be retried

Evaluation design:

```text
same model + same agent + same task set

baseline:
  no Archiva MCP server

archiva:
  Archiva MCP server enabled
  repo pre-seeded with decision logs for relevant touched anchors
  agent instructed to call why before edits and write_decision after non-trivial choices
```

Metrics to report:

- resolved task percentage
- test pass percentage
- patch regression count
- average tool calls per task
- average tokens per resolved task
- count of decisions read and written
- count of stale/orphan decisions after each task

Important caveat: most public SWE-bench tasks start from a clean historical repo. To test Archiva fairly, create an **Archiva-seeded variant** of the task where `.decisions/` contains prior rationale that a continuing project would have.

### Terminal-Bench

Use Terminal-Bench when you want end-to-end terminal tasks scored by task-specific tests. It is useful for measuring whether an agent can operate in a real shell environment while using tools such as MCP.

Best fit for Archiva:

- tasks with multiple implementation paths
- tasks that require revisiting prior code during the same run
- tasks where a second pass should respect a decision recorded in the first pass

Evaluation design:

```text
same agent runtime + same terminal-bench tasks

baseline:
  no Archiva MCP server

archiva:
  Archiva installed in the image
  MCP config available to the agent
  AGENTS.md includes the Archiva usage instruction
```

Metrics to report:

- task success rate
- number of test reruns
- elapsed wall-clock time
- terminal commands/tool calls
- whether the final repo has valid decision records
- whether the second task pass reads existing decisions before editing

## A/B Harness Pattern

For any agent benchmark, keep everything identical except Archiva availability:

```text
1. Select benchmark tasks.
2. Pin model, agent version, temperature, timeout, and container image.
3. Run baseline with no Archiva MCP config.
4. Run treatment with Archiva MCP config and AGENTS.md instructions.
5. Compare test-verified outcomes.
6. Publish raw trajectories and patches.
```

Avoid evaluating Archiva with only a single-turn prompt. Archiva is most useful when work spans multiple edits, sessions, agents, or design alternatives.

## Suggested Result Table

```md
| Benchmark | Agent | Model | Tasks | Baseline resolved | Archiva resolved | Token/tool delta | Notes |
|---|---|---:|---:|---:|---:|---:|---|
| SWE-bench Verified subset | Codex CLI | ... | 50 | TBD | TBD | TBD | Archiva-seeded repo variant |
| Terminal-Bench subset | Claude Code | ... | 20 | TBD | TBD | TBD | MCP enabled in treatment |
```

## Included Context-Footprint Benchmark

The repository includes a small deterministic benchmark:

```sh
npm run benchmark
```

It measures storage/context footprint for a fixture repository:

```text
decisions: 3
full .dlog bytes: 1361
compact .dmap bytes: 76
session-start bytes: 559
.dmap vs .dlog byte reduction: 94.4%
session context vs .dlog byte reduction: 58.9%
average .dmap bytes per decision: 25.3
```

This is not a model-quality score. It only measures how compactly Archiva can expose decision memory.
