# Archiva v2 Rust Architecture

Status: current implementation record, updated 2026-06-30.

Archiva v2 is a std-only Rust re-engineering of the TypeScript Archiva behavior. The TypeScript codebase remains the behavioral oracle for compatibility unless a divergence is documented as an intentional hardening improvement and covered by differential tests.

## Product Shape

- One native CLI binary named `archiva`.
- No runtime service, daemon, database, hosted API, or account.
- npm is only the distribution wrapper; after installation, `archiva` runs as a native binary.
- Decision storage remains repository-local in `.decisions/`.
- MCP is served over stdio with manually implemented JSON-RPC.

## Crate And Module Layout

The Rust implementation lives under `src/` and is organized around the production boundaries below.

- `src/main.rs`: process entrypoint and panic/error boundary.
- `src/cli.rs`: command parsing, help text, positional option compatibility, and command dispatch.
- `src/mcp.rs`: newline-delimited JSON-RPC MCP server and tool dispatch.
- `src/core/anchor.rs`: TypeScript/JavaScript, Rust, and C/C++ anchor extraction, complexity, ranges, and parser diagnostics.
- `src/core/decision.rs`: write-decision input validation, supersession, history, and decision record construction.
- `src/core/dlog.rs`: schema-1 `.dlog` parsing and writing.
- `src/core/dmap.rs`: `.dmap` parsing and writing.
- `src/core/storage.rs`: transactional dlog/dmap writes, locking, stale lock recovery, and derivative map regeneration.
- `src/core/project.rs`: project-level workflows for init, why, history, lint, status, ghost check, and post-tool-use.
- `src/core/git.rs`: native Git object reader for `HEAD:<path>` content across SHA-1 and SHA-256 repositories.
- `src/core/json.rs`: manual JSON parsing/writing for CLI, MCP, and settings.
- `src/core/yaml.rs`: manual YAML subset used by Archiva decision logs.
- `src/core/diff.rs`: line-range shifting and reanchor support.
- `src/core/fingerprint.rs`: deterministic code normalization and fingerprinting.
- `src/core/paths.rs`: portable project-relative path validation and normalization.
- `src/core/fs.rs`: filesystem primitives, atomic write helpers, locks, and traversal.
- `src/core/status.rs`: decision status types and issue aggregation.
- `src/core/settings.rs`: Claude settings merge support.
- `src/core/gitignore.rs`: compatibility gitignore matcher for scan filtering.
- `src/core/time.rs`: UTC timestamp formatting.
- `src/core/version.rs`: package version surface.

## Ownership And Data Flow

Commands enter through `cli.rs` or `mcp.rs` and delegate to `core::project` operations. Project operations resolve and validate repository-relative paths, load `.dlog` and `.dmap` files through storage helpers, extract anchors from source files, update decision records, and write `.dlog` as the source of truth. `.dmap` files are compact derivatives and may be regenerated from `.dlog`.

Write paths use a decision-base lock before mutating `.dlog` or `.dmap`. Readers repair missing or corrupt derivative `.dmap` files from valid `.dlog` data where the behavior is intentionally documented as a Rust hardening improvement.

The native Git reader is request scoped. A single `GitObjectReadContext` caches pack-index validations and sorted offsets during one `HEAD:<path>` read, then is discarded so repository changes between commands are not hidden by global state.

## Behavioral Compatibility

The Rust CLI preserves the public command set:

- `archiva init`
- `archiva why`
- `archiva history`
- `archiva lint`
- `archiva status`
- `archiva hooks session-start`
- `archiva hooks post-tool-use`
- `archiva mcp`
- `archiva write-decision`

The compatibility suite compares Rust against the TypeScript implementation for CLI output, MCP protocol behavior, decision file contents, path normalization, status side effects, lint behavior, reanchor behavior, and package installation behavior. Known intentional improvements include stricter path validation, bounded MCP input handling, corrupt derivative `.dmap` repair, scoped MCP `ghost_check`, release argument validation, and Git rename recovery.

## Serialization Strategy

Archiva uses manual std-only parsers and writers:

- JSON: manual parser/writer for CLI input, MCP envelopes, MCP schemas, and settings JSON.
- YAML: manual schema-1 `.dlog` parser/writer for the subset Archiva owns.
- `.dmap`: manual line parser using final-colon status suffix handling so anchors may contain colons.

The writer treats `.dlog` as authoritative and `.dmap` as rebuildable. Unknown schema-1 fields are not preserved on rewrite, matching the TypeScript behavior.

## Anchor Strategy

TypeScript/JavaScript extraction is compatibility-oriented and covered by differential fixtures. Rust extraction is native-only and supports functions, methods, structs, enums, traits, modules, impls, nested modules, function-local items, significant `if` blocks, and complexity estimates. C/C++ extraction is native-only and supports free functions, class/struct methods, classes, structs, enums, significant `if` blocks, and complexity estimates.

Corpus validation requires Rust and C/C++ native-only runs to report mixed anchor-kind coverage on larger corpora. Unsupported source languages are not treated as corpus candidates.

## Git Strategy

The native Git reader supports SHA-1 and SHA-256 repositories without spawning `git show`. It handles loose objects, packed objects, v1/v2 pack indexes, OFS/REF deltas, alternates, linked worktree common dirs, packed refs, chained symbolic refs, pack/index checksums, pack trailer validation, and pack header count validation.

Repository object format is detected from worktree and common gitdir config before HEAD resolution. A `GitObjectFormat` descriptor drives ref validation, tree object-id width, pack-index object-name width, REF_DELTA base-id width, object hashing, and pack/index trailer hashing.

## Testing And Validation Strategy

Local and CI validation layers:

- Rust unit and integration tests.
- TypeScript compatibility tests.
- Differential CLI/MCP/file comparison against the TypeScript implementation.
- Native package smoke for installed direct, meta, root-tarball, and published package paths.
- Stress harness for repeated writes, mutations, reanchors, supersedes, MCP ghost checks, and cleanup residue.
- Benchmark comparison for startup, write, why, post-tool-use, lint, status, and MCP ghost check.
- Synthetic scale smoke with TypeScript/Rust artifact and command-output parity.
- Seeded 100k-file / 1M-decision scale validation.
- External corpus validation.
- Long-horizon corpus matrix covering Rust compiler, Cargo, ripgrep, Tokio, Linux kernel, LLVM, TypeScript, Node, React, and Next.js.
- Cross-platform GitHub Actions builds/tests and native package smokes for Linux, macOS, Windows, glibc, musl, x64, and arm64 where runners are configured.

## Release Gates

Publishing native packages requires:

- version consistency between `package.json`, `Cargo.toml`, and release tag;
- `npm run check`;
- Rust property soak;
- package build and package smoke;
- release differential;
- stress soak;
- benchmark comparison;
- synthetic scale smoke;
- seeded scale;
- external TypeScript corpus scale;
- Rust self-corpus scale;
- mandatory long-horizon corpus matrix;
- per-target native build and deep package smoke;
- post-publish install smoke.

## Future Extension Points

The implementation keeps extension points local and std-only:

- add source-language extractors behind explicit file-extension dispatch in `src/core/anchor.rs` and `src/core/project.rs`;
- add MCP tools by extending the manual `tools/list` schema and `tools/call` dispatcher in `src/mcp.rs`;
- add package targets through `tools/native-targets.mjs`, then let metadata validation enforce package, workflow, and lockfile drift;
- add long-horizon corpora through the shared matrix contract in `tools/validate-native-package-metadata.mjs`;
- add differential or scale scenarios as standalone tools that compare observable CLI/MCP/file behavior before changing release gates.

## Remaining Architecture Gaps

- Execute and archive real GitHub-hosted macOS, Windows, ARM, musl, heavy-validation, and long-horizon workflow results.
- Keep this document and `docs/archiva-v2-review-status.md` current with final CI artifacts before declaring v2 complete.
