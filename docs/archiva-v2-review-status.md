# Archiva v2 Review Status

Status: current review record, updated 2026-06-30.

## Current Evidence

The Rust implementation is present and is the package entrypoint. Current local evidence from this workspace:

- `npm run check`: passed, including `cargo fmt --check`, clippy with `-D warnings`, 301 passing Rust tests, 1 ignored Rust test, 9 integration tests, 1 doc-style test, native package metadata validation, and the v2 completion audit.
- `npm test`: passed, 108 Vitest tests across 7 files, including `audit:v2` regression coverage for strict completion, script drift, silent workflow artifact producers, nested and ambiguous evidence artifacts, and C/C++ artifact semantics.
- `cargo test --quiet core::git`: passed, 42 tests, including SHA-256 loose, packed, and linked-worktree coverage.
- `npm run build`: passed, including release Rust build, native package staging, TypeScript build, and native bin shim generation.
- `npm run differential:release`: passed, including `post-tool-use-sha256-git` and all compatibility/improvement scenarios.
- `npm run property:soak`: passed the ignored extended serialization/diff property test.
- `npm run stress:soak`: passed, 30 cycles across 10 files / 6 functions.
- `npm run smoke:package`: passed with staged `linux-x64-gnu` native tarball, installed TypeScript and Rust CLI behavior, git-backed reanchor, clean status/lint, MCP `tools/list`, and MCP `ghost_check`.
- `npm run scale:smoke`: passed with 512-file Rust scale, measured RSS, and TypeScript-vs-Rust command-summary plus decision artifact parity.
- `npm run scale:corpus:rust` against `src/`: passed with bounded 40-file / 24-decision / 16-mutation self-corpus settings, corpus semantic checks, shifted/stale mutation evidence, and mixed Rust anchor coverage.
- Forced C/C++ `npm run scale:corpus` against a temporary C/C++ corpus: passed with native-only corpus validation, shifted/stale mutation evidence, and mixed C/C++ anchor-kind coverage.
- `npm run benchmark:compare`: passed within configured runtime and RSS thresholds; measured Rust peak RSS was 2816 KiB and all benchmark ratios were within gate.
- `npm run audit:v2`: passed 36 local evidence checks; `npm run --silent audit:v2 -- --evidence-dir <synthetic bundle>` passed the artifact-bundle validation path.
- `git diff --check`: passed.
- `cargo run --quiet -- lint`: passed.
- `cargo run --quiet -- status`: 537 decisions, 0 stale, 0 orphan, 0 issues.

## Independent Review Findings Addressed

Recent reviewer findings and dispositions:

- Pack-index absent-prefix validation could be optimized incorrectly and silently skip malformed indexes.
  - Disposition: added regression coverage for corrupt checksum, checksum-valid fanout/name mismatch, and prior-pack miss followed by later-pack hit.
- Scale parity compared decision artifacts but not user-visible command behavior.
  - Disposition: scale parity now compares normalized status/stdout/stderr command summaries for lint, status, session-start, and why, while ignoring RSS differences because only Rust is measured.
- Rust-only corpus validation had no TypeScript oracle and weak semantic checks.
  - Disposition: corpus runs now verify every decision dlog/dmap, mutation evidence, status/session-start/why output, and Rust anchor-kind coverage on larger corpora.
- Per-target native package smoke was too shallow.
  - Disposition: installed package smoke now covers TypeScript and Rust anchors, status, lint, git-backed post-tool-use, MCP tools/list, and MCP ghost_check. CI and publish matrices already invoke this helper per target.
- Long-horizon corpus coverage omitted TypeScript compiler and Tokio.
  - Disposition: validation and publish long-horizon matrices now include pinned TypeScript and Tokio entries, and metadata validation enforces the long-horizon matrix.
- Linux kernel and LLVM were not meaningful long-horizon targets while C/C++ source files were unsupported.
  - Disposition: C/C++ source discovery, anchor extraction, corpus selection, semantic checks, and metadata validation are now wired into the native path; validation and publish matrices include pinned Linux kernel and LLVM entries.
- Completion evidence was narrative rather than machine-readable.
  - Disposition: `npm run audit:v2` now checks local v2 evidence and can validate collected heavy-validation and long-horizon JSON artifacts with `--evidence-dir`.
- Completion audit behavior was not regression-tested.
  - Disposition: Vitest coverage now exercises local audit success, strict-complete failure, exact release-script drift, missing/invalid/failed artifacts, and C/C++ long-horizon semantic failures.
- Musl post-publish smoke only checked version output.
  - Disposition: the Alpine musl post-publish job now exercises init, write-decision, why, status, lint, and MCP with the installed published native binary.

## Remaining Local Gaps

- Final release completion still needs `audit:v2 --evidence-dir` against fresh CI, long-horizon, publish, and post-publish artifacts from the release commit.

## External Evidence Still Required

These cannot be proven from this Linux workspace alone:

- macOS and Windows Rust build/test results from GitHub-hosted runners;
- Linux arm64 and musl native package build/smoke results;
- full heavy-validation workflow artifacts;
- scheduled or manually triggered long-horizon corpus artifacts;
- npm publish and post-publish install smoke artifacts.

Until those artifacts exist and pass, the v2 objective remains active rather than complete.
