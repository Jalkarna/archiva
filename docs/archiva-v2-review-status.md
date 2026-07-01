# Archiva v2 Review Status

Status: current review record, updated 2026-07-02.

## Current Evidence

The Rust implementation is present and is the package entrypoint. Current evidence from this workspace and GitHub:

- Rust implementation validation commit `dabe3923a48f41ca1d09b624c9353b733c83e4ac` is pushed on PR #3.
- GitHub CI run `28549540760`: passed on Linux, macOS, and Windows Rust jobs; all seven native package build/smoke jobs; robustness gates; and the main test job.
- Heavy validation run `28549550140`: passed with uploaded JSON artifacts for differential, stress soak, benchmark comparison, synthetic scale smoke, seeded scale, external TypeScript corpus scale, and Rust self-corpus scale.
- Long-horizon corpus run `28551553784`: all ten corpus jobs passed and uploaded JSON artifacts for Rust compiler, Cargo, ripgrep, Tokio, Linux kernel, LLVM, TypeScript, Node, React, and Next.
- `npm run --silent audit:v2 -- --evidence-dir /tmp/archiva-evidence-combined-28551553784 --json`: passed 60 checks with 0 failures against the combined heavy-validation and long-horizon artifact bundle.
- `npm run check`: passed, including TypeScript compile, `cargo fmt --check`, clippy with `-D warnings`, Rust tests, native package metadata validation, and the v2 completion audit.
- `cargo test --all-targets --locked --quiet`: passed, including 318 Rust unit tests, 1 ignored Rust test, and all integration suites.
- `cargo clippy --all-targets --locked -- -D warnings`: passed.
- `cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings`: passed.
- `npm run build`: passed, including release Rust build, native package staging, TypeScript build, and native bin shim generation.
- `npm run differential:release`: passed, 56 scenarios, 0 failures.
- `npm run property:soak`: passed the ignored extended serialization/diff property test.
- `npm run stress:soak`: passed, 30 cycles across 10 files / 6 functions, 0 failures.
- `npm run scale:smoke`: passed with 512-file Rust scale and TypeScript-vs-Rust parity artifacts.
- `npm run scale:corpus` against the pinned TypeScript compiler corpus: passed with 80 selected files, 30 decision writes, and byte-matched TypeScript/Rust decision artifacts.
- `npm run scale:corpus:rust` against `src/`: passed with 31 selected Rust files, 24 decision writes, and mixed Rust anchor-kind coverage.
- `npm run benchmark:compare`: passed within configured runtime and RSS thresholds; heavy-validation measured Rust peak RSS under 3 MiB and all benchmark ratios were within gate.
- `cargo run --quiet -- lint`: passed.
- `cargo run --quiet -- status`: 594 decisions, 0 stale, 0 orphan, 0 issues.

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

## External Evidence Status

The previously missing external validation evidence is now archived for this commit:

- macOS and Windows Rust build/test results: proven by CI run `28549540760`.
- Linux arm64 and musl native package build/smoke results: proven by CI run `28549540760`.
- full heavy-validation workflow artifacts: proven by Heavy validation run `28549550140`.
- scheduled or manually triggered long-horizon corpus artifacts: proven by long-horizon jobs in run `28551553784`.

## Remaining Release Evidence

- npm publish and post-publish install smoke artifacts still require a real release/tagged publish workflow run.

Until publish and post-publish artifacts exist and pass, the release v2 objective remains active rather than complete.
