# Archiva v2 — Release-Readiness Audit

> Historical audit note: this document records the independent pre-remediation
> audit of commit `33f160e`. It is kept as the source list of release findings;
> current readiness must be judged from the branch head, local/CI validation, and
> `docs/archiva-v2-review-status.md`.

**Auditor role:** Independent Principal Software Architect / Release Auditor
**Subject:** Archiva v2 — std-only Rust re-engineering of a TypeScript "decision memory for AI coding agents" tool
**Date:** 2026-07-01
**Branch audited:** `codex/archiva-v2-rust-validation` (HEAD `33f160e`, version 0.2.0)

**Method:** 115 independent agents across 17 subsystem/dimension reviews; every substantive finding adversarially re-verified by a second agent that reproduced it against the compiled release binary or read the cited code. Documentation and the team's own `docs/archiva-v2-review-status.md` were treated as **unverified claims**, not evidence.

**Independently verified baseline (by the auditor before fan-out):**

- `cargo build --release` — clean.
- `cargo clippy --all-targets --locked -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `cargo test` — 301 lib tests pass (1 ignored) + 9 + 1 + 3 integration tests pass.
- Binary functional: `archiva --version` → `0.2.0`; `archiva status` on the repo → 537 decisions, 0 stale, 0 orphan, 0 issues across 56 `.dlog` files.

**Verification outcome across the panel:** 96 findings `CONFIRMED`, 1 `PLAUSIBLE`, **0 `REFUTED`**. Severity distribution (corrected, non-refuted): **17 high / 27 medium / 44 low / 9 info**, plus 10 audit-coverage gaps from a completeness critic.

---

## 1. Executive Summary

Archiva v2 is a genuinely impressive piece of engineering: a zero-dependency, std-only Rust implementation of a multi-language anchor extractor, a from-scratch git object reader (SHA-1 **and** SHA-256, packs, deltas, alternates), hand-written JSON/YAML parsers, a CLI, and a stdio MCP server — all clean-compiling, clippy-strict, well-formatted, and backed by 300+ tests and an elaborate differential/stress/scale/corpus validation harness. The code quality at the unit level is high and the discipline is real.

**But it is not ready for a stable 1.0 public release, and it is not yet the reference implementation.** The audit confirmed a coherent cluster of release-blocking problems that the existing test strategy structurally cannot see:

1. **A class of trivially-reachable process aborts (panics / stack overflows) triggered by ordinary committed, team-shared data.** Three distinct crashes were reproduced (a lone `'` in a `.dlog` → `yaml.rs:700`; a mid-codepoint UTF-8 slice → `yaml.rs:311`; deeply-nested source → unbounded recursion in the anchor extractor), and the completeness critic found a **fourth** (empty block scalar) in minutes. Because `.decisions/` is git-tracked by default and shared across a team, a single malformed byte in one file aborts `status`, `lint`, the per-session `session-start` hook, and — most seriously — **kills the long-lived MCP server mid-session**, dropping all in-flight agent context. This is a class, not a list.

2. **The product's headline automatic workflow is broken end-to-end.** The auto-wired `PostToolUse` re-anchor hook is a confirmed no-op under real Claude Code: `init` wires it with no argument relying on `ARCHIVA_FILE`, but Claude Code delivers the edited path as JSON on stdin, which `post-tool-use` never reads. It errors on every edit and silently never re-anchors. Compounding this, even when invoked correctly, re-anchoring is **non-idempotent** and **falsely marks correct decisions STALE** whenever there is no committed HEAD baseline (new files, or multiple edits between commits) — and the corruption compounds and does not self-heal.

3. **Performance cliffs contradict the "scales to large repos" claim, and the scale harness is blind to both of them.** Anchor extraction is O(n²) per file (a 1.4 MB file ≈ 55–66 s for one file); the hot-file write path is O(n²) cumulative (1,200 decisions in one file ≈ 86 s). The scale-smoke harness uses tiny one-function files and skips any file > 256 KiB, so neither bottleneck is ever exercised.

4. **Operational diagnosability is essentially zero** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery (dmap repair, stale-lock takeover) — for a tool that runs unattended as an agent hook.

None of the high-severity findings are memory-unsafety or RCE — Rust aborts cleanly — so the worst case is availability/DoS and silent metadata corruption, not exploitation. The defects are concentrated, well-understood, and individually fixable without architectural change. With a focused 4–8 week remediation pass (panic-safety hardening, hook stdin contract, idempotent re-anchoring, the O(n²) fixes, and a logging channel), this can become an excellent 1.0.

**Verdict: Do not ship as-is. Strong foundation; specific, fixable blockers.**

---

## 2. System Overview

Archiva stores *why code exists* beside the code. Per source file it maintains `.decisions/<path>.dlog` (authoritative YAML, schema:1) and `.decisions/<path>.dmap` (compact derivative index). Decisions are anchored to AST identities (`fn:foo`, `struct:Bar`, `block:if_x`) rather than line numbers, carry a fingerprint for drift detection, and form supersession chains. The same core operations are reachable three ways — CLI (`init`/`why`/`history`/`lint`/`status`/`hooks`/`write-decision`/`mcp`), a stdio JSON-RPC MCP server (`why`/`write_decision`/`ghost_check`), and Claude Code hooks (`session-start`/`post-tool-use`). Distribution is via an npm wrapper that selects a platform-specific native binary; the runtime is a single native binary with **no dependencies** (`Cargo.toml` `[dependencies]` is empty).

The architecture is sound and the product thesis is coherent. The problems are in robustness, the hook integration contract, performance at scale, and observability — not in the concept or the module decomposition.

**Module sizes (Rust, src/):** `anchor.rs` 12,341 (incl. ~5,090 test lines) · `git.rs` 4,329 · `project.rs` 2,261 · `fs.rs` 1,487 · `yaml.rs` 1,465 · `mcp.rs` 1,174 · `cli.rs` 1,030 · `decision.rs` 963 · `storage.rs` 959 · `json.rs` 722 · `diff.rs` 657 · `property_tests.rs` 540 · `dlog.rs` 507 · `paths.rs` 487. Total ~31,464 lines.

---

## 3. Architectural Assessment — **8/10**

Module boundaries are clean and cohesive: `cli`/`mcp` entrypoints → `core::project` orchestration → typed core modules (`decision`, `storage`, `dlog`/`dmap`, `anchor`, `git`, `paths`, `fs`). Coupling is low and the data-flow ownership (`.dlog` authoritative, `.dmap` rebuildable, request-scoped git reader) is well-reasoned.

The central architectural tension is the **zero-dependency, reimplement-everything-by-hand** stance: ~2.6k lines of git plumbing including a from-scratch DEFLATE inflater (`git.rs`), hand-written JSON/YAML parsers, and a multi-thousand-line multi-language anchor tokenizer. This is defensible *as a product tradeoff* (trivial supply chain, tiny binary, no transitive CVEs) but it concentrates the entire bug surface in hand-rolled parsers that have **not been fuzzed** — and that is precisely where every confirmed panic lives. The std-only purity is the root cause of the dominant risk class.

Two concrete architectural weaknesses (both verified):

- **No schema migration story.** `DLOG_SCHEMA_VERSION` is a hardcoded `1` and the parser hard-rejects anything else; a single forward-version file aborts every whole-repo command. There is no migrate-on-read and no skip-with-warning.
- **The ground-truth anchor range is computed and then discarded** in the normal re-anchor path (`project.rs:290-304`) — the parser already knows the anchor's exact current position, but the code trusts a fragile HEAD-diff shift instead. This is the root cause of the idempotency/STALE corruption.

Top simplification opportunity: prefer the extractor's live anchor position over diff-shifting; this single change fixes two high-severity findings at once.

---

## 4. Workflow Assessment — **5/10**

The intended loop is *read map → ask why → edit → write decision → lint drift*. Traced end-to-end:

- **`init` → first decision → `why`**: works cleanly; idempotent on re-run.
- **`write-decision`**: works, but is **non-atomic across `.dlog`/`.dmap`** (a torn write durably commits the decision while reporting failure, exit 1) and the natural retry is **non-idempotent** (overwrites the just-committed record with a new id, losing the original reasoning with no history entry).
- **Auto re-anchor on edit (the core promise)**: **broken under real Claude Code** (hook ignores the stdin payload) and **corrupts line attribution** even when invoked correctly (false STALE + compounding line drift with no committed baseline).
- **Re-deciding an anchor**: silently destroys the prior decision and its entire history unless `supersedes:<id>` is passed; superseding *into* an anchor occupied by a different live decision silently deletes that unrelated decision.

The "happy path" demos work; the realistic agent workflow (uncommitted files, multiple edits per commit, re-decisions) has multiple silent-data-integrity failures.

---

## 5. Feature Completeness Assessment — **7/10**

The advertised command set is fully present and the CLI surface is consistent and complete. Gaps are in fidelity rather than coverage: MCP `why` cannot do line-based lookup (the `line` field is silently dropped and it returns a *confidently wrong* whole-file result rather than "not found"); no `--fix` audit trail; no migration tooling. Feature breadth is appropriate and intentionally narrow (the README's positioning vs. broad memory tools is honest and well-argued).

---

## 6. Behavioral Consistency Assessment — **6/10**

CLI ↔ MCP ↔ hooks largely agree on validation and path normalization (verified: `.//src/a.ts` and `src\a.ts` normalize to one identity across entrypoints). Confirmed divergences:

- **MCP `why` ignores `line`** and returns the wrong decision; CLI `why <file> <line>` is correct.
- **MCP tool errors are returned as JSON-RPC protocol errors (`-32000`)** instead of the MCP convention `result.isError:true` — a spec inconsistency for tool *execution* failures.
- **`lint` exit code conflates** "found issues" with "command failed"; `status` returns 0 even with outstanding issues. No exit-code taxonomy.

---

## 7. Data Model Assessment — **7/10**

The decision record (chose/because/rejected/anchor/fingerprint/lines_hint/history/supersession) is well-designed and matches the spec and the YAML schema. Two material risks: **unknown/forward-compat fields are silently dropped on every rewrite** (data loss for any external or future-version annotation), and the **no-supersede overwrite** path discards history. The model is good; its *evolution and preservation guarantees* are weak.

---

## 8. Storage & Persistence Assessment — **7/10**

Individual writes are atomic (temp + rename + fsync; Unix parent-dir fsync). Verified strengths: corrupt `.dmap` self-heals from `.dlog`; stale-lock recovery works. Verified weaknesses:

- **No cross-file atomicity** between `.dlog` and `.dmap` (torn-write false-failure).
- **PID-liveness lock veto can wedge all writes indefinitely** (PID reuse defeats the staleness check; no force-unlock).
- **Read-only `status`/`lint` acquire the per-file write lock** and fail on a read-only `.decisions/`.
- Parent-dir durability fsync is a **no-op on Windows** (crash-consistency claim is weaker there and untested in crash-injection form).

---

## 9. CLI Assessment — **8/10**

The strongest subsystem. Dispatch, help text, exit-code routing (0/1), `--` escaping, and unknown-flag/command handling are correct and extensively tested. Rough edges: `write-decision` **reads stdin before validating its own args** (a malformed call with an open stdin hangs), the `lint` exit-code conflation, inconsistent `error:`-prefixing between argument vs. semantic errors, and `why` accepting line `0`.

---

## 10. Protocol & Integration Assessment — **6/10**

JSON-RPC 2.0 framing, id handling, `notifications/*` swallowing, and method dispatch are correct and tested. **The integration contract with Claude Code is broken** (PostToolUse stdin payload ignored — the single most important integration defect). MCP tool-error encoding deviates from the MCP convention. And the long-lived server has **no panic isolation** — one crafted `tools/call` aborts the whole process.

---

## 11. Performance Assessment — **6/10**

Measured, not assumed:

- **Anchor extraction O(n²) per file** (`is_top_level`/`rust_depths_before` re-scan the token prefix per declaration): TS 10k/20k/40k lines = 0.5/2.3/9.9 s; a 1.4 MB file ≈ 55–66 s; a single huge Rust fn ≈ 47 s vs ~3 s for the TS equivalent. Memory stays modest (~70 MB) — purely CPU.
- **Hot-file writes O(n²) cumulative** (full render + redundant re-parse + full rewrite + fsync per write): #1,200 in one file ≈ 86 s cumulative; 1,500 timed out at 120 s. The `storage.rs:133` self re-parse of freshly-rendered YAML is unconditional wasted work.
- **`status` parses every `.dlog` twice** per invocation and sweeps the whole source tree.
- The `.dmap` index **is never read by any command** — every read re-parses the verbose `.dlog`, so the derivative index pays maintenance cost for zero read benefit.

Startup is excellent (~0.7 ms). For small/medium repos performance is fine; the cliffs are real and reachable.

---

## 12. Scalability Assessment — **5/10**

The "scales to 100k files / 1M decisions / Linux-kernel-and-LLVM corpora" claim is **not substantiated by the harness**, which uses tiny one-function files, skips files > 256 KiB, and defaults to 1 decision/file — exactly avoiding both quadratic paths. The blast radius is also wider than the per-command framing: because whole-repo commands re-extract anchors from *every* source file (decided or not), one large generated/minified/vendored file degrades `lint`/`status` repo-wide. Linear-in-file-count scanning is otherwise reasonable.

---

## 13. Reliability Assessment — **5/10**

Dominated by the panic class: `status`, `lint`, `session-start`, and the MCP server all abort (SIGABRT / exit 101 or 134) on malformed-but-committed input, and **a single corrupt `.dlog` aborts the entire repo-wide command** rather than being skipped and reported — taking down visibility for all healthy files. Recovery paths that exist (dmap repair, lock recovery) are correct but silent. Single-file commands are correctly scoped and limit some blast radius.

---

## 14. Failure Recovery Assessment — **6/10**

Good: atomic single-file writes, self-healing dmap, stale-lock recovery. Weak: no cross-file transaction; `lint --fix` is non-atomic across files and leaves a partially-fixed repo with no record of what changed on mid-run failure; no panic boundary so corruption aborts rather than degrades; no migration/forward-compat recovery. The recovery primitives are sound but not composed into command-level resilience.

---

## 15. Security Assessment — **6/10**

Threat model is correct (local CLI; parses untrusted repo/agent-supplied `.dlog`/source; no authn expected). No RCE, no memory unsafety, no command execution, no network. Confirmed issues:

- **DoS via panics** on committed/shared input (the dominant issue) — including killing the shared MCP server.
- **Write-side symlink escape**: the read path canonicalizes and rejects escapes, but the *write* path does not — a checked-in symlink under `.decisions/` writes `.dlog`/`.dmap` outside the repo and can clobber a same-named `.dmap` in the target. A clean bypass of the project's own advertised symlink control (asymmetric: `.dlog` clobber is gated by the parse step, `.dmap` is not).

Read-path path-validation hardening is genuinely strong (traversal, drive/UNC/device, reserved Windows names, trailing dot/space all rejected; verified). The git zlib path bounds delta depth (though the cap of 32 is *below* git's default 50 — a correctness bug, not a security one).

---

## 16. Testing Assessment — **6/10**

Volume is high and the differential-against-TS strategy is the right idea. But the confidence is narrower than the count implies:

- **The differential oracle covers only TS/JS.** The Rust and C/C++ extractors — the largest *novel* code the v2 effort added — have **no independent oracle**; correctness rests on same-team unit tests. A wrong range in a `.rs` file is silently accepted (verified: a decision with a deliberately wrong line range passes `lint`).
- **No fuzzing of the hand-written parsers** — the exact gap that let four panics survive into a release candidate.
- **No concurrency/crash-injection tests** — atomicity and locking verdicts rest on code reading.
- **The scale harness exercises a best case** (tiny files, 256 KiB skip, 1 decision/file).
- **The strongest suites (property soak, full scale, corpus matrix) run only weekly/on-dispatch, not on PRs.**
- **Cross-platform (Windows/macOS/arm/musl) is CI-only and unverified here**; the Windows crash-consistency path is a different (weaker) code path that is not crash-tested.

---

## 17. Documentation Assessment — **8/10**

README, spec, and architecture docs are clear, honest, and unusually well-written (the competitive-positioning table is fair; `docs/archiva-v2-review-status.md` candidly lists remaining gaps). Two doc-vs-reality drifts matter: the quick-start's auto-wired hook doesn't work as documented, and the "scales to large repos" claim is not borne out. Otherwise documentation is a strength.

---

## 18. Developer Experience Assessment — **7/10**

Onboarding is smooth and the CLI is pleasant and consistent. DX is undercut by: the broken auto-hook surfacing a recurring error on every edit, the absence of any diagnostic/verbose mode, confusing error messages with no file path on corrupt-store scans (`file: missing required field` reads as a sentence, not a schema field), and the silent data-loss footguns (re-decide without supersede). The bones are good; the failure-mode UX needs work.

---

## 19. Operational Readiness Assessment — **4/10**

The single biggest operational gap is **observability: there is none** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery. For an unattended agent hook, any non-crash misbehavior currently requires recompiling with instrumentation to investigate. Combined with the panic class and the no-path corrupt-file errors, field diagnosability is poor.

---

## 20. Production Readiness Assessment — **5/10**

Not production-ready for general/public use today. It is usable in a controlled, single-user, small-repo, all-committed-files setting where the panic triggers and perf cliffs are unlikely. It is not ready for team use over a shared `.decisions/` tree (the panic propagation vector) or for large repos (the perf cliffs) or for the documented Claude Code auto-workflow (the hook contract).

---

## 21. Open Source Readiness Assessment — **7/10**

Strong: MIT license, clean repo, clippy-strict, formatted, CONTRIBUTING-grade docs, a real CI/validation/publish pipeline, and a thoughtful architecture doc with explicit extension points. Adoption-readiness is gated almost entirely by the production-readiness blockers above plus the missing panic-safety and fuzz gates that an open-source contributor base would expect before trusting it with their decision history. Packaging has one sharp edge: `import.meta.dirname` in the install tooling hard-requires Node ≥ 20.11 while npm doesn't enforce `engines`, so older-Node users get a cryptic postinstall crash.

---

## 22. Prioritized Findings

**Severity distribution (adversarially verified):** 0 critical *as labeled* / 17 high / 27 medium / 44 low / 9 info. 96 CONFIRMED, 1 PLAUSIBLE, **0 REFUTED**. The completeness critic argues — and I concur — that the *consolidated* "panic on committed/shared input" theme meets a **critical / release-blocking** bar even though no single line-item was labeled critical.

### Release blockers (must fix before 1.0)

| # | Finding | Location | Why it blocks |
|---|---|---|---|
| B1 | **Panic/abort class on committed input** (lone `'` → `yaml.rs:700`; mid-codepoint slice → `yaml.rs:310`; empty block scalar; deep-nesting recursion → `anchor.rs:733`) | `yaml.rs`, `anchor.rs` | One malformed byte in a shared `.dlog`/source aborts `status`/`lint`/`session-start` and **kills the MCP server**. It's a class — must be closed by fuzzing + depth bounds + a panic boundary, not point patches. |
| B2 | **PostToolUse hook is a no-op under Claude Code** (ignores stdin `file_path`) | `settings.rs:5`, `main.rs:46-51`, `cli.rs:234-258` | The core advertised automation never runs in the documented environment. |
| B3 | **Re-anchor falsely marks STALE + non-idempotent line drift** (no HEAD baseline / multiple edits per commit; ground-truth range discarded) | `project.rs:271-304`, `diff.rs:17-44` | Silent corruption of the authoritative store under normal agent activity; breaks line-based `why`. |
| B4 | **O(n²) anchor extraction** (per-file prefix re-scan) | `anchor.rs:6678`, `1729/1760` | Tens of seconds–minutes on a single large file; hooks read as hangs. |
| B5 | **One corrupt `.dlog` aborts the whole repo command** + **errors carry no file path** | `project.rs:76-82/402-408/133-149`, `error.rs:88-110` | Localized corruption blinds the whole repo; operators can't locate the bad file. |

### High (fix before or immediately after 1.0)

B6 non-atomic `.dlog`/`.dmap` write (false-failure + retry id-churn, `storage.rs:256-258`) · B7 write-side symlink escape (`paths.rs`/`storage.rs`) · B8 O(n²) hot-file writes + redundant re-parse (`storage.rs:131-133`) · B9 no logging/diagnostics anywhere · B10 Rust/C/C++ extractors have no differential oracle · B11 silent data loss on re-decide without `supersedes` · B12 MCP `why` returns confidently-wrong result for a `line` query.

### Medium (notable)

PID-liveness lock wedge · read-only `status`/`lint` take write locks · MCP tool errors as `-32000` not `isError` · `lint`/`status` exit-code taxonomy · unknown-field drop on rewrite · no schema migration · anchored-gitignore patterns never match descendants · C++ `enum class` phantom anchor · git delta-depth cap 32 < 50 · `.dmap` never read · scale harness blind to both quadratics · `lint --fix` non-atomic · Node-version postinstall crash · case-insensitive-FS identity collisions · heavy validation runs only weekly.

---

## 23. Recommended Roadmap

**Milestone 1 — Panic-safety & robustness (release-blocking, ~2 wks)**

1. Fuzz the YAML/JSON parsers (`cargo-fuzz` or in-repo property soak over arbitrary bytes); fix every panic; add a depth/recursion bound to the anchor extractor and block-scalar paths.
2. Wrap MCP per-request handling in `catch_unwind` → return `isError`; never let one request abort the server.
3. Make whole-repo commands skip-and-report corrupt files (continue, name the file) instead of aborting; attach the file path to all parse/schema/IO errors.
4. **Make "no panic on any `.dlog`/source input" and "MCP server survives a malformed request" hard PR-CI gates.**

**Milestone 2 — Core workflow correctness (release-blocking, ~1–2 wks)**

5. Parse the Claude Code hook stdin JSON (`tool_input.file_path`) in `post-tool-use`; add an integration test feeding the real payload shape.
6. Re-anchor from the extractor's ground-truth position whenever the anchor still resolves; reserve diff-shift for the orphan/incomplete case. Add a regression test asserting two consecutive `post-tool-use` runs leave `lines_hint`/STALE unchanged.
7. Detect re-decide-without-supersede and supersede-into-occupied-anchor; refuse or auto-chain into history.
8. Make `.dlog`+`.dmap` a recoverable transaction (treat a `.dmap` write failure as success-with-warning, since reads self-heal).

**Milestone 3 — Scale & observability (~1–2 wks)**

9. Eliminate the O(n²) extraction (single-pass depth tracking) and the O(n²) write path (drop the `storage.rs:133` re-parse in release; consider per-file decision bounds); add a large-single-file and a dense-single-file perf regression to the PR gate.
10. Add an env-gated stderr diagnostic channel (files scanned/skipped, lock acquire/recover, dmap repair, git-HEAD fallback).
11. Either make `status`/`session-start` actually read `.dmap`, or drop it.

**Milestone 4 — 1.0 hardening (~1–2 wks)**

12. Independent oracle for Rust/C/C++ extraction (cross-check ranges vs tree-sitter or rustc/clang spans) over a real corpus.
13. Define a schema versioning/migration policy; preserve unknown fields on rewrite.
14. Canonicalize the write path (close the symlink escape); add concurrency + crash-injection tests; promote heavy validation into the release gate; fix the lock-wedge backstop, exit-code taxonomy, gitignore anchoring, git delta-depth cap, and Node-version preflight.
15. Run the multi-process lock and differential suites on Windows/macOS CI (not just `cargo test`).

---

## Subsystem Scorecard

| Subsystem / Dimension | Score |
|---|---|
| CLI surface & dispatch | 8/10 |
| MCP stdio JSON-RPC server | 7/10 |
| Anchor extraction engine | 7/10 |
| Native Git object reader | 8/10 |
| Decision logic (validation/supersession/history) | 7/10 |
| Storage / locking / atomicity / recovery | 7/10 |
| Project workflow orchestration | 8/10 |
| Serialization (JSON/YAML/dlog/dmap) | 6/10 |
| Path validation & portability | 7/10 |
| Diff / reanchor / line-shifting | 7/10 |
| Security (cross-cutting) | 6/10 |
| Performance & scalability | 6/10 |
| Reliability / recovery / observability | 7/10 |
| Testing strategy & confidence | 7/10 |
| Release engineering & packaging | 8/10 |
| API/CLI/behavioral consistency & DX | 8/10 |
| Overall architecture & maintainability | 8/10 |

*(Per-subsystem scores reflect each module's intrinsic quality. The system-level scores below are lower because the dominant failures are cross-cutting — they emerge from interactions, e.g. parser-panic × shared-committed-store × long-lived-server.)*

### Overall scores

- **Engineering quality (unit level): 8/10** — clean, disciplined, well-tested-in-volume, idiomatic, zero-warning.
- **Production readiness: 5/10** — concentrated, reproducible blockers; safe only in a controlled single-user setting today.
- **Validation confidence: 6/10** — broad and partly differential, but blind to the exact failure classes that bite (no fuzz, no scale realism, no Rust/C oracle, no concurrency/crash injection, no real-hook integration); heavy suites are off the PR path.

---

## Final Questions — Answered Explicitly

**Does the implementation fully achieve its stated goals?**
No. It achieves the *static* goal (a fast, repo-native, zero-dependency decision store with a working CLI and MCP surface) but not the *dynamic* goal: the automatic agent workflow it is built around (auto re-anchor on edit) is broken end-to-end under real Claude Code and corrupts line attribution even when invoked correctly, and the "scales to large repos" claim is not substantiated.

**Is the architecture appropriate for long-term evolution?**
Mostly yes. Module boundaries, ownership, and extension points are sound. The two evolution gaps — no schema migration path and the zero-dependency stance concentrating un-fuzzed parser risk — are addressable without restructuring.

**Is the implementation internally consistent?**
Largely, with confirmed exceptions: CLI vs MCP `why` line semantics, MCP error encoding vs spec, and exit-code conventions. These are localized.

**Would you approve this for a stable public release?**
No. The panic class on committed/shared input, the broken auto-hook, the re-anchor corruption, and the absence of observability are release-blocking for a stable 1.0.

**Would you approve it as the reference implementation?**
Not yet. A reference implementation must be panic-safe against its own committed data format and must have the core workflow proven end-to-end. Once Milestones 1–2 land and panic-safety + real-hook integration are CI gates, it is a credible reference candidate.

**Highest-priority improvements before release?**
(1) Fuzz and bound the parsers + add a panic boundary + skip-and-report corrupt files; (2) fix the PostToolUse stdin contract; (3) make re-anchoring idempotent from ground-truth positions; (4) fix the O(n²) extraction/write paths; (5) add a diagnostic logging channel. In that order.

**What work remains before 1.0?**
Milestones 1–4 above. Realistically 4–8 focused weeks. The critical path is panic-safety + workflow correctness (M1–M2).

**What risks remain after release (assuming blockers fixed)?**
Cross-platform behavior (Windows/macOS/arm/musl) remains CI-only and the Windows crash-consistency path is weaker; the Rust/C/C++ extractors stay oracle-light until an independent ground truth exists; the hand-rolled git/DEFLATE code is a maintenance hotspot that needs an ongoing differential-against-`git` fuzz harness; and concurrency/crash-consistency guarantees rest on reasoning until fault-injection tests exist.

**How does engineering quality compare with mature, well-regarded OSS in the same domain?**
At the *craft* level (code cleanliness, lint discipline, documentation, the differential-testing instinct) it compares favorably with well-run early-stage OSS and exceeds many. At the *production-hardening* level it is behind mature tools like ripgrep, gitoxide/`gix`, or tree-sitter, which earned trust through extensive fuzzing, fault injection, and battle-tested robustness against adversarial input — exactly the layer Archiva has not yet built. It has the bones of a top-tier project and an unusually honest self-assessment; it has not yet done the hardening that separates a strong prototype from a definitive reference.

---
---

# APPENDIX — Full Verified Findings

Every finding below survived independent adversarial verification (reproduction against the release binary or direct code read). `verdict` is the second agent's call; `severity` is the corrected severity. Findings are grouped by subsystem/dimension, then sorted high → info.

## CLI surface and dispatch  — score 8/10

> The CLI is a hand-rolled, commander.js-compatible dispatcher: main.rs reads stdin conditionally, special-cases `archiva mcp` (exactly one arg) to the stdio server, and otherwise calls run_cli, which special-cases `lint` (to thread a non-zero exit code) before falling through to run_cli_result. Argument parsing is per-command and consistent in its broad shape: unknown options yield `error: unknown option '<x>'`, unknown/extra positionals yield `error: unexpected argument`/`error: unknown command`, and every subcommand supports `-h`/`--help` plus a hand-written help string. I exercised every command with good args, missing args, extra args, unknown flags, `--help`, and `--` escaping, and confirmed exit codes (0 success/help, 1 error) and stdout/stderr routing match the source. The dispatch is largely correct and the help text matches the documented usage strings. The notable defects are operational rather than crashes: `write-decision` reads (and can block on) stdin before validating its own arguments; the `lint` exit code conflates "lint found problems" with "lint crashed"; and there are user-facing inconsistencies in error-message prefixing and in `why`'s line-number validation. Overall a solid, well-tested surface with a few rough edges worth fixing before release.

*Score rationale:* The CLI is clean, well-structured, and extensively tested (the in-file test module exercises help text, error messages, --, stdin bounding, and round-trip workflows). Dispatch is correct, exit codes and stdout/stderr routing are sound, and help text matches the documented usage. Points off for the stdin-before-validation hang in write-decision (the one finding with real operational bite), the lint exit-code conflation, and a cluster of low-severity consistency issues (error prefixing, line-0 acceptance, help-help rejection, first-arg-only validation). None are crashes or security issues; all are fixable without architectural change.

**Verified behaviors (checked, not assumed):**

- `--version`/`-V` print '0.2.0' exit 0; `--help`/`-h` and no-args print main help exit 0 (matches APPLICATION_VERSION = CARGO_PKG_VERSION in src/core/version.rs:5)
- Unknown command -> `error: unknown command '<x>'` exit 1; unknown top-level option (`--bogus`, `-x`) -> `error: unknown option '<x>'` exit 1
- `archiva mcp` (exactly 1 arg) is intercepted by main.rs and starts the stdio server (exit 0 with closed stdin); `mcp foo` -> unexpected argument, `mcp --bad` -> unknown option, `mcp --help` -> help
- write-decision reads JSON from stdin when --json absent (Recorded dec_001); --json and --json= both bypass stdin; missing --json value -> `error: option '--json <json>' argument missing`; empty stdin -> `Unexpected end of input` exit 1
- CONFIRMED DEFECT: `write-decision --bad` / `write-decision foo` block on a non-closing stdin (timeout 124) because should_read_stdin gates only on --json/--json=/-h/--help and the stdin read in main.rs precedes argument validation
- why: missing file -> `error: missing required argument 'file'`; >2 args -> unexpected argument; all-digit arg treated as line (0 accepted -> 'No decision found at line 0', u32 overflow -> 'line must be a positive integer'); non-digit treated as anchor
- history requires file AND anchor (distinct missing-argument errors), rejects 3rd arg
- hooks dispatch: no sub -> hooks help; session-start rejects extra args and supports --help; post-tool-use reads positional or ARCHIVA_FILE env (verified ARCHIVA_FILE=src/a.ts works; absent both -> 'Missing file path. Pass one or set ARCHIVA_FILE.')
- CONFIRMED: lint returns exit 1 both for error-severity findings (output on stdout) and for command failure like corrupt dlog (output on stderr) — exit code cannot distinguish the two
- help subcommand: `help <cmd>` works for all 8 real commands (exit 0) but `help help`, `help --help`, `help -h` -> `unknown command` exit 1; `help why extra` -> unexpected argument
- `--` escaping works: `why src/a.ts -- --bad` treats --bad as positional anchor (exit 0); status/mcp validate only first arg so `status --help extra` silently ignores 'extra' (exit 0) while `status extra` errors
- Error propagation: all errors go to stderr with trailing newline and exit 1 (CliResult::err); success output to stdout; arg-structure errors use 'error:' prefix while semantic/parser errors are unprefixed (inconsistent)

### F1. [MEDIUM] write-decision blocks on / consumes stdin before validating its own arguments

`defect` · location `src/main.rs:46-51 (should_read_stdin) and src/cli.rs:283-321 (run_write_decision)` · reporter-confidence high · verification **CONFIRMED**

**Description:** main.rs decides whether to read stdin purely from should_read_stdin(): true whenever argv[0]=="write-decision" and none of --json/--json=/-h/--help appear. So `archiva write-decision --bad`, `archiva write-decision foo`, or any invalid invocation that lacks --json first does a blocking read of stdin to completion, and only afterwards does run_write_decision reject the bad flag/positional. If stdin is an open pipe/terminal with no EOF, the process hangs indefinitely before ever reporting the argument error.

**Why it matters:** An agent or script that calls write-decision with a typo'd flag and an attached-but-idle stdin (common when a parent process leaves stdin open) will hang instead of failing fast with a clear error. It also means an unbounded stdin read happens for invocations that will be rejected anyway, wasting work.

**Impact:** Hang / delayed and confusing failure for malformed write-decision calls; potential to stall an automation pipeline.

**Likelihood:** Medium — requires a malformed write-decision call with a non-closing stdin; plausible in agent/hook contexts where stdin is inherited.

**Evidence (reporter):** Ran: `( sleep 5 > fifo & ); timeout 3 archiva write-decision --bad < fifo` -> exit=124 (timed out, i.e. it was reading stdin). Contrast: `archiva write-decision --bad </dev/null` -> immediate `error: unknown option '--bad'` exit=1. Also `timeout 3 bash -c 'sleep 100 | archiva write-decision foo'` -> exit=124. With --json present (`... --json '{}'`) it does not block. Source: should_read_stdin gates only on --json/--json=/-h/--help (main.rs:48-50), so unknown flags/extra positionals slip through and stdin is read at main.rs:7-17 before run_write_decision validates at cli.rs:293-321.

**Independent verification:** Code matches claim exactly. src/main.rs:46-51 should_read_stdin() returns true whenever argv[0]=="write-decision" and none of --json/--json=/-h/--help appear; main.rs:7-17 then performs a blocking read_stdin_to_string() BEFORE run_cli/run_write_decision ever validates args. Argument validation lives downstream at src/cli.rs:293-321 (unknown-flag and unexpected-positional rejection), reached only after the stdin read completes.

Reproduced with /home/ubuntu/archaeo/target/release/archiva:
- `write-decision --bad </dev/null` -> immediate `error: unknown option '--bad'`, exit=1 (EOF lets the read return instantly).
- Case A: `write-decision --bad` with a never-closing stdin (fifo fed by `sleep 100`), timeout 3 -> exit=124, ZERO output -> process blocked in the stdin read, never reached validation.
- Case C: same as A via a second fifo -> exit=124 (reproducible).
- Case B (contrast): `write-decision --bad --json '{}'` with the same never-closing fifo stdin, timeout 3 -> immediate `error: unknown option '--bad'`, exit=1 -> with --json present should_read_stdin() short-circuits, stdin is NOT read, and the bad flag is reported at once.
The only differentiator between blocking (A/C) and non-blocking (B) is the presence of `--json`, exactly as the gate at main.rs:48-50 dictates. Also confirmed `write-decision foo` (extra positional) blocks identically (earlier run, exit=124).

**Verifier notes / severity correction:** Claim is accurate in mechanism, location, and reproduction; medium severity is appropriate and left unchanged. Scope nuance worth recording: the hang only triggers when write-decision is invoked WITHOUT --json AND with malformed args AND stdin is an open pipe/tty with no EOF. The common automation path supplies --json (unaffected), and a closed/EOF stdin (e.g. </dev/null) fails fast. So real-world likelihood is moderate, not pervasive — it bites interactive typos and pipelines whose upstream stays open. Note this is not unique to bad args: ANY no-json write-decision (even a valid one intending to read a heredoc/pipe) reads stdin first, but for malformed calls the cost is a confusing hang instead of a fast clear error. Recommended fix: parse and validate write-decision arguments first, and only read stdin lazily when --json is absent AND args are otherwise valid (i.e. when stdin is genuinely the intended JSON source). Equivalent: move the read inside run_write_decision after the arg loop succeeds and json is None, rather than pre-reading in main().

**Recommended resolution:** Either parse write-decision arguments before reading stdin (move flag validation ahead of the stdin read, reading stdin lazily only when --json is absent AND args are otherwise valid), or have should_read_stdin treat any unrecognized argument as a reason to skip the read so validation runs first. Lazy stdin (read inside run_write_decision only when json is None and no arg errors) is the cleanest fix.

---

### F2. [MEDIUM] lint exit code conflates 'lint found error issues' with 'lint command failed'

`defect` · location `src/cli.rs:351-382 (run_lint_cli / run_lint) and src/cli.rs:31-37 (CliResult::err)` · reporter-confidence high · verification **CONFIRMED**

**Description:** `lint` is special-cased to return a non-zero status when has_error_issue() is true (status 1, results on stdout). But when the lint run itself fails (e.g. a corrupt .dlog), run_lint returns Err and run_lint_cli funnels it through CliResult::err, which also produces status 1 (message on stderr). Both distinct outcomes — 'lint ran and found problems' vs 'lint could not run' — surface as exit code 1.

**Why it matters:** CI and agent automation routinely branch on lint's exit code. With both failure modes mapped to 1, a script cannot distinguish 'decisions are stale, surface to user' from 'the tool crashed / data is corrupt, halt and investigate'. The only differentiator is stdout-vs-stderr and message parsing, which is brittle.

**Impact:** Operational ambiguity: corruption/crashes get silently treated as ordinary lint failures (or vice versa) by callers keying on exit status.

**Likelihood:** Medium — corrupt or schema-mismatched dlogs are exactly the scenario lint is meant to catch, and CI keying on exit codes is the intended usage.

**Evidence (reporter):** Case A (stale fingerprint, a real lint finding): `archiva lint` -> exit=1, stdout=161 bytes, stderr=0. Case B (corrupt dlog `schema: nope`): `archiva lint` -> exit=1, stdout=0 bytes, stderr=29 bytes. Same exit code, different stream. Code: run_lint computes `status = if has_error_issue(...) {1} else {0}` (cli.rs:380) for findings; the Err arm hits CliResult::err which hardcodes status 1 (cli.rs:32-37).

**Independent verification:** Code reading and live binary runs both confirm the claim.

CODE (src/cli.rs):
- Lines 31-37: CliResult::err() hardcodes `status: 1`, message routed to stderr.
- Lines 40-43: `lint` is special-cased at dispatch top, routed to run_lint_cli before the generic run_cli_result path.
- Lines 351-356: run_lint_cli maps Ok((status, output)) -> CliResult::with_status(status, output) but Err(error) -> CliResult::err(...) (the hardcoded-status-1, stderr arm).
- Lines 379-381: run_lint computes `let status = if has_error_issue(&issues) { 1 } else { 0 }` and returns it on the Ok path with findings on stdout.
- src/main.rs:43 process::exit(result.status) maps the CliResult status straight to the process exit code. No remapping.
- src/core/lint.rs:96-99 has_error_issue returns true when any issue is LintSeverity::Error (e.g. rule arc/stale, project.rs:433).

LIVE REPRODUCTION (scratch /tmp/linttest, fresh `archiva init`, one recorded decision for app.js fn:add):
- Baseline (no issues): `archiva lint` -> exit=0, stdout=26 bytes ("No decision issues found."), stderr=0.
- CASE A (mutated anchored source so fingerprint drifts -> arc/stale ERROR): `archiva lint` -> exit=1, stdout=86 bytes ("ERROR arc/stale app.js fn:add: ... fingerprint differs ..."), stderr=0 bytes.
- CASE B (corrupted .dlog: changed `schema: 1` to `schema: nope`): `archiva lint` -> exit=1, stdout=0 bytes, stderr=25 bytes ("schema: expected integer").

Identical exit code 1 for two semantically distinct outcomes ("lint ran and found error-level findings" vs "lint could not run / file corrupt"); the only differentiator is which stream carries the text. My byte counts differ from the prior auditor's (A:86 vs claimed 161; B:25 vs claimed 29) purely because the scratch decision content/error strings differ — the structural assertion (same exit, different stream) holds exactly.

**Verifier notes / severity correction:** Claim is accurate in both code citation and reproduction; severity medium is appropriate. This is a verified defect in exit-code semantics (operational ergonomics), not a crash or data-loss bug. Common convention (e.g. ESLint, shellcheck, many linters) reserves exit 1 for "issues found" and exit 2 for "tool/internal error" precisely so CI and wrapper scripts can distinguish them; Archiva collapses both onto 1. Callers keying solely on exit status cannot tell "lint found stale decisions" from "a .dlog is corrupt and lint aborted." A caller could disambiguate by checking whether stderr is non-empty (CASE B writes only stderr, CASE A only stdout), but that is an undocumented and fragile contract — `archiva lint --help` documents no exit-code contract at all. Two minor scoping corrections to the prior report, neither of which changes the verdict: (1) the claimed byte counts (161/29) are setup-specific and did not reproduce verbatim (I got 86/25); the durable invariant is "same exit, different stream." (2) The claim's phrase "or vice versa" slightly overstates it — the two outcomes never actually swap streams (findings always go to stdout, run failures always to stderr), so a caller CAN distinguish via stream inspection; the defect is strictly that the *exit code* alone is ambiguous. Recommended resolution: return a distinct exit code (e.g. 2) from run_lint_cli's Err arm for run failures, keeping 1 reserved for "error-level findings present," and document the contract in lint_help().

**Recommended resolution:** Use a distinct exit code for operational failures (e.g. 2 for 'lint could not run', reserving 1 for 'lint found error-severity issues', 0 for clean), mirroring conventions like ESLint/ruff. At minimum document the stream/exit-code contract so callers can rely on it.

---

### F3. [LOW] Inconsistent error-message prefixing ('error:' vs bare) across the CLI

`techdebt` · location `src/cli.rs (arg-parsing errors use 'error: ...' e.g. lines 68-75,113-124,137-145; semantic errors are bare e.g. line 154 'line must be a positive integer', 253 'Missing file path...', and json/yaml/path errors via error.rs:100-108)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Argument-structure errors are prefixed with `error:` (commander-style: unknown option, unexpected argument, missing required argument). But other user-facing failures from the same commands are unprefixed: `line must be a positive integer`, `Missing file path. Pass one or set ARCHIVA_FILE.`, JSON/YAML parse messages (`Expected JSON literal`), and path-validation messages. The output style a user sees depends on which validation layer rejected them.

**Why it matters:** Inconsistent prefixing makes the surface feel unpolished and complicates downstream log parsing/grep rules that key on an `error:` prefix. It is cosmetic but visible on essentially every error path.

**Impact:** Minor UX/log-parsing inconsistency; no functional impact.

**Likelihood:** High — any user hitting a non-arg-structure error sees the unprefixed style.

**Evidence (reporter):** Observed side by side: `archiva nope` -> `error: unknown command 'nope'`; `archiva why src/a.ts 99999999999` -> `line must be a positive integer` (no prefix); `archiva hooks post-tool-use` -> `Missing file path. Pass one or set ARCHIVA_FILE.` (no prefix); `archiva write-decision --json 'not json'` -> `Expected JSON literal` (no prefix). All written to stderr with exit 1, but mixed prefixing.

**Independent verification:** Ran the binary in /tmp/archiva_audit (after `archiva init`):
- `archiva nope` -> `error: unknown command 'nope'` (exit 1)
- `archiva why src/a.ts 99999999999` -> `line must be a positive integer` (exit 1, NO prefix)
- `archiva hooks post-tool-use` -> `Missing file path. Pass one or set ARCHIVA_FILE.` (exit 1, NO prefix)
- `archiva write-decision --json 'not json'` -> `Expected JSON literal` (exit 1, NO prefix)
- `archiva why ../escape.ts` -> `Invalid project-relative path "../escape.ts": parent path segments are not allowed` (NO prefix)
- `archiva why -x` -> `error: unknown option '-x'` (prefixed)

Source confirms the two distinct layers exactly as claimed:
- src/cli.rs hardcodes the `error:` prefix only at arg-structure rejection sites: unknown command/option (lines 68-75, 99-103, 113-124, 273-277), unexpected argument (lines 84-88, 119-124, 140-145, 238-243, 314-319), missing required argument (line 138, 168), missing option value (line 297-299).
- Semantic errors are bare strings passed to ArchivaError::cli without a prefix: `line must be a positive integer` (cli.rs:154), `Missing file path...` (cli.rs:253).
- src/core/error.rs:82-110 `user_message()` returns bare messages for Json/Yaml (line 100-101), InvalidPath (line 97-99 `Invalid project-relative path ...`), Schema, Anchor, Git, Dmap, Mcp variants. The `error:` token only exists inside the hardcoded Cli-variant strings in cli.rs, never added centrally.
- src/main.rs:36-43 writes both stdout and stderr and exits with result.status; arg-parse and semantic failures both surface via stderr with exit 1, confirming the "same exit/stream, mixed prefix" observation. (Note: error.rs lives at src/core/error.rs, not src/error.rs as the claim's location line stated; line numbers 100-108 match.)

**Verifier notes / severity correction:** CONFIRMED as written; severity techdebt/low is accurate. Only correction is the file path: error.rs is at src/core/error.rs (the claim wrote src/error.rs), though the cited line range 100-108 is correct. Root cause is architectural-but-minor: the `error:` prefix is a literal embedded in arg-parsing strings in cli.rs rather than a property of the error type, so any error originating outside the arg-parser (semantic checks, json/yaml/path/schema layers via error.rs user_message) is emitted bare. No functional impact: all paths go to stderr with exit code 1; this is purely UX/log-parsing cosmetic inconsistency. A clean fix would centralize prefixing in main.rs (or user_message) and drop the inline literals, or deliberately drop the prefix everywhere for consistency.

**Recommended resolution:** Pick one convention (commander emits `error:` on stderr for all CLI-level failures) and apply it uniformly, e.g. have CliResult::err / ArchivaError::Cli prepend a consistent prefix, or strip it everywhere. Keep parser-origin messages but normalize the prefix at the CLI boundary.

---

### F4. [LOW] `why <file> <line>` accepts 0 and rejects out-of-range as 'positive integer', a self-contradictory contract

`defect` · location `src/cli.rs:147-156 (run_why line-vs-anchor branch)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The line/anchor disambiguator treats an arg as a line number iff it is non-empty and all ASCII digits, then parses to u32. `0` passes (all digits) and parses fine, so it is treated as line 0 and routed to why_for_line, producing `No decision found for src/a.ts at line 0.` But a value that overflows u32 (all digits) fails parse and yields the error `line must be a positive integer`. So 0 — not a positive integer — is accepted, while a large integer is rejected with a message claiming the requirement is 'positive integer'. The validation message does not match the actual accepted domain.

**Why it matters:** Line numbers are 1-based in every editor/tool; line 0 is meaningless and silently produces a 'not found' rather than an input error, while the error message for the overflow case misdescribes the rule. Minor correctness/clarity gap in a user-facing command.

**Impact:** Confusing behavior for edge inputs to `why`; no data corruption.

**Likelihood:** Low — requires passing 0 or an absurdly large line number.

**Evidence (reporter):** `archiva why src/a.ts 0` -> `No decision found for src/a.ts at line 0.` exit=0 (accepted). `archiva why src/a.ts 99999999999999999999` -> `line must be a positive integer` exit=1. Code: the branch guard at cli.rs:148-150 only checks is_ascii_digit (so 0 qualifies), then parse::<u32> at 152-154 fails on overflow with the 'positive integer' message.

**Independent verification:** Code read at /home/ubuntu/archaeo/src/cli.rs:147-156: the line-vs-anchor guard accepts an arg as a line iff `!line_or_anchor.is_empty() && line_or_anchor.bytes().all(|byte| byte.is_ascii_digit())` (lines 148-150). "0" satisfies this (non-empty, all ASCII digits), then `parse::<u32>()` succeeds, routing to project::why_for_line(... line=0). An all-digit value exceeding u32::MAX passes the same guard but fails parse, emitting `ArchivaError::cli("line must be a positive integer")` (line 154).

Ran the binary in /tmp/archiva_audit (git init + `archiva init`, file a.ts):
- `archiva why a.ts 0` -> "No decisions found for a.ts." exit=0 (ACCEPTED as line 0)
- `archiva why a.ts 99999999999999999999` -> "line must be a positive integer" exit=1 (REJECTED)
- `archiva why a.ts 4294967296` (u32::MAX+1) -> "line must be a positive integer" exit=1 (REJECTED)

So 0 (not a positive integer) is accepted while an all-digit overflow value is rejected with a message asserting the requirement is "positive integer". The accepted domain is actually 0..=u32::MAX, contradicting the error text. The routed line-0 path lands in src/core/decision.rs:162-173 (why_for_line_from_dlog), which compares line against lines_hint.start/end and returns either a decision or "No decision found for <file> at line 0." (the claim's exact "...at line 0." message appears when a .dlog exists for the file; my fresh project had none, so it returned "No decisions found for a.ts." — same accepted-as-line-0 behavior, different no-match string).

**Verifier notes / severity correction:** Confirmed as reported; severity low is correct (cosmetic/contract-clarity issue, no data corruption, no security impact). Minor scope correction: the claim's literal output "No decision found for src/a.ts at line 0." only renders when a .dlog already exists for the file; with no decisions logged the message is "No decisions found for <file>." (src/core/decision.rs:164). The behavioral defect — 0 accepted as a valid line while the rejection message claims a "positive integer" requirement — holds in both cases. Recommended resolution: either reject line==0 explicitly (treat 0 as out-of-domain alongside overflow) and/or change the error message to "line must be an integer between 1 and 4294967295" (or "must be a non-negative integer <= u32::MAX" if 0 is intentionally a valid sentinel). Aligning the accepted domain with the message text resolves the contradiction.

**Recommended resolution:** Reject 0 explicitly (and clarify the message, e.g. 'line must be a positive integer (>= 1)'); consider mapping a digit-string that overflows u32 to the same 'line out of range' wording so the contract is internally consistent.

---

### F5. [LOW] `archiva help help` and `help --help` are rejected though 'help' is an advertised command

`defect` · location `src/cli.rs:79-104 (run_help)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Main help lists `help [command]  display help for command`. But run_help has no `help` arm and no `-h/--help` handling: `archiva help help` falls into the catch-all and errors `unknown command 'help'`, and `archiva help --help` / `help -h` error `unknown command '--help'`/`'-h'`. commander.js shows help-for-help in these cases. So the one command that is purely about discovering help cannot describe itself.

**Why it matters:** Minor discoverability/consistency gap. A user exploring the CLI via `help <x>` hits an error for the self-referential and flag forms, which reads as a bug against the advertised `help [command]` surface.

**Impact:** Cosmetic/usability; no functional risk.

**Likelihood:** Low — only when a user types `help help` or `help --help`.

**Evidence (reporter):** `archiva help help` -> `error: unknown command 'help'` exit=1. `archiva help --help` -> `error: unknown command '--help'` exit=1. `archiva help -h` -> `error: unknown command '-h'` exit=1. All eight real subcommands (`help init`, `help why`, ...) return 0. Code: run_help match at cli.rs:89-104 has arms for init/why/history/hooks/status/lint/mcp/write-decision only; default returns unknown-command, and there is no `-h`/`--help` check before the match.

**Independent verification:** Code at /home/ubuntu/archaeo/src/cli.rs:79-105 (run_help): after the `args.len() > 1` guard (line 83), the match (lines 89-104) has arms only for init/why/history/hooks/status/lint/mcp/write-decision; the default arm (lines 98-103) returns `error: unknown command '<value>'`. There is no `help` arm and no `-h`/`--help` check before the match. Note `help [command]` IS advertised in main help — confirmed via `archiva --help`: `help [command]    display help for command`.

Ran /home/ubuntu/archaeo/target/release/archiva:
- `help help` -> `error: unknown command 'help'`, exit=1
- `help --help` -> `error: unknown command '--help'`, exit=1
- `help -h` -> `error: unknown command '-h'`, exit=1
- `help init` -> exit=0 (real subcommands work)

All three error cases reproduce exactly as claimed.

**Verifier notes / severity correction:** Fully confirmed as stated; no mis-scoping or mis-severity. Severity low/info is appropriate: purely cosmetic/usability, zero functional risk — no data path, parsing, or storage behavior is affected, and every real subcommand's help works. Minor nuance: `--help`/`-h` after `help` are reported as "unknown command" rather than "unknown option", which is slightly inconsistent with the top-level dispatcher (cli.rs:68-71) that distinguishes options (`starts_with('-')`) from commands; run_help collapses both into the command default. Recommended fix: in run_help add a `"help" => help_help()` (or alias to main_help) arm and treat `-h`/`--help` as a request for help-for-help before the match, mirroring commander.js. Trivial, low-priority polish; not a release blocker.

**Recommended resolution:** Add a `help`/`-h`/`--help` arm in run_help that returns main_help() (matching commander's behavior), so `archiva help help` and `archiva help --help` print help instead of erroring.

---

### F6. [LOW] Several subcommands validate only the first argument, masking trailing garbage when --help/help is present

`defect` · location `src/cli.rs:330-346 (run_status), 384-407 (run_mcp), 188-210 (hooks help/session-start arg checks)` · reporter-confidence high · verification **CONFIRMED**

**Description:** run_status and run_mcp inspect only args.first(): `archiva status --help extra` prints help and ignores the stray `extra` (exit 0), and `archiva mcp --help extra` likewise. So extra arguments after a recognized -h/--help are silently dropped rather than reported as unexpected. Contrast `archiva status extra` (proper error) and `archiva why src/a.ts fn:f extra` (proper 'unexpected argument'). The validation is inconsistent: whether trailing junk is caught depends on whether a help flag precedes it.

**Why it matters:** Inconsistent strictness. A user who fat-fingers an extra token after --help gets no feedback for some commands but an error for others. Low impact but a correctness/consistency wart against the otherwise strict 'unexpected argument' policy.

**Impact:** Silent acceptance of malformed input for status/mcp (and similar first-arg-only checks); no data risk.

**Likelihood:** Low — needs a help flag followed by extra tokens.

**Evidence (reporter):** `archiva status --help extra` -> prints status help, exit=0 (extra ignored). `archiva mcp --help extra` -> prints mcp help, exit=0. Compare `archiva status extra` -> `error: unexpected argument 'extra'` exit=1. Code: run_status only matches on args.first() (cli.rs:330) and never checks args.len(); run_mcp the same (cli.rs:390).

**Independent verification:** Ran the cited cases against /home/ubuntu/archaeo/target/release/archiva:
- `archiva status --help extra` → prints status help, exit=0 (extra silently dropped).
- `archiva mcp --help extra` → prints mcp help, exit=0.
- Contrast `archiva status extra` → `error: unexpected argument 'extra'` exit=1; `archiva mcp extra` → same, exit=1.

Code matches the claim. src/cli.rs:330 `run_status` only inspects `args.first()`; once it matches `-h|--help` it returns `status_help()` with no `args.len()` check, so trailing args are never examined. src/cli.rs:390 `run_mcp` is identical (matches on `args.first()` only). hooks `session-start` (cli.rs:211-225) checks `args.get(1)` for help first and returns before the `args.get(1)` unexpected-arg check, so `hooks session-start --help extra` → help, exit=0 (confirmed).

**Verifier notes / severity correction:** Confirmed and correctly scoped/severity. The claim's reasoning ("similar first-arg-only checks") is broader than stated: I confirmed the same first-flag-wins-over-trailing-junk behavior also affects `lint --help extra`, `write-decision --help extra`, and `hooks session-start --help extra` (all exit=0, extra ignored). Note write-decision/lint scan ALL args for a help flag (not just first), so `write-decision foo --help` also yields help — a slightly different but related inconsistency. This is purely cosmetic input-validation inconsistency: no data risk, no security impact, only affects malformed CLI invocations that include a help flag. Borderline info/low; low is acceptable. Root cause is the absence of a shared "no extra args after help" guard across subcommands; recommended fix is a small helper that, after detecting -h/--help, asserts no other non-flag tokens are present, applied uniformly.

**Recommended resolution:** After detecting -h/--help in these single-option commands, also assert no further args (mirroring run_help's args.len()>1 check at cli.rs:83-88) so trailing tokens are reported consistently.

---

### F7. [INFO] run_mcp's empty-args error branch is unreachable dead code via the binary

`techdebt` · location `src/cli.rs:384-389 (run_mcp is_empty branch) vs src/main.rs:26-34` · reporter-confidence high · verification **CONFIRMED**

**Description:** main.rs intercepts the case `args.len()==1 && args[0]=="mcp"` and routes it to serve_stdio before run_cli is ever called. run_cli/run_cli_result only dispatch `mcp` when there are additional args (args[1..] non-empty by construction of that path is not guaranteed, but the 1-arg case never reaches it). Consequently run_mcp([]) — the `if args.is_empty()` branch returning the 'available through the native binary entrypoint' message — cannot be reached through the compiled binary; reaching run_mcp at all requires `mcp <something>`, so args is never empty there.

**Why it matters:** Dead branch is harmless but misleading: a maintainer reading run_mcp may believe `archiva mcp` with no extra args prints that guidance string, when in fact main.rs starts the server. Slight maintenance hazard / confusion.

**Impact:** None at runtime; documentation/maintenance clarity only.

**Likelihood:** N/A (cannot trigger via binary).

**Evidence (reporter):** `archiva mcp </dev/null` -> starts server, exit=0 (handled by main.rs:26-34, not run_mcp). Reaching run_mcp needs `mcp foo` (-> unexpected argument) or `mcp --help`. There is no argv that delivers an empty slice to run_mcp, so cli.rs:385-388 is dead. The lint `unreachable!` at cli.rs:66 similarly documents that the `mcp`/`lint` arms in run_cli_result are partly vestigial.

**Independent verification:** Read src/main.rs:26-36 and src/cli.rs:40-77, 384-408. main.rs:26 intercepts exactly args==["mcp"] and calls serve_stdio, returning before run_cli. The sole dispatch to run_mcp is cli.rs:67 `"mcp" => run_mcp(&args[1..])`, reachable only when args[0]=="mcp"; since len==1 ["mcp"] is pre-intercepted, any reaching argv has len>=2, so args[1..] is non-empty by construction, making the is_empty() branch (cli.rs:385-388) unreachable via the binary. Ran the binary: `archiva mcp </dev/null` -> exit=0 (server, via main.rs); `archiva mcp foo` -> "error: unexpected argument 'foo'" exit=1 (cli.rs:399-403, the non-empty arg path, NOT the is_empty path); `archiva mcp --help` -> mcp_help exit=0 (cli.rs:392). No argv triggers the "available through the native binary entrypoint" message. grep of callers confirms only main.rs:36 and tests call run_cli; no test invokes bare ["mcp"]. The lint `unreachable!` at cli.rs:66 is corroborated: lint is intercepted at cli.rs:41-43 before run_cli_result.

**Verifier notes / severity correction:** Claim is accurate and correctly scoped to "via the binary." One clarification: run_cli is a pub library API, so the is_empty() branch IS reachable by an external library caller passing ["mcp"]; it is dead only through the compiled binary entrypoint, which is exactly the claim's scope. Severity info/techdebt is appropriate — no runtime impact, maintenance/clarity only. Recommended resolution: drop the is_empty() branch (and the trailing redundant `if let Some(arg)`/fallthrough mcp_help at cli.rs:390-407 which is also unreachable since args is always non-empty here) or add a debug_assert/comment documenting the binary-only invariant, mirroring the lint unreachable! at cli.rs:66.

**Recommended resolution:** Either remove the unreachable is_empty branch (and the now-redundant mcp arm in run_cli_result), or move the `mcp`-with-no-args handling entirely into run_cli so behavior is defined in one place and the special-case in main.rs is just an optimization, not the sole code path.

---

## MCP stdio JSON-RPC server  — score 7/10

> The MCP server is a line-delimited JSON-RPC 2.0 transport over stdio implementing initialize, tools/list, and tools/call for three tools (write_decision, why, ghost_check). It is well-engineered on the mechanics that matter most for robustness: newline framing handles multi-chunk lines, CRLF, and partial trailing lines; a 10 MiB per-line byte cap drains and rejects oversized input with -32700 while keeping the session alive; invalid UTF-8 and malformed JSON are reported and recovered from without crashing; notifications/* are correctly swallowed; and requests lacking an id with no method produce no response. I drove the binary end to end (initialize -> tools/list -> write_decision -> why -> ghost_check) and the round-trip is correct and produces text content identical to the CLI for why/write_decision, with ghost_check producing the same rule set as CLI lint for a given file. The principal weaknesses are spec/MCP-convention deviations inherited faithfully from the TypeScript original: tool execution errors (and unknown tools, bad args, missing anchors) are returned as JSON-RPC protocol errors with code -32000 rather than as MCP tool results with isError:true, which risks the rich guidance text (e.g. "Available anchors: ...") not reaching the model; unknown methods use -32000 instead of the spec-mandated -32601; the jsonrpc version field is not validated; and JSON arrays/batches are rejected with -32600. These are mostly faithful-to-TS tradeoffs rather than regressions, but they are real protocol-conformance gaps. No memory-safety or session-integrity defects were found.

*Score rationale:* Transport mechanics are robust and well-tested: multi-chunk/CRLF/partial-line framing, a correctly-drained 10 MiB oversize guard that preserves the session, UTF-8 and parse-error recovery, notification swallowing, and id-omission handling are all correct and I confirmed them by driving the binary. Content parity with the CLI for why/write_decision/ghost_check is exact. Points are deducted for genuine JSON-RPC/MCP conformance gaps -- tool errors surfaced as -32000 protocol errors instead of isError tool results (the most impactful for an agent-facing tool), unknown methods as -32000 instead of -32601, unvalidated jsonrpc version, and batch rejection -- several of which are faithful-to-TS choices but still deviate from the specs the audit measures against. No safety or session-integrity defects found.

**Verified behaviors (checked, not assumed):**

- Full round-trip over the real binary in /tmp: initialize -> tools/list -> tools/call write_decision -> why -> ghost_check all succeed; protocolVersion reported as 2024-11-05, serverInfo {name:archiva, version:0.2.0}, capabilities {tools:{}}.
- MCP why output is byte-identical to CLI 'archiva why src/calc.ts fn:compute' (same anchor/id/lines/chose/because/recorded text); MCP write_decision returns 'Recorded dec_001.' identical to CLI write-decision.
- ghost_check matches CLI lint rule set per file: stale ('arc/stale ...'), stale+supersede after status persisted, orphan ('arc/orphan ...'), and clean ('No issues found for <file>.'); confirmed file-scoped (unrelated dlogs untouched).
- id handling: string id echoed verbatim; array id [1,2] echoed verbatim; null/absent id with valid method -> id-less response; missing-method+id present -> -32600 'Missing method'; missing-method+null/absent id -> no response.
- Error codes observed: malformed JSON -> -32700 with id null (session continues); non-object request (array/batch) -> -32600 'Invalid request'; two JSON objects on one line -> -32700 'Unexpected trailing characters'; unknown method -> -32000; tools/call without handler in default path -> -32000.
- Framing: partial line with no trailing newline is processed at EOF; CRLF line endings trimmed correctly; oversize line (DEFAULT_MAX_BYTES+1) -> -32700 'JSON input exceeds configured byte limit' with id null, then the next valid request on the following line is answered (session survives).
- Invalid UTF-8 line (0xff) -> -32700 response, session continues to answer subsequent tools/list (confirmed via tests/mcp_stdio.rs and code path mcp.rs:236-254).
- notifications/* methods (including with an id present) produce no response (mcp.rs:344-346).
- Argument validation: why with missing file -> 'file: missing required field'; why with non-object arguments (string or array) -> 'expected object'; ghost_check with '../etc/passwd' -> path-hardening rejection 'parent path segments are not allowed'; all surfaced as -32000.
- Numeric id precision: ids >2^53 and exponent-form ids are rounded/normalized in the echoed response (9007199254740993->...992, 1e3->1000); verified node produces identical values, confirming faithful JS f64 parity rather than a Rust-specific regression.
- jsonrpc version is not validated: requests with missing jsonrpc field or 'jsonrpc':'1.0' are processed normally; boolean id 'false' is accepted and echoed.

### F8. [MEDIUM] tools/call execution errors returned as JSON-RPC protocol errors (-32000) instead of MCP tool results with isError:true

`architecture` · location `src/mcp.rs:405-414 (handle_tool_call), src/mcp.rs:108-137 (ProjectToolHandler::call_tool)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Every tool-layer failure -- unknown tool name, missing/empty required argument, non-object arguments, a missing source anchor, a hardened path rejection -- is mapped to a JSON-RPC error envelope with code -32000 via error_response(id, -32000, &message). The MCP 2024-11-05 convention for tools/call is that tool *execution* errors are reported inside the result object as {content:[...], isError:true}, reserving JSON-RPC protocol errors for transport/dispatch failures. Returning protocol errors means MCP clients treat these as RPC failures; the helpful diagnostic text the tool produces (e.g. the write_decision 'Available anchors in src/calc.ts: export:compute, fn:compute.' guidance) is buried in an error.message and may never be surfaced to the model that called the tool. For a tool whose entire purpose is feeding decision context to an AI agent, losing that guidance path is consequential.

**Why it matters:** The product's value is the agent reading tool output. MCP clients commonly render isError tool results back to the model but treat -32000 protocol errors as opaque failures, so the corrective guidance (available anchors, why a path was rejected) can be dropped.

**Impact:** Agents driving the server may not receive actionable error guidance; behavior is non-conformant with MCP tool-error conventions.

**Likelihood:** Occurs on every failed tools/call (wrong anchor, bad path, validation failure).

**Evidence (reporter):** Ran: printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"write_decision","arguments":{"file":"src/calc.ts","anchor":"fn:nope","lines":[1,3],"chose":"x","because":"y","rejected":[]}}}' | archiva mcp -> {"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"Anchor \"fn:nope\" does not exist ... Available anchors ...: export:compute, fn:compute."}}. Also id:1 unknown_tool -> error -32000 'Unknown tool: unknown_tool'; id:3 missing file -> error -32000 'file: missing required field'. None return a result with isError:true.

**Independent verification:** Code read confirms the cited paths exactly. src/mcp.rs:405-414 handle_tool_call maps every Err(message) from the handler to error_response(id, -32000, &message); the only success path is success_response. src/mcp.rs:108-137 ProjectToolHandler::call_tool returns Result<JsonValue, String>: unknown tool -> Err("Unknown tool: ..."), and each tool path uses map_err(user_message) so input-parse failures, missing/empty fields, missing anchors, and path-validation rejections all become Err(String). src/mcp.rs:348-364 also returns -32000 for unsupported methods. Crucially, `grep -rn "isError" src/ tests/` returns nothing (exit 1) — the success/result envelope (text_result, src/mcp.rs:391-399) never sets isError, so there is literally no code path that emits an MCP tool-execution-error result.

Live reproduction against /home/ubuntu/archaeo/target/release/archiva in /tmp/archiva_audit (git-inited, `archiva init`, src/calc.ts with export function compute):
id:1 initialize -> result protocolVersion "2024-11-05" (server advertises the MCP version whose convention this violates; confirmed const at src/core/version.rs:7).
id:2 unknown_tool -> {"error":{"code":-32000,"message":"Unknown tool: unknown_tool"}}
id:3 missing file field -> {"error":{"code":-32000,"message":"file: missing required field"}}
id:4 anchor fn:nope -> {"error":{"code":-32000,"message":"Anchor \"fn:nope\" does not exist in src/calc.ts. A decision recorded against a missing anchor is an immediate orphan. Available anchors in src/calc.ts: export:compute, fn:compute."}}
id:5 valid call -> {"result":{"content":[{"type":"text","text":"Recorded dec_001."}]}}
This matches the claimed transcript precisely: the actionable "Available anchors" guidance is buried inside error.message of a -32000 JSON-RPC error envelope, while only the success path returns the MCP content array.

**Verifier notes / severity correction:** Claim is accurate and correctly scoped; severity medium stands. All five sub-claims verified: unknown-tool, missing-required-field, and missing-anchor cases all return -32000, and no result with isError:true exists anywhere (the field is entirely absent from the codebase). The most consequential instance is the missing-anchor case, whose diagnostic ("A decision recorded against a missing anchor is an immediate orphan. Available anchors ...: export:compute, fn:compute.") is exactly the agent-facing guidance the tool exists to provide, yet it is delivered as a JSON-RPC protocol error rather than an MCP tool result.

Two fairness nuances that do NOT change the verdict: (1) Under MCP, classifying an *unknown tool name* as a JSON-RPC error is defensible (it is arguably a dispatch failure), so that single sub-case is the weakest part of the claim; the strong, clearly-nonconformant cases are the in-tool execution failures (missing field, missing anchor, path rejection). (2) Real-world impact depends on the client: many MCP clients do log error.message, but the 2024-11-05 convention exists precisely so tool errors flow back into the model's tool-result context (isError:true) where the calling agent reliably reads them; -32000 errors are commonly treated as transport faults and not re-fed to the model. So the "guidance may never reach the model" impact is genuine but client-dependent — consistent with medium, not high. Recommended resolution: in handle_tool_call, return tool-execution failures as success_response(id, result_with_isError_true) where result = {content:[{type:"text",text:message}], isError:true}, and reserve -32000/-32601/-32600 for true dispatch/transport failures (malformed params, unknown method).

**Recommended resolution:** For tools/call, wrap tool-handler Err(message) in a success_response whose result is text_result(message) with an added isError:true field, per MCP. Reserve -32000/-32602 for genuine dispatch/param-shape failures. This is faithful behavior the TS original also got wrong (server.ts:56-65 throws -> -32000), so fixing it is a deliberate conformance improvement, not a parity break that affects content.

---

### F9. [LOW] Unknown JSON-RPC method returns -32000 instead of spec-mandated -32601 (Method not found)

`defect` · location `src/mcp.rs:359-364` · reporter-confidence high · verification **CONFIRMED**

**Description:** The catch-all match arm returns error_response(id, -32000, &format!("Unsupported MCP method: {method}")) for any method other than initialize/tools/list/tools/call. JSON-RPC 2.0 mandates code -32601 'Method not found' for unknown methods; -32000 is the reserved range for implementation-defined server errors. A client probing standard MCP methods it expects a server might support (e.g. resources/list, prompts/list, completion/complete) receives -32000 rather than the discoverable -32601.

**Why it matters:** The audit brief explicitly lists -32601 as an expected error code. Clients that branch on -32601 to detect unsupported-but-known methods will misclassify this server's responses.

**Impact:** Protocol non-conformance; clients cannot distinguish 'method not found' from a generic server error.

**Likelihood:** Every call to a non-implemented method.

**Evidence (reporter):** Ran: printf '{"jsonrpc":"2.0","id":4,"method":"does/not/exist"}' | archiva mcp -> {"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"Unsupported MCP method: does/not/exist"}}.

**Independent verification:** Read src/mcp.rs:348-364. The match on `method` handles only "initialize", "tools/list", "tools/call"; the catch-all `_ => Some(error_response(id.cloned(), -32000, &format!("Unsupported MCP method: {method}")))` returns code -32000 for any other method. (The "tools/call" arm with no handler also returns -32000, but that is a separate path.)

Reproduced against the built binary in /tmp:
1. `printf '{"jsonrpc":"2.0","id":4,"method":"does/not/exist"}\n' | archiva mcp` -> `{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"Unsupported MCP method: does/not/exist"}}`
2. `printf '{"jsonrpc":"2.0","id":7,"method":"resources/list"}\n' | archiva mcp` -> `{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"Unsupported MCP method: resources/list"}}`

Both match the claimed evidence verbatim. JSON-RPC 2.0 reserves -32601 for "Method not found"; -32000..=-32099 is the implementation-defined server-error range. Returning -32000 for an unknown method is a genuine spec non-conformance.

**Verifier notes / severity correction:** The defect is real and reproduced exactly; the location (src/mcp.rs:359-364) and the technical description (wrong error code for unknown methods) are accurate. I am correcting severity from medium to low. Rationale: the practical impact is minimal. MCP clients discover server capabilities by reading the `capabilities` object returned from `initialize` (which here advertises only `tools`), not by probing for methods and inspecting error codes. An unknown method still yields a well-formed JSON-RPC error object with a clear, descriptive message and the correct request id, so no client breaks; only the numeric code is non-canonical. It is a correctness/conformance defect worth a one-line fix (change the catch-all to -32601, and arguably keep -32000 for the no-handler "tools/call" case which is a state error rather than a missing method), but it does not rise to medium given negligible functional/security/data impact. Confidence: high.

**Recommended resolution:** Return -32601 for unknown methods. Note this faithfully mirrors the TS original (server.ts:83 throws -> -32000), so it is a parity-preserving but spec-incorrect choice; correcting it is low-risk.

---

### F10. [LOW] jsonrpc version field is never validated (missing or wrong version accepted)

`defect` · location `src/mcp.rs:322-365 (handle_protocol_request_internal reads only id and method)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The request dispatcher inspects only object.get("id") and object.get("method"); it never checks that a "jsonrpc":"2.0" member is present and equal to the string "2.0". Requests with "jsonrpc":"1.0" or with the field entirely absent are processed and answered as if valid. JSON-RPC 2.0 requires the version member and the spec says a missing/incorrect version is an Invalid Request (-32600).

**Why it matters:** Lax version handling means the server cannot reject clients speaking the wrong protocol, and silently answers malformed requests.

**Impact:** Minor robustness/conformance gap; could mask client bugs.

**Likelihood:** Only with non-conformant clients.

**Evidence (reporter):** Ran: printf '{"id":1,"method":"initialize"}' | archiva mcp -> normal initialize result (no jsonrpc field). printf '{"jsonrpc":"1.0","id":6,"method":"initialize"}' -> normal result.

**Independent verification:** Read src/mcp.rs:322-365 (handle_protocol_request_internal). The function only reads object.get("id") (line 333) and object.get("method") (line 334); there is no reference to a "jsonrpc" key anywhere in the dispatcher or its surrounding parse path. Confirmed by grep: no occurrence of the literal "jsonrpc" as a lookup key on incoming requests in mcp.rs — it appears only in outgoing response construction.

Runtime reproduction in /tmp/mcptest (after `archiva init`), driving `archiva mcp` over stdio:
- printf '{"id":1,"method":"initialize"}\n' (no jsonrpc field) -> {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05",...}} — accepted, normal success result.
- printf '{"jsonrpc":"1.0","id":6,"method":"initialize"}\n' (wrong version) -> {"jsonrpc":"2.0","id":6,"result":{...}} — accepted, normal success result.
- printf '{"jsonrpc":"2.0","id":7,"method":"initialize"}\n' (correct) -> identical success result.

All three produce the same successful initialize response, proving the version member is neither required nor checked against "2.0". Per JSON-RPC 2.0, a missing/incorrect jsonrpc member is an Invalid Request (-32600), so the spec-correct behavior would be to reject the first two.

**Verifier notes / severity correction:** Claim is accurate in code location, mechanism, evidence, and severity. The dispatcher at src/mcp.rs:333-342 extracts only `id` and `method` and never inspects `jsonrpc`. Severity low is correct: this is a conformance/robustness gap, not a security or correctness defect for real clients (compliant MCP clients always send jsonrpc:"2.0"). Impact is limited to silently tolerating malformed/legacy clients and masking client bugs; it cannot cause data corruption or crashes. Recommended resolution: after confirming the value is an Object, validate that object.get("jsonrpc") == Some(JsonValue::String("2.0")) and otherwise return error_response(id-or-Null, -32600, "Invalid request") — matching the existing -32600 path already used for non-object requests at lines 327-331. Minor nuance worth noting: strict enforcement could break the existing lenient behavior if any internal test or tooling relies on omitting the field, so add it as a validation step with a test rather than assuming no callers depend on the leniency.

**Recommended resolution:** Validate the jsonrpc member equals "2.0" and return -32600 otherwise. Matches the TS original's leniency, so document as an intentional tolerance if not fixed.

---

### F11. [LOW] JSON-RPC batch arrays are rejected as -32600 rather than processed; also diverges from TS (which silently swallows them)

`tradeoff` · location `src/mcp.rs:326-332 (non-object request -> -32600 'Invalid request')` · reporter-confidence high · verification **CONFIRMED**

**Description:** A top-level JSON array (a JSON-RPC batch) is not an Object, so the early guard returns error_response(Null, -32600, 'Invalid request'). The server does not implement batch processing. The 2024-11-05 MCP/JSON-RPC lineage permits batches; later MCP revisions removed them, so this is a defensible omission. However it is also a behavioral divergence from the TS original, which JSON.parses the array, finds request.method undefined and request.id undefined, and therefore emits nothing at all -- whereas Rust emits a -32600 error with id null.

**Why it matters:** Batch is rarely used over stdio, but the divergence means a client that sent a batch sees an error from Rust and silence from TS; any differential harness comparing the two could flag it.

**Impact:** Negligible in practice for current MCP clients; relevant only for batch-using clients or strict differential testing.

**Likelihood:** Low (batching uncommon over stdio).

**Evidence (reporter):** Ran: printf '[{"jsonrpc":"2.0","id":10,"method":"initialize"},{"jsonrpc":"2.0","id":11,"method":"tools/list"}]' | archiva mcp -> {"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid request"}} (single error, batch not executed).

**Independent verification:** Code read at src/mcp.rs:322-332: handle_protocol_request_internal opens with `let JsonValue::Object(object) = request else { return Some(error_response(Some(JsonValue::Null), -32600, "Invalid request")); }`. A well-formed top-level JSON array parses cleanly via json::parse (not a -32700 parse error), enters handle_protocol_request_internal via the dispatch chain handle_protocol_line_with_tool_handler -> handle_protocol_request_with_tool_handler -> handle_protocol_request_internal (src/mcp.rs:28-54, 258), fails the Object refutable let, and returns the -32600 error with id null. There is no JsonValue::Array arm before this guard, so no batch processing exists.

Ran the exact reproduction:
  printf '[{"jsonrpc":"2.0","id":10,"method":"initialize"},{"jsonrpc":"2.0","id":11,"method":"tools/list"}]' | /home/ubuntu/archaeo/target/release/archiva mcp
Output (single line, batch not executed):
  {"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid request"}}

This is byte-identical to the prior auditor's claimed evidence. Neither the initialize nor tools/list element was processed; exactly one -32600 error with id:null was emitted.

**Verifier notes / severity correction:** The Rust-side behavioral claim is fully confirmed: a top-level JSON-RPC batch array is rejected as -32600 "Invalid request" with id null, and the array's contained requests are never executed. Categorization as tradeoff/low is accurate and not inflated: batch support was present in the 2024-11-05 JSON-RPC lineage but removed in later MCP spec revisions, and no current mainstream MCP client sends top-level batches over stdio, so practical impact is negligible.

One scope caveat: the sub-claim that the TS original "JSON.parses the array, finds method/id undefined, and therefore emits nothing" could NOT be independently verified — the TS source is not present in this repo (/home/ubuntu/archaeo is the Rust re-engineering), so I did not run or read the TS implementation. That divergence assertion is plausible and internally consistent (an array has no `.method`/`.id`, and the described TS guard would no-op), but it rests on the prior auditor's description, not my own observation. The differential-behavior framing should be treated as PLAUSIBLE rather than independently confirmed. This does not change the severity: even if TS also emitted something, the finding remains a low-severity tradeoff. The primary, actionable claim (Rust returns -32600/id:null and does not process batches) is CONFIRMED with high confidence.

**Recommended resolution:** Either implement batch (iterate array, collect responses, omit notification results) or document explicitly that batch is unsupported. If parity with TS matters, suppress the response for array inputs.

---

### F12. [LOW] Numeric id precision loss and normalization (ids stored/echoed as f64)

`operational` · location `src/core/json.rs:11 (Number(f64)), src/core/json.rs:340-379 (parse_number), src/core/json.rs:535-552 (format_number); echoed via src/mcp.rs:609-616 success_response/id.cloned()` · reporter-confidence high · verification **CONFIRMED**

**Description:** All JSON numbers are parsed into f64, so a request id larger than 2^53 or written in float/exponent form is silently rounded/normalized when echoed back in the response. The response id therefore may not byte-match the request id a client sent. This is faithful to the TypeScript original (JavaScript also uses IEEE-754 doubles and produces identical mangling -- verified with node), so it is not a regression, but it is a correlation hazard for any client that uses large integer ids and matches responses by exact id value.

**Why it matters:** JSON-RPC clients correlate responses to requests by id; a mismatched echoed id can break correlation for clients using ids > 2^53 or non-canonical numeric forms.

**Impact:** Response/request correlation can fail for clients using very large numeric ids. String ids are unaffected and round-trip exactly (verified).

**Likelihood:** Low; most MCP clients use small integer or string ids.

**Evidence (reporter):** Ran: id 9007199254740993 -> echoed 9007199254740992; id 12345678901234567890 -> echoed 12345678901234567000; id 1e3 -> echoed 1000. node JSON.parse/stringify produces the identical values, confirming faithful JS parity. String id 'str-id' and array id [1,2] echo back unchanged.

**Independent verification:** Code locations all verified by reading source:
- src/core/json.rs:11 — `Number(f64)` confirmed: JSON numbers are stored as f64.
- src/core/json.rs:340-379 — parse_number parses the raw slice via `raw.parse::<f64>()` (lines 376-378), so all numeric ids become f64.
- src/core/json.rs:535-552 — format_number serializes from f64 (to_string + exponent normalization).
- src/mcp.rs:333 `let id = object.get("id")`, echoed via `id.cloned()` into success_response/error_response (lines 349-360, 609-616). The id is the parsed JsonValue, so the f64 round-trip applies on echo.

Reproduction — drove the release binary's MCP server over stdio (cd /tmp/mcptest, git init + archiva init), piping newline-delimited JSON-RPC requests:
  id 9007199254740993  -> echoed "id":9007199254740992   (off by 1)
  id 12345678901234567890 -> echoed "id":12345678901234567000
  id 1e3               -> echoed "id":1000               (normalized)
  id "str-id"          -> echoed "str-id"  (exact round-trip)
  id [1,2]             -> echoed [1, 2]     (exact round-trip; full value confirmed via python json.loads)
  id 42                -> echoed 42         (exact)

Node parity confirmed: `node -e JSON.parse` on the same literals yields 9007199254740992, 12345678901234567000, 1000 — byte-identical mangling, so this is faithful to the TS/JS original and not a regression introduced by the Rust port.

**Verifier notes / severity correction:** Claim is accurate in every particular: code locations, the f64 storage/parse/format path, the mcp.rs id.cloned() echo, the exact reproduced values, the JS-parity rationale, and the note that string/array ids round-trip exactly. Severity low and category operational are correct. One small nuance worth recording: JSON-RPC 2.0 states the response id MUST equal the request id and that ids SHOULD NOT contain fractional parts; echoing a numerically-different value (9007199254740993 -> ...992) is technically a minor spec deviation for >2^53 integer ids, but the real-world correlation hazard is narrow (only clients using ids beyond 2^53 or float/exponent forms) and the behavior matches the TS original, so low severity is justified rather than elevated. No correction to scope needed.

**Recommended resolution:** Preserve the original id token verbatim for echo (store the raw lexeme alongside the parsed value, or special-case id passthrough). If parity with TS is the priority, leave as-is and document the >2^53 id limitation. Recommend at minimum documenting 'use string ids'.

---

### F13. [LOW] Invalid id types (boolean) are accepted and echoed instead of rejected

`defect` · location `src/mcp.rs:333 (id read untyped), src/mcp.rs:609-616 (id echoed unconditionally)` · reporter-confidence high · verification **CONFIRMED**

**Description:** JSON-RPC 2.0 restricts id to String, Number, or Null. The server reads id as an arbitrary JsonValue and echoes it back without type checking, so a boolean id is accepted and reflected. Object/array ids are likewise echoed (array id [1,2] verified). This is harmless in practice and matches the untyped TS handling, but it is a minor conformance gap.

**Why it matters:** Strict clients/proxies may reject responses carrying a non-conformant id type.

**Impact:** Negligible; only affects deliberately malformed clients.

**Likelihood:** Very low.

**Evidence (reporter):** Ran: printf '{"jsonrpc":"2.0","id":false,"method":"initialize"}' | archiva mcp -> {"jsonrpc":"2.0","id":false,"result":{...}}.

**Independent verification:** Code read: src/mcp.rs:333 reads `let id = object.get("id");` as an untyped JsonValue with no type discrimination. The only type-aware handling of id is for the missing-method branch (lines 337-341, distinguishing Null/None vs other). For valid methods, id is passed through `id.cloned()` to success_response/error_response (lines 349-358), which at src/mcp.rs:609-616 unconditionally push whatever id was supplied into the response (`if let Some(id) = id { entries.push(("id", id)); }`) with no check that it is String/Number/Null.

Runtime reproduction (built release binary, cwd /tmp):
  printf '{"jsonrpc":"2.0","id":false,"method":"initialize"}\n' | archiva mcp
  -> {"jsonrpc":"2.0","id":false,"result":{"protocolVersion":"2024-11-05",...}}
  printf '{"jsonrpc":"2.0","id":[1,2],"method":"initialize"}\n' | archiva mcp
  -> {"jsonrpc":"2.0","id":[1,2],"result":{...}}
  printf '{"jsonrpc":"2.0","id":{"a":1},"method":"initialize"}\n' | archiva mcp
  -> {"jsonrpc":"2.0","id":{"a":1},"result":{...}}

Boolean, array, and object ids are all accepted and reflected verbatim. JSON-RPC 2.0 spec (sec 4) restricts id to String, Number, or Null; SHOULD NOT contain fractional Number and MUST be one of those three types.

**Verifier notes / severity correction:** Claim is accurate in every particular: cited locations (src/mcp.rs:333 untyped read, src/mcp.rs:609-616 unconditional echo) are correct, the boolean-id reproduction matches exactly, and the array-id [1,2] echo is also reproduced as stated. I additionally verified object ids ({"a":1}) are echoed too, consistent with the claim's "object/array ids are likewise echoed." Severity low is appropriate: this is a JSON-RPC 2.0 conformance gap with negligible practical impact — real MCP clients (Claude Desktop, etc.) only send string/number ids, the malformed id must be deliberately crafted, and echoing it back causes no crash, state corruption, or security issue. Correctly categorized as a defect (minor spec-conformance), not an architectural issue. Recommended resolution (optional, low priority): at the id read, validate that id is String/Number/Null and otherwise return error -32600 with a Null id, or normalize non-conforming ids to Null in responses.

**Recommended resolution:** Optionally validate id is string/number/null and return -32600 for other types. Low priority.

---

### F14. [INFO] ghost_check reimplemented as file-scoped lint_file rather than TS project-wide lint+filter (verified result-equivalent, no defect)

`tradeoff` · location `src/mcp.rs:124-133 (ghost_check -> project::lint_file file-scoped), vs TS src/mcp/server.ts:96-99 (lintProject(root).filter(file===input.file))` · reporter-confidence high · verification **CONFIRMED**

**Description:** The TS ghost_check runs lintProject over the entire repo (mutating stale/orphan status across ALL dlogs as a side effect) then filters the issue list to the requested file. The Rust version calls lint_file, which lints and mutates only the requested file's dlog. I verified the user-visible text result is identical across stale, orphan, and clean cases, and confirmed (tests and direct runs) that unrelated dlogs are not mutated. This is a deliberate, tested behavioral improvement (less I/O, no surprising cross-file mutation), not a divergence in returned content. Flagging only so reviewers know the implementation intentionally differs from the TS reference and a naive differential harness that inspects whole-project side effects could see a difference.

**Why it matters:** Reviewers comparing against the TS reference should know the scoping is intentional and that returned results are equivalent.

**Impact:** None on returned tool output; reduces write amplification and avoids mutating unrelated files.

**Likelihood:** N/A.

**Evidence (reporter):** Stale: MCP ghost_check on freshly-stale file -> 'arc/stale fn:compute: ...'; after status persisted, MCP and CLI both report stale+supersede ('arc/stale ...\narc/supersede ...'). Orphan: MCP 'arc/orphan fn:gone: fn:gone no longer exists in src/o.ts' matches CLI 'WARNING arc/orphan src/o.ts fn:gone: ...'. Clean: 'No issues found for src/calc.ts.'. Test ghost_check_is_file_scoped_and_does_not_mutate_unrelated_dlogs (mcp.rs:1056) confirms unrelated dlog status stays None.

**Independent verification:** Code matches the cited locations and behavior. src/mcp.rs:124-133 calls project::lint_file (single-file) then format_ghost_check_result; TS src/mcp/server.ts:96-99 calls lintProject(projectRoot).filter(issue.file===input.file). Traced the mutation path: lint_file (src/core/project.rs:97-112) only locks/loads the requested file's dlog via lint_dlog_locked (412-428), and lint_dlog (459-550) is the sole stale-marking site (write_dlog/write_dmap at 544-547) — so only the requested file can be mutated. lint_project_inner (123-162) iterates ALL dlog files, so the TS whole-project lint mutates every drifted dlog. Both paths feed the SAME per-file logic for the returned issue list: dlog issues (stale/orphan/supersede) depend only on that file's dlog+source, and undecided issues route through the identical lint_complex_undecided_file (570-620) with an identical file-scoped `decided` set, so returned content is provably equal and issue ordering (dlog-issues-then-undecided within a file) is preserved in both.

Ran the binary in /tmp/gcaudit to reproduce all four claimed cases:
- Non-mutation: wrote decisions for src/calc.ts and src/other.ts via MCP, drifted other.ts to `return 999`, ran ghost_check on src/calc.ts -> "No issues found for src/calc.ts." and other.ts.dlog had NO status field added before AND after (confirmed unrelated dlog not mutated).
- Stale: ghost_check on drifted calc.ts (1st run) -> "arc/stale fn:compute: ...code fingerprint differs..."; (2nd run, status persisted) -> "arc/stale ...\narc/supersede fn:compute: ...is stale and has not been superseded". CLI `lint` emits the same two rows (ERROR arc/stale / arc/supersede src/calc.ts).
- Orphan: MCP -> "arc/orphan fn:gone: fn:gone no longer exists in src/o.ts"; CLI -> "WARNING arc/orphan src/o.ts fn:gone: ...".
- Rule names verified at src/core/lint.rs:37-41 (arc/parser, arc/stale, arc/orphan, arc/supersede); formatter at lint.rs:77-94 produces "rule anchor: message" and "No issues found for X." matching TS server.ts:99.
- Cited test mcp.rs:1057 (ghost_check_is_file_scoped_and_does_not_mutate_unrelated_dlogs) asserts unrelated dlog .status == None after ghost_check on a clean target; it exists and matches the description.

**Verifier notes / severity correction:** Claim is accurate and correctly scoped. The implementation genuinely differs from the TS reference (file-scoped lint_file vs whole-project lint+filter), but the user-visible tool output is provably result-equivalent for stale, orphan, supersede, undecided, and clean cases — and I independently reproduced every claimed output via both the MCP stdio server and the CLI. The only real difference is a side-effect: TS mutates (stale-marks) ALL drifted dlogs as a side effect of a single ghost_check, whereas Rust mutates only the requested file. This is a legitimate improvement (less write amplification, no surprising cross-file mutation), not a defect. severity=info is correct; this is a documented tradeoff, not a bug. One caveat for completeness: a naive differential test harness that snapshots whole-project on-disk dlog state after a ghost_check WILL observe a difference (e.g., other.ts gaining status:stale under TS but not under Rust) — the claim already calls this out, which is the entire reason it is worth flagging. No correction to scope or severity needed.

**Recommended resolution:** No change needed. Document the intentional scoping in code comments so it is not 'corrected' back to a project-wide scan.

---

## Anchor extraction engine (largest module)  — score 7/10

> The anchor extraction engine is a 12,341-line hand-rolled, multi-language (TS/JS, TSX, Rust, C/C++) lexer+parser that produces named code anchors (functions, methods, classes/structs/enums/traits/impls, nested modules, function-local items, significant if-blocks) with cyclomatic complexity and line ranges, plus graceful-degradation diagnostics. I verified its behavior by building a harness that calls the public extract_anchors and by differentially comparing it against the real ts-morph TS oracle (dist/src/core/anchor.js). Fidelity is genuinely high: 300 randomized differential tests and dozens of hand-crafted TS/TSX edge cases (decorators, overloads, generators, conditional/optional types, nullish coalescing, template-literal interpolation complexity, computed/private/static members, collision #N suffixing, unicode identifiers) produced ZERO divergences from the oracle. 20,000 structured-fuzz inputs across all five languages produced no panics, and the `complete`/diagnostics flag correctly gates orphan marking so partial parses never falsely orphan a decision. However I found five real defects: (1) a stack overflow that aborts the real binary on deeply nested Rust input; (2) O(n^2) blowup on flat declaration-dense files (half a second for a plausible 3k-line generated barrel file); (3) C++ `enum class`/`enum struct` emits a phantom second anchor that also corrupts the identity of adjacent real types; (4) C++ destructors are anchored identically to constructors; and (5) a fingerprint normalization divergence on U+FEFF/BOM versus the TS oracle that causes false drift across the TS->Rust migration. The Rust and C/C++ paths have no oracle and the C-family path is by far the least-tested (~3 test references), which is exactly where the correctness bugs surfaced. The 12k-line hand-written parser is impressive and works, but its size, dense helper graph (100+ functions), and absence of recursion/size guards make it the highest-risk module to maintain and extend.

*Score rationale:* The TS/JS/TSX extractor is genuinely high quality: byte-for-byte agreement with the ts-morph AST oracle across 300 randomized differential tests and extensive hand-crafted edge cases (decorators, overloads, conditional/optional types, nullish, template-literal interpolation complexity, collision suffixing, unicode), 20k fuzz inputs with no panics, and a well-designed complete/diagnostics contract that correctly prevents false orphaning under partial parses. That is excellent engineering for a hand-rolled parser. It loses points for five concrete defects, weighted by blast radius: an unbounded-recursion stack-overflow that aborts the real binary (status/write-decision), a genuine O(n^2) blowup on flat declaration-dense files that the repo's own nested files happened to mask, a TS-incompatible BOM fingerprint divergence that breaks the migration's staleness promise, and two C++ anchor-identity bugs (enum class phantom + destructor==constructor) in the least-tested, no-oracle path. None corrupt stored decision data silently beyond fingerprints, and TS fidelity is strong, so the engine is close to release-ready but the stack overflow and the fingerprint divergence in particular should be addressed before shipping.

**Verified behaviors (checked, not assumed):**

- Built an example binary over the public extract_anchors and a Node oracle over the compiled ts-morph extractAnchors (dist/src/core/anchor.js); ran 300 randomized differential TS tests -> 0 divergences in anchor name/lines/complexity.
- Hand-crafted TS/TSX edge cases (classes, methods, getters/setters, computed/private/static members, decorators, overloads, generators, conditional types, optional params, nullish, regex, switch, template-literal interpolation complexity, default/named/aliased exports, namespaces, enums, unicode identifiers, duplicate-name #N suffixing) all matched the oracle exactly.
- Fuzzed ~20,000 structured token-soup inputs across ts/tsx/rs/c/cpp -> zero panics; malformed inputs (unterminated string/template/block-comment, unbalanced braces, null bytes, empty) produce graceful diagnostics with complete=false.
- Verified stack overflow (SIGABRT exit 134) on the real archiva binary: `status` and `write-decision` crash on a Rust file with ~15000-20000 nested `mod`/blocks; threshold between depth 10000 (OK) and 15000 (crash); no production worker-thread/stack guard (thread::spawn at project.rs:970 is test-only).
- Measured O(n^2) extraction: TS many-funcs 5000/10000/20000 = 326ms/1.46s/5.49s; Rust = 623ms/2.69s/11.1s; nested ifs 1000-8000 = 11/41/173/743ms; a realistic 3000-line generated barrel file = 548ms; root-caused to is_top_level / rust_depths_before / per-if complexity_between prefix rescans.
- Reproduced C++ `enum class Color` -> phantom `class:Color` anchor in addition to `enum:Color`; confirmed a decision can be recorded against the phantom; with an adjacent real `class Color` the real one is demoted to `class:Color#2`.
- Reproduced C++ destructor `~A()` anchored as `fn:A.A`, identical to the constructor.
- Confirmed fingerprint divergence vs TS oracle on U+FEFF/BOM (leading BOM: Rust 1c28dc3f vs JS ba7816bf) using codepoint-built inputs; U+2028/2029/00A0/3000/000B/200B all agree.
- Verified end-to-end drift pipeline on the real binary: wrote a decision, mutated the function body, and confirmed status reports 1 stale and lint emits arc/stale + arc/supersede; the complete flag correctly gates orphan marking (incomplete extraction never falsely orphans).
- Confirmed real binary extraction performance is fine on actual repo files: project.rs (2261 lines) 8ms, git.rs (4329) 36ms, anchor.rs (12341) 86ms; the quadratic only bites flat declaration-dense files. All 57 in-module anchor tests pass.

### F15. [HIGH] Stack overflow aborts the binary on deeply nested Rust source (status/write-decision/MCP DoS)

`defect` · location `src/core/anchor.rs:733 (collect_rust_item_anchors), recurses at 857-869` · reporter-confidence high · verification **CONFIRMED**

**Description:** collect_rust_item_anchors recurses once per nested brace scope (modules and any local block that can contain items) with no depth limit, and extraction runs on the main thread (default 8MB stack) with no stack-size guard. Deeply nested Rust input overflows the stack and the process aborts with SIGABRT (exit 134). The only thread::spawn in the codebase is test-only (project.rs:970), so production extraction has no larger-stack mitigation.

**Why it matters:** status and lint re-anchor by scanning file contents across the whole repo, and write-decision reads+extracts the target file. A single pathological file (generated/minified artifact, or an adversarial commit) crashes the entire command rather than degrading gracefully. This contradicts the engine's otherwise-careful graceful-degradation design (diagnostics + complete=false). It is an unhandled crash with whole-command blast radius, not a recoverable error.

**Impact:** Whole-command denial of service: `archiva status`, `archiva write-decision`, and any MCP tool path that triggers extraction abort with a core dump. In a repo with one such file, decision-health reporting is fully unavailable.

**Likelihood:** Low for honest hand-written code (15k+ nesting depth required); higher for repos containing generated, bundled, or adversarial files that status/lint will still scan.

**Evidence (reporter):** Real binary, scratch repo /tmp/archtest with a decision referencing a deep file: `python3 -c "print('mod m {' * 20000 + '}' * 20000)" > deep_real.rs` then `archiva status` -> `thread 'main' has overflowed its stack / fatal runtime error: stack overflow, aborting` exit 134. Also reproduced via `archiva write-decision` (exit 134) and directly via extract_anchors. Crash threshold observed between depth 10000 (OK, 376ms) and 15000 (crash). Nested function-local blocks (`fn f(){ {{{...}}} }`) crash the same way at depth ~40000.

**Independent verification:** Code: src/core/anchor.rs:733 collect_rust_item_anchors recurses unconditionally at lines 857-869 (the "{" arm, guarded only by rust_block_can_contain_local_items at 1733) and again at structural-anchor handling 1010-1024 (mod body via collect_rust_structural_anchor_at, anchor.rs:946). No depth/recursion limit exists anywhere in anchor.rs (grep for depth/MAX_DEPTH/limit returns only token_limit, which bounds token count, not recursion). Extraction runs on the main thread; the only thread::spawn in src/ is project.rs:970, which sits inside `#[cfg(test)] mod tests` (module opens at project.rs:687), so production extraction has no larger-stack guard. Confirmed via grep -rn "thread::spawn|stack_size|thread::Builder" src/ (single hit at project.rs:970). Default main-thread stack ~8MB (ulimit -s = 8192).

Reproductions in /tmp/archtest (git init + `archiva init`, src/deep_real.rs):
- `python3 -c "print('mod m {'*20000 + '}'*20000)"` then `archiva status` -> "thread 'main' has overflowed its stack / fatal runtime error: stack overflow, aborting", core dumped, exit 134. Note: status crashed with NO decision recorded — it scans project source directly.
- `archiva write-decision --json '{"file":"src/deep_real.rs","anchor":"mod:m","lines":[1,2],"chose":"x","because":"y","rejected":[]}'` -> same crash, exit 134.
- Crash threshold for mod nesting: depth 5000 exit 0, 10000 exit 0, 15000 exit 134 (matches claimed 10000-OK / 15000-crash boundary).
- Function-local block variant `fn f(){ {{...}} }` at depth 40000 -> exit 134 (matches claimed ~40000).
- MCP path: piped initialize + tools/call write_decision over stdio -> server process aborts mid-session ("stack overflow, aborting", core dumped). This kills the entire long-lived MCP server, not just one command.

**Verifier notes / severity correction:** Every element of the claim verified exactly: cited file:line correct, recursion sites correct, test-only spawn correct, exit 134 SIGABRT correct, depth thresholds match (mod ~10k-15k, fn-local ~40k), and all three surfaces (status, write-decision, MCP) reproduced. status crashes even with zero recorded decisions because it scans project source for extraction — so a single malicious .rs file makes status/lint health reporting permanently unavailable in that repo with no recorded decision required. The MCP impact is the most severe facet: one crafted tool call aborts the persistent server process (DoS of the whole agent session), which the claim notes but is worth emphasizing. Severity high is appropriate (not critical): it is an availability/DoS issue requiring an attacker-influenced source file in the repo, with no memory-safety/data-corruption or RCE consequence — Rust aborts cleanly on guard-page hit. Recommended fix: add an explicit recursion-depth cap in collect_rust_item_anchors (and the C/C++/TS equivalents) that stops descending and emits a diagnostic past a sane limit (e.g. a few hundred), OR run extraction on a thread::Builder with a bounded, generous stack and treat overflow defensively. A depth cap is preferable since it is deterministic and avoids relying on stack size.

**Recommended resolution:** Add an explicit recursion-depth cap in collect_rust_item_anchors (and the C-family/TS recursive scanners): on exceeding a sane limit e.g. 256, stop recursing, push a diagnostic, and set complete=false instead of recursing. Alternatively run extraction on a worker thread with a bounded stack and treat overflow as complete=false. Either path converts the crash into the existing graceful-degradation contract.

---

### F16. [MEDIUM] O(n^2) extraction blowup on flat, declaration-dense files

`defect` · location `src/core/anchor.rs:6678 (is_top_level), called at 2238/2320/3025; src/core/anchor.rs:1729 (is_rust_direct_scope_member via rust_depths_before:1760), called at 748; collect_if_block_candidates:5443 (complexity_between rescans nested region)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Several scan loops perform an O(index) re-scan from the start of the token stream for every candidate token. is_top_level(tokens, index) folds over tokens[..index] for every top-level function/class/const; is_rust_direct_scope_member calls rust_depths_before(tokens, start, index) for every token in a scope; collect_if_block_candidates calls complexity_between over the whole nested region per `if`. With N flat top-level declarations this is O(N^2).

**Why it matters:** Real nested source is fast (anchor.rs at 12,341 lines extracts in 86ms) because advancing past bodies keeps the top-level item count low, which likely masked this in testing. But generated/flat files (barrel re-export files, API clients, generated bindings, large enums of consts) are declaration-dense at the top level and hit the quadratic directly. status/lint scan every tracked source file, so cost compounds across the repo.

**Impact:** A plausible 3,000-line generated barrel of arrow exports takes 548ms for a single file; 20,000 flat functions take 5.5s (TS) / 11s (Rust). Repo-wide status/lint over several such files becomes multi-second to multi-minute.

**Likelihood:** Medium; generated/flat files are common in JS/TS projects and the tool is meant to run on whole repos (and on every hook invocation).

**Evidence (reporter):** Built harness over extract_anchors. TS many-funcs: n=5000 326ms, 10000 1.46s, 20000 5.49s (4x per doubling = quadratic). Rust many-funcs: 5000 623ms, 10000 2.69s, 20000 11.1s. Nested ifs: 1000 11ms, 2000 41ms, 4000 173ms, 8000 743ms. Flat 8000 ifs (advances past bodies): 24ms (linear). Realistic 3000-line barrel: 548ms.

**Independent verification:** CODE (all three quadratic patterns confirmed by reading src/core/anchor.rs):

1. is_top_level (anchor.rs:6678-6698) folds over tokens[..index] — O(index) — and additionally re-folds tokens[..export_index] in its export branch. Called inside per-token loops `for index in 0..tokens.len()` at 2238 (collect_function_anchors), 2320 (collect_class_anchors), 3025/3075/3110/3212 (variable anchors), plus ~15 more export/type/enum/namespace loops. N top-level decls => N calls each O(N) => O(N^2).

2. is_rust_direct_scope_member (1729-1731) -> rust_depths_before (1760-1775) folds tokens[start..index]. Called at 748 and 753 inside the `while index < end` scan in collect_rust_item_anchors (733). For a flat scope it advances index by 1 per non-member token, so each position costs O(index) => O(N^2).

3. collect_if_block_candidates (5414-5453) loops every `if` token and calls complexity_between(tokens, index, end_index, ...) (6459-6487) which linearly scans index..=end_index. For NESTED ifs each region spans the rest of the bodies => O(N^2); for FLAT/sequential ifs the regions are small => linear (matches claim).

RUNTIME (built `cargo run --release --example` harness calling archiva::core::anchor::extract_anchors directly; harness deleted afterward):
- TS flat funcs: n=5000 485ms, 10000 1.92s, 20000 7.89s (~4x per doubling = quadratic)
- Rust flat funcs: n=5000 1.28s, 10000 4.74s, 20000 18.86s (~4x per doubling = quadratic; even worse constant than TS, matching claim)
- Nested ifs: 1000 14.6ms, 2000 52ms, 4000 224ms, 8000 937ms (~4x per doubling = quadratic)
- Flat ifs 8000: 89ms (linear — confirms claim's distinction)
- Realistic 3000-line arrow barrel: 1.50s
- TS const-arrow exports: 5000 2.16s, 10000 8.72s, 20000 36.4s (quadratic)

END-TO-END CLI (scratch project /tmp/archbench, `archiva init`, wrote a decision against a 20k-function big.ts so it is scanned): `archiva lint` = 15.54s, `archiva status` = 15.13s. Adding a 2nd (2000-fn) and 3rd (3000-line) decision-tracked file: combined `status` = 17.44s. Confirms the per-file extract_anchors call at project.rs:464 (lint) and 235/464 (status) makes repo-wide commands multi-second on pathological files. `archiva why big.ts fn:f0` was ~0ms because the `why` path resolves the single named anchor without the full collect_* sweep, so the latency is specific to status/lint, not all commands.

**Verifier notes / severity correction:** CONFIRMED as stated; severity medium is correct. All three cited mechanisms are real and independently reproduced; the numbers in the original report match my measurements closely (my Rust 20k run was 18.9s vs their 11s — same quadratic shape, machine-dependent constant). Scope corrections/refinements: (1) Real-world blast radius is bounded — extract_anchors only runs for files that have a recorded decision (a .dlog under .decisions/), not every source file in the repo (lint_dlog at project.rs:459-467, status path at 463-464). A repo only pays this cost for tracked files, and only if those tracked files are pathologically large/declaration-dense. This keeps it medium, not high. (2) The `why <file> <anchor>` command does NOT exhibit the blowup (single-anchor resolution path), so the impact is specific to status and lint, which scan all anchors of each tracked file. (3) The const-arrow / barrel path is also affected (40000 anchors at 36s for 20k exports) via is_top_level in the variable-anchor loops, slightly broadening the claim beyond just `function`/`class`/`if`. Recommended resolution: precompute a prefix-sum of brace depth (and Rust paren/bracket depth) once per token stream so is_top_level / is_rust_direct_scope_member / scope-membership become O(1) lookups, and have collect_if_block_candidates skip past already-counted nested regions (or memoize complexity). This restores O(N) without changing output. A simpler mitigation is a token-count guard that emits an incomplete-extraction diagnostic above a threshold, but the prefix-sum fix is preferable since it preserves correctness on large generated files.

**Recommended resolution:** Track depth/scope incrementally during the single forward pass instead of recomputing prefix depth per token: maintain a running brace-depth counter for is_top_level, pass the current depth into the Rust scope walker rather than recomputing rust_depths_before from scope start, and precompute per-if complexity via a single nested pass. This makes extraction linear in token count.

---

### F17. [MEDIUM] C++ `enum class` / `enum struct` emits a phantom duplicate anchor that corrupts adjacent real types

`defect` · location `src/core/anchor.rs:487-507 (collect_c_family_type_anchors)` · reporter-confidence high · verification **CONFIRMED**

**Description:** When the loop hits `enum`, it correctly skips a following `class`/`struct` keyword to locate the name (lines 491-498) and emits enum:Name. But the very next loop iteration lands on that same `class`/`struct` token and treats it as a fresh type declaration, emitting a second phantom anchor (class:Name / struct:Name) at the same line. The only guard (lines 502-507) suppresses a `.`-prefixed keyword, not an `enum`-prefixed one.

**Why it matters:** It pollutes the anchor namespace with an anchor for a class/struct that does not exist, and because the phantom claims the base name first, a genuine adjacent `class Name` is pushed to the collision suffix `class:Name#2`. A user anchoring a decision to the real class via the natural `class:Name` silently binds to the phantom (wrong kind, possibly wrong line), and the decision's drift detection then tracks the wrong region.

**Impact:** Spurious anchors are recordable (verified) and the real type's anchor identity is corrupted; decisions can attach to a non-existent construct at an incorrect location.

**Likelihood:** Medium; `enum class` is idiomatic modern C++ and appears in essentially every C++11+ codebase.

**Evidence (reporter):** Harness + real binary. `printf 'enum class Color { Red, Green };' | dump_anchors x.cpp` -> both `enum:Color 1-1` AND `class:Color 1-1`. `enum struct Status{...}` -> `enum:Status` + `struct:Status`. In /tmp/archtest, `write-decision` against anchor `class:Color` on an enum-class-only file succeeds (Recorded dec_001). With `enum class Color{Red};` followed by real `class Color{int x;};` the available anchors are `class:Color, class:Color#2, enum:Color` (real class demoted to #2).

**Independent verification:** Code read at src/core/anchor.rs:481-547 (collect_c_family_type_anchors). The match arm for "enum" (lines 491-499) correctly advances name_index past a following "class"/"struct" to emit enum:Name. But the loop is `for index in 0..tokens.len()` over EVERY token, so the very next iteration lands on that same "class"/"struct" token and re-enters the match (lines 489-490), treating it as a fresh type declaration. The only suppression guard (lines 502-507) checks whether the PRIOR token is "." — it does not check for an "enum" prefix. With `enum class Color`, when index points at "class", the prior token is "enum" (text != "."), so the guard does not fire and a second phantom `class:Color` anchor is emitted at the same line via builder.add (lines 532-538).

Runtime reproduction with /home/ubuntu/archaeo/target/release/archiva in /tmp/archtest (archiva init'd):
- `enum class Color { Red, Green };` -> write-decision against struct:Color reports: "Available anchors in x.cpp: class:Color, enum:Color." Recording against both enum:Color (Recorded dec_001) and the phantom class:Color (Recorded dec_002) SUCCEEDS — both are real recordable anchors.
- `enum struct Status { Ok, Err };` -> Available anchors: enum:Status, struct:Status (phantom struct:Status recordable; class:Status correctly rejected). Confirms the enum-struct variant.
- Collision case `enum class Color { Red };\nclass Color { int x; };` -> Available anchors: "class:Color, class:Color#2, enum:Color". The phantom from the enum (token index lower, line 1) is added first and claims the canonical name class:Color; the REAL class declaration (line 2) collides and is demoted to class:Color#2. A decision recorded against the natural identifier class:Color therefore binds to the enum's location, not the real class. dmap confirms both class:Color entries coexist.

**Verifier notes / severity correction:** Claim is accurate in defect, location, mechanism, and impact. No correction to severity or scope needed. medium is appropriate: enum class/enum struct is an extremely common modern C++ idiom (scoped enums), so this is not an edge case; every such declaration emits a phantom class:/struct: anchor and, when a real same-named class/struct also exists in the file, corrupts the real type's anchor identity (demoted to #2) so decisions silently attach to the wrong construct/location. It is capped below high only because C/C++ is the native-only (non-primary) language target and the failure is incorrect-anchor rather than crash/data-loss. Root cause: the loop has no guard preventing the class/struct keyword consumed by a preceding `enum` from being re-processed as its own type declaration. Recommended fix: in collect_c_family_type_anchors, when the "enum" arm consumes a following "class"/"struct" (name_index = index+2), skip that consumed keyword token on the next iteration — e.g. track the consumed index and `continue` when the current "class"/"struct" token's immediately-preceding token is "enum", mirroring the existing "." guard at lines 502-507.

**Recommended resolution:** When the keyword is `class`/`struct` and the immediately preceding non-trivial token is `enum`, `continue` (the enum branch already handled it). Add tests for `enum class` and `enum struct`, including the adjacent-real-type collision case.

---

### F18. [LOW] C++ destructors are anchored identically to the constructor (fn:Class.Class)

`defect` · location `src/core/anchor.rs:637-667 (c_family_qualified_name_before) and c_family_anchor_name:669` · reporter-confidence high · verification **CONFIRMED**

**Description:** c_family_qualified_name_before only skips `*`, `&`, `&&` before the function name; the destructor tilde `~` is a separate token that is ignored. A destructor `~A()` therefore produces the name `A`, identical to the constructor `A()`. A class with both yields fn:A.A and fn:A.A#2 with no way to distinguish constructor from destructor (order-dependent).

**Why it matters:** Anchors are the stable identity a decision binds to. Two semantically distinct members collapsing to the same base name makes the #N suffix the only differentiator, which is fragile under edits/reordering and confusing to a human or agent recording a decision.

**Impact:** Ambiguous/duplicate anchors for any C++ class defining both a constructor and destructor; a decision may attach to the wrong member.

**Likelihood:** Low-medium; constructor+destructor pairs are common in RAII C++ but the resulting ambiguity is a usability/correctness nuisance rather than a crash or data loss.

**Evidence (reporter):** Harness. `class A { void normal(){} ~A(){ cleanup(); } };` -> `fn:A.normal 2-2`, `fn:A.A 3-3` (the line-3 anchor is the destructor, named A.A). With a real constructor too, both collapse to fn:A.A / fn:A.A#2.

**Independent verification:** Code (read): src/core/anchor.rs.
- Tokenizer (tokenize_with_diagnostics, lines 7072-7095) emits `~` as a standalone single-char punctuation token: it is not an identifier start char (is_identifier_start_char @7228 allows only `_`,`# Archiva v2 — Release-Readiness Audit

**Auditor role:** Independent Principal Software Architect / Release Auditor
**Subject:** Archiva v2 — std-only Rust re-engineering of a TypeScript "decision memory for AI coding agents" tool
**Date:** 2026-07-01
**Branch audited:** `codex/archiva-v2-rust-validation` (HEAD `33f160e`, version 0.2.0)

**Method:** 115 independent agents across 17 subsystem/dimension reviews; every substantive finding adversarially re-verified by a second agent that reproduced it against the compiled release binary or read the cited code. Documentation and the team's own `docs/archiva-v2-review-status.md` were treated as **unverified claims**, not evidence.

**Independently verified baseline (by the auditor before fan-out):**

- `cargo build --release` — clean.
- `cargo clippy --all-targets --locked -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `cargo test` — 301 lib tests pass (1 ignored) + 9 + 1 + 3 integration tests pass.
- Binary functional: `archiva --version` → `0.2.0`; `archiva status` on the repo → 537 decisions, 0 stale, 0 orphan, 0 issues across 56 `.dlog` files.

**Verification outcome across the panel:** 96 findings `CONFIRMED`, 1 `PLAUSIBLE`, **0 `REFUTED`**. Severity distribution (corrected, non-refuted): **17 high / 27 medium / 44 low / 9 info**, plus 10 audit-coverage gaps from a completeness critic.

---

## 1. Executive Summary

Archiva v2 is a genuinely impressive piece of engineering: a zero-dependency, std-only Rust implementation of a multi-language anchor extractor, a from-scratch git object reader (SHA-1 **and** SHA-256, packs, deltas, alternates), hand-written JSON/YAML parsers, a CLI, and a stdio MCP server — all clean-compiling, clippy-strict, well-formatted, and backed by 300+ tests and an elaborate differential/stress/scale/corpus validation harness. The code quality at the unit level is high and the discipline is real.

**But it is not ready for a stable 1.0 public release, and it is not yet the reference implementation.** The audit confirmed a coherent cluster of release-blocking problems that the existing test strategy structurally cannot see:

1. **A class of trivially-reachable process aborts (panics / stack overflows) triggered by ordinary committed, team-shared data.** Three distinct crashes were reproduced (a lone `'` in a `.dlog` → `yaml.rs:700`; a mid-codepoint UTF-8 slice → `yaml.rs:311`; deeply-nested source → unbounded recursion in the anchor extractor), and the completeness critic found a **fourth** (empty block scalar) in minutes. Because `.decisions/` is git-tracked by default and shared across a team, a single malformed byte in one file aborts `status`, `lint`, the per-session `session-start` hook, and — most seriously — **kills the long-lived MCP server mid-session**, dropping all in-flight agent context. This is a class, not a list.

2. **The product's headline automatic workflow is broken end-to-end.** The auto-wired `PostToolUse` re-anchor hook is a confirmed no-op under real Claude Code: `init` wires it with no argument relying on `ARCHIVA_FILE`, but Claude Code delivers the edited path as JSON on stdin, which `post-tool-use` never reads. It errors on every edit and silently never re-anchors. Compounding this, even when invoked correctly, re-anchoring is **non-idempotent** and **falsely marks correct decisions STALE** whenever there is no committed HEAD baseline (new files, or multiple edits between commits) — and the corruption compounds and does not self-heal.

3. **Performance cliffs contradict the "scales to large repos" claim, and the scale harness is blind to both of them.** Anchor extraction is O(n²) per file (a 1.4 MB file ≈ 55–66 s for one file); the hot-file write path is O(n²) cumulative (1,200 decisions in one file ≈ 86 s). The scale-smoke harness uses tiny one-function files and skips any file > 256 KiB, so neither bottleneck is ever exercised.

4. **Operational diagnosability is essentially zero** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery (dmap repair, stale-lock takeover) — for a tool that runs unattended as an agent hook.

None of the high-severity findings are memory-unsafety or RCE — Rust aborts cleanly — so the worst case is availability/DoS and silent metadata corruption, not exploitation. The defects are concentrated, well-understood, and individually fixable without architectural change. With a focused 4–8 week remediation pass (panic-safety hardening, hook stdin contract, idempotent re-anchoring, the O(n²) fixes, and a logging channel), this can become an excellent 1.0.

**Verdict: Do not ship as-is. Strong foundation; specific, fixable blockers.**

---

## 2. System Overview

Archiva stores *why code exists* beside the code. Per source file it maintains `.decisions/<path>.dlog` (authoritative YAML, schema:1) and `.decisions/<path>.dmap` (compact derivative index). Decisions are anchored to AST identities (`fn:foo`, `struct:Bar`, `block:if_x`) rather than line numbers, carry a fingerprint for drift detection, and form supersession chains. The same core operations are reachable three ways — CLI (`init`/`why`/`history`/`lint`/`status`/`hooks`/`write-decision`/`mcp`), a stdio JSON-RPC MCP server (`why`/`write_decision`/`ghost_check`), and Claude Code hooks (`session-start`/`post-tool-use`). Distribution is via an npm wrapper that selects a platform-specific native binary; the runtime is a single native binary with **no dependencies** (`Cargo.toml` `[dependencies]` is empty).

The architecture is sound and the product thesis is coherent. The problems are in robustness, the hook integration contract, performance at scale, and observability — not in the concept or the module decomposition.

**Module sizes (Rust, src/):** `anchor.rs` 12,341 (incl. ~5,090 test lines) · `git.rs` 4,329 · `project.rs` 2,261 · `fs.rs` 1,487 · `yaml.rs` 1,465 · `mcp.rs` 1,174 · `cli.rs` 1,030 · `decision.rs` 963 · `storage.rs` 959 · `json.rs` 722 · `diff.rs` 657 · `property_tests.rs` 540 · `dlog.rs` 507 · `paths.rs` 487. Total ~31,464 lines.

---

## 3. Architectural Assessment — **8/10**

Module boundaries are clean and cohesive: `cli`/`mcp` entrypoints → `core::project` orchestration → typed core modules (`decision`, `storage`, `dlog`/`dmap`, `anchor`, `git`, `paths`, `fs`). Coupling is low and the data-flow ownership (`.dlog` authoritative, `.dmap` rebuildable, request-scoped git reader) is well-reasoned.

The central architectural tension is the **zero-dependency, reimplement-everything-by-hand** stance: ~2.6k lines of git plumbing including a from-scratch DEFLATE inflater (`git.rs`), hand-written JSON/YAML parsers, and a multi-thousand-line multi-language anchor tokenizer. This is defensible *as a product tradeoff* (trivial supply chain, tiny binary, no transitive CVEs) but it concentrates the entire bug surface in hand-rolled parsers that have **not been fuzzed** — and that is precisely where every confirmed panic lives. The std-only purity is the root cause of the dominant risk class.

Two concrete architectural weaknesses (both verified):

- **No schema migration story.** `DLOG_SCHEMA_VERSION` is a hardcoded `1` and the parser hard-rejects anything else; a single forward-version file aborts every whole-repo command. There is no migrate-on-read and no skip-with-warning.
- **The ground-truth anchor range is computed and then discarded** in the normal re-anchor path (`project.rs:290-304`) — the parser already knows the anchor's exact current position, but the code trusts a fragile HEAD-diff shift instead. This is the root cause of the idempotency/STALE corruption.

Top simplification opportunity: prefer the extractor's live anchor position over diff-shifting; this single change fixes two high-severity findings at once.

---

## 4. Workflow Assessment — **5/10**

The intended loop is *read map → ask why → edit → write decision → lint drift*. Traced end-to-end:

- **`init` → first decision → `why`**: works cleanly; idempotent on re-run.
- **`write-decision`**: works, but is **non-atomic across `.dlog`/`.dmap`** (a torn write durably commits the decision while reporting failure, exit 1) and the natural retry is **non-idempotent** (overwrites the just-committed record with a new id, losing the original reasoning with no history entry).
- **Auto re-anchor on edit (the core promise)**: **broken under real Claude Code** (hook ignores the stdin payload) and **corrupts line attribution** even when invoked correctly (false STALE + compounding line drift with no committed baseline).
- **Re-deciding an anchor**: silently destroys the prior decision and its entire history unless `supersedes:<id>` is passed; superseding *into* an anchor occupied by a different live decision silently deletes that unrelated decision.

The "happy path" demos work; the realistic agent workflow (uncommitted files, multiple edits per commit, re-decisions) has multiple silent-data-integrity failures.

---

## 5. Feature Completeness Assessment — **7/10**

The advertised command set is fully present and the CLI surface is consistent and complete. Gaps are in fidelity rather than coverage: MCP `why` cannot do line-based lookup (the `line` field is silently dropped and it returns a *confidently wrong* whole-file result rather than "not found"); no `--fix` audit trail; no migration tooling. Feature breadth is appropriate and intentionally narrow (the README's positioning vs. broad memory tools is honest and well-argued).

---

## 6. Behavioral Consistency Assessment — **6/10**

CLI ↔ MCP ↔ hooks largely agree on validation and path normalization (verified: `.//src/a.ts` and `src\a.ts` normalize to one identity across entrypoints). Confirmed divergences:

- **MCP `why` ignores `line`** and returns the wrong decision; CLI `why <file> <line>` is correct.
- **MCP tool errors are returned as JSON-RPC protocol errors (`-32000`)** instead of the MCP convention `result.isError:true` — a spec inconsistency for tool *execution* failures.
- **`lint` exit code conflates** "found issues" with "command failed"; `status` returns 0 even with outstanding issues. No exit-code taxonomy.

---

## 7. Data Model Assessment — **7/10**

The decision record (chose/because/rejected/anchor/fingerprint/lines_hint/history/supersession) is well-designed and matches the spec and the YAML schema. Two material risks: **unknown/forward-compat fields are silently dropped on every rewrite** (data loss for any external or future-version annotation), and the **no-supersede overwrite** path discards history. The model is good; its *evolution and preservation guarantees* are weak.

---

## 8. Storage & Persistence Assessment — **7/10**

Individual writes are atomic (temp + rename + fsync; Unix parent-dir fsync). Verified strengths: corrupt `.dmap` self-heals from `.dlog`; stale-lock recovery works. Verified weaknesses:

- **No cross-file atomicity** between `.dlog` and `.dmap` (torn-write false-failure).
- **PID-liveness lock veto can wedge all writes indefinitely** (PID reuse defeats the staleness check; no force-unlock).
- **Read-only `status`/`lint` acquire the per-file write lock** and fail on a read-only `.decisions/`.
- Parent-dir durability fsync is a **no-op on Windows** (crash-consistency claim is weaker there and untested in crash-injection form).

---

## 9. CLI Assessment — **8/10**

The strongest subsystem. Dispatch, help text, exit-code routing (0/1), `--` escaping, and unknown-flag/command handling are correct and extensively tested. Rough edges: `write-decision` **reads stdin before validating its own args** (a malformed call with an open stdin hangs), the `lint` exit-code conflation, inconsistent `error:`-prefixing between argument vs. semantic errors, and `why` accepting line `0`.

---

## 10. Protocol & Integration Assessment — **6/10**

JSON-RPC 2.0 framing, id handling, `notifications/*` swallowing, and method dispatch are correct and tested. **The integration contract with Claude Code is broken** (PostToolUse stdin payload ignored — the single most important integration defect). MCP tool-error encoding deviates from the MCP convention. And the long-lived server has **no panic isolation** — one crafted `tools/call` aborts the whole process.

---

## 11. Performance Assessment — **6/10**

Measured, not assumed:

- **Anchor extraction O(n²) per file** (`is_top_level`/`rust_depths_before` re-scan the token prefix per declaration): TS 10k/20k/40k lines = 0.5/2.3/9.9 s; a 1.4 MB file ≈ 55–66 s; a single huge Rust fn ≈ 47 s vs ~3 s for the TS equivalent. Memory stays modest (~70 MB) — purely CPU.
- **Hot-file writes O(n²) cumulative** (full render + redundant re-parse + full rewrite + fsync per write): #1,200 in one file ≈ 86 s cumulative; 1,500 timed out at 120 s. The `storage.rs:133` self re-parse of freshly-rendered YAML is unconditional wasted work.
- **`status` parses every `.dlog` twice** per invocation and sweeps the whole source tree.
- The `.dmap` index **is never read by any command** — every read re-parses the verbose `.dlog`, so the derivative index pays maintenance cost for zero read benefit.

Startup is excellent (~0.7 ms). For small/medium repos performance is fine; the cliffs are real and reachable.

---

## 12. Scalability Assessment — **5/10**

The "scales to 100k files / 1M decisions / Linux-kernel-and-LLVM corpora" claim is **not substantiated by the harness**, which uses tiny one-function files, skips files > 256 KiB, and defaults to 1 decision/file — exactly avoiding both quadratic paths. The blast radius is also wider than the per-command framing: because whole-repo commands re-extract anchors from *every* source file (decided or not), one large generated/minified/vendored file degrades `lint`/`status` repo-wide. Linear-in-file-count scanning is otherwise reasonable.

---

## 13. Reliability Assessment — **5/10**

Dominated by the panic class: `status`, `lint`, `session-start`, and the MCP server all abort (SIGABRT / exit 101 or 134) on malformed-but-committed input, and **a single corrupt `.dlog` aborts the entire repo-wide command** rather than being skipped and reported — taking down visibility for all healthy files. Recovery paths that exist (dmap repair, lock recovery) are correct but silent. Single-file commands are correctly scoped and limit some blast radius.

---

## 14. Failure Recovery Assessment — **6/10**

Good: atomic single-file writes, self-healing dmap, stale-lock recovery. Weak: no cross-file transaction; `lint --fix` is non-atomic across files and leaves a partially-fixed repo with no record of what changed on mid-run failure; no panic boundary so corruption aborts rather than degrades; no migration/forward-compat recovery. The recovery primitives are sound but not composed into command-level resilience.

---

## 15. Security Assessment — **6/10**

Threat model is correct (local CLI; parses untrusted repo/agent-supplied `.dlog`/source; no authn expected). No RCE, no memory unsafety, no command execution, no network. Confirmed issues:

- **DoS via panics** on committed/shared input (the dominant issue) — including killing the shared MCP server.
- **Write-side symlink escape**: the read path canonicalizes and rejects escapes, but the *write* path does not — a checked-in symlink under `.decisions/` writes `.dlog`/`.dmap` outside the repo and can clobber a same-named `.dmap` in the target. A clean bypass of the project's own advertised symlink control (asymmetric: `.dlog` clobber is gated by the parse step, `.dmap` is not).

Read-path path-validation hardening is genuinely strong (traversal, drive/UNC/device, reserved Windows names, trailing dot/space all rejected; verified). The git zlib path bounds delta depth (though the cap of 32 is *below* git's default 50 — a correctness bug, not a security one).

---

## 16. Testing Assessment — **6/10**

Volume is high and the differential-against-TS strategy is the right idea. But the confidence is narrower than the count implies:

- **The differential oracle covers only TS/JS.** The Rust and C/C++ extractors — the largest *novel* code the v2 effort added — have **no independent oracle**; correctness rests on same-team unit tests. A wrong range in a `.rs` file is silently accepted (verified: a decision with a deliberately wrong line range passes `lint`).
- **No fuzzing of the hand-written parsers** — the exact gap that let four panics survive into a release candidate.
- **No concurrency/crash-injection tests** — atomicity and locking verdicts rest on code reading.
- **The scale harness exercises a best case** (tiny files, 256 KiB skip, 1 decision/file).
- **The strongest suites (property soak, full scale, corpus matrix) run only weekly/on-dispatch, not on PRs.**
- **Cross-platform (Windows/macOS/arm/musl) is CI-only and unverified here**; the Windows crash-consistency path is a different (weaker) code path that is not crash-tested.

---

## 17. Documentation Assessment — **8/10**

README, spec, and architecture docs are clear, honest, and unusually well-written (the competitive-positioning table is fair; `docs/archiva-v2-review-status.md` candidly lists remaining gaps). Two doc-vs-reality drifts matter: the quick-start's auto-wired hook doesn't work as documented, and the "scales to large repos" claim is not borne out. Otherwise documentation is a strength.

---

## 18. Developer Experience Assessment — **7/10**

Onboarding is smooth and the CLI is pleasant and consistent. DX is undercut by: the broken auto-hook surfacing a recurring error on every edit, the absence of any diagnostic/verbose mode, confusing error messages with no file path on corrupt-store scans (`file: missing required field` reads as a sentence, not a schema field), and the silent data-loss footguns (re-decide without supersede). The bones are good; the failure-mode UX needs work.

---

## 19. Operational Readiness Assessment — **4/10**

The single biggest operational gap is **observability: there is none** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery. For an unattended agent hook, any non-crash misbehavior currently requires recompiling with instrumentation to investigate. Combined with the panic class and the no-path corrupt-file errors, field diagnosability is poor.

---

## 20. Production Readiness Assessment — **5/10**

Not production-ready for general/public use today. It is usable in a controlled, single-user, small-repo, all-committed-files setting where the panic triggers and perf cliffs are unlikely. It is not ready for team use over a shared `.decisions/` tree (the panic propagation vector) or for large repos (the perf cliffs) or for the documented Claude Code auto-workflow (the hook contract).

---

## 21. Open Source Readiness Assessment — **7/10**

Strong: MIT license, clean repo, clippy-strict, formatted, CONTRIBUTING-grade docs, a real CI/validation/publish pipeline, and a thoughtful architecture doc with explicit extension points. Adoption-readiness is gated almost entirely by the production-readiness blockers above plus the missing panic-safety and fuzz gates that an open-source contributor base would expect before trusting it with their decision history. Packaging has one sharp edge: `import.meta.dirname` in the install tooling hard-requires Node ≥ 20.11 while npm doesn't enforce `engines`, so older-Node users get a cryptic postinstall crash.

---

## 22. Prioritized Findings

**Severity distribution (adversarially verified):** 0 critical *as labeled* / 17 high / 27 medium / 44 low / 9 info. 96 CONFIRMED, 1 PLAUSIBLE, **0 REFUTED**. The completeness critic argues — and I concur — that the *consolidated* "panic on committed/shared input" theme meets a **critical / release-blocking** bar even though no single line-item was labeled critical.

### Release blockers (must fix before 1.0)

| # | Finding | Location | Why it blocks |
|---|---|---|---|
| B1 | **Panic/abort class on committed input** (lone `'` → `yaml.rs:700`; mid-codepoint slice → `yaml.rs:310`; empty block scalar; deep-nesting recursion → `anchor.rs:733`) | `yaml.rs`, `anchor.rs` | One malformed byte in a shared `.dlog`/source aborts `status`/`lint`/`session-start` and **kills the MCP server**. It's a class — must be closed by fuzzing + depth bounds + a panic boundary, not point patches. |
| B2 | **PostToolUse hook is a no-op under Claude Code** (ignores stdin `file_path`) | `settings.rs:5`, `main.rs:46-51`, `cli.rs:234-258` | The core advertised automation never runs in the documented environment. |
| B3 | **Re-anchor falsely marks STALE + non-idempotent line drift** (no HEAD baseline / multiple edits per commit; ground-truth range discarded) | `project.rs:271-304`, `diff.rs:17-44` | Silent corruption of the authoritative store under normal agent activity; breaks line-based `why`. |
| B4 | **O(n²) anchor extraction** (per-file prefix re-scan) | `anchor.rs:6678`, `1729/1760` | Tens of seconds–minutes on a single large file; hooks read as hangs. |
| B5 | **One corrupt `.dlog` aborts the whole repo command** + **errors carry no file path** | `project.rs:76-82/402-408/133-149`, `error.rs:88-110` | Localized corruption blinds the whole repo; operators can't locate the bad file. |

### High (fix before or immediately after 1.0)

B6 non-atomic `.dlog`/`.dmap` write (false-failure + retry id-churn, `storage.rs:256-258`) · B7 write-side symlink escape (`paths.rs`/`storage.rs`) · B8 O(n²) hot-file writes + redundant re-parse (`storage.rs:131-133`) · B9 no logging/diagnostics anywhere · B10 Rust/C/C++ extractors have no differential oracle · B11 silent data loss on re-decide without `supersedes` · B12 MCP `why` returns confidently-wrong result for a `line` query.

### Medium (notable)

PID-liveness lock wedge · read-only `status`/`lint` take write locks · MCP tool errors as `-32000` not `isError` · `lint`/`status` exit-code taxonomy · unknown-field drop on rewrite · no schema migration · anchored-gitignore patterns never match descendants · C++ `enum class` phantom anchor · git delta-depth cap 32 < 50 · `.dmap` never read · scale harness blind to both quadratics · `lint --fix` non-atomic · Node-version postinstall crash · case-insensitive-FS identity collisions · heavy validation runs only weekly.

---

## 23. Recommended Roadmap

**Milestone 1 — Panic-safety & robustness (release-blocking, ~2 wks)**

1. Fuzz the YAML/JSON parsers (`cargo-fuzz` or in-repo property soak over arbitrary bytes); fix every panic; add a depth/recursion bound to the anchor extractor and block-scalar paths.
2. Wrap MCP per-request handling in `catch_unwind` → return `isError`; never let one request abort the server.
3. Make whole-repo commands skip-and-report corrupt files (continue, name the file) instead of aborting; attach the file path to all parse/schema/IO errors.
4. **Make "no panic on any `.dlog`/source input" and "MCP server survives a malformed request" hard PR-CI gates.**

**Milestone 2 — Core workflow correctness (release-blocking, ~1–2 wks)**

5. Parse the Claude Code hook stdin JSON (`tool_input.file_path`) in `post-tool-use`; add an integration test feeding the real payload shape.
6. Re-anchor from the extractor's ground-truth position whenever the anchor still resolves; reserve diff-shift for the orphan/incomplete case. Add a regression test asserting two consecutive `post-tool-use` runs leave `lines_hint`/STALE unchanged.
7. Detect re-decide-without-supersede and supersede-into-occupied-anchor; refuse or auto-chain into history.
8. Make `.dlog`+`.dmap` a recoverable transaction (treat a `.dmap` write failure as success-with-warning, since reads self-heal).

**Milestone 3 — Scale & observability (~1–2 wks)**

9. Eliminate the O(n²) extraction (single-pass depth tracking) and the O(n²) write path (drop the `storage.rs:133` re-parse in release; consider per-file decision bounds); add a large-single-file and a dense-single-file perf regression to the PR gate.
10. Add an env-gated stderr diagnostic channel (files scanned/skipped, lock acquire/recover, dmap repair, git-HEAD fallback).
11. Either make `status`/`session-start` actually read `.dmap`, or drop it.

**Milestone 4 — 1.0 hardening (~1–2 wks)**

12. Independent oracle for Rust/C/C++ extraction (cross-check ranges vs tree-sitter or rustc/clang spans) over a real corpus.
13. Define a schema versioning/migration policy; preserve unknown fields on rewrite.
14. Canonicalize the write path (close the symlink escape); add concurrency + crash-injection tests; promote heavy validation into the release gate; fix the lock-wedge backstop, exit-code taxonomy, gitignore anchoring, git delta-depth cap, and Node-version preflight.
15. Run the multi-process lock and differential suites on Windows/macOS CI (not just `cargo test`).

---

## Subsystem Scorecard

| Subsystem / Dimension | Score |
|---|---|
| CLI surface & dispatch | 8/10 |
| MCP stdio JSON-RPC server | 7/10 |
| Anchor extraction engine | 7/10 |
| Native Git object reader | 8/10 |
| Decision logic (validation/supersession/history) | 7/10 |
| Storage / locking / atomicity / recovery | 7/10 |
| Project workflow orchestration | 8/10 |
| Serialization (JSON/YAML/dlog/dmap) | 6/10 |
| Path validation & portability | 7/10 |
| Diff / reanchor / line-shifting | 7/10 |
| Security (cross-cutting) | 6/10 |
| Performance & scalability | 6/10 |
| Reliability / recovery / observability | 7/10 |
| Testing strategy & confidence | 7/10 |
| Release engineering & packaging | 8/10 |
| API/CLI/behavioral consistency & DX | 8/10 |
| Overall architecture & maintainability | 8/10 |

*(Per-subsystem scores reflect each module's intrinsic quality. The system-level scores below are lower because the dominant failures are cross-cutting — they emerge from interactions, e.g. parser-panic × shared-committed-store × long-lived-server.)*

### Overall scores

- **Engineering quality (unit level): 8/10** — clean, disciplined, well-tested-in-volume, idiomatic, zero-warning.
- **Production readiness: 5/10** — concentrated, reproducible blockers; safe only in a controlled single-user setting today.
- **Validation confidence: 6/10** — broad and partly differential, but blind to the exact failure classes that bite (no fuzz, no scale realism, no Rust/C oracle, no concurrency/crash injection, no real-hook integration); heavy suites are off the PR path.

---

## Final Questions — Answered Explicitly

**Does the implementation fully achieve its stated goals?**
No. It achieves the *static* goal (a fast, repo-native, zero-dependency decision store with a working CLI and MCP surface) but not the *dynamic* goal: the automatic agent workflow it is built around (auto re-anchor on edit) is broken end-to-end under real Claude Code and corrupts line attribution even when invoked correctly, and the "scales to large repos" claim is not substantiated.

**Is the architecture appropriate for long-term evolution?**
Mostly yes. Module boundaries, ownership, and extension points are sound. The two evolution gaps — no schema migration path and the zero-dependency stance concentrating un-fuzzed parser risk — are addressable without restructuring.

**Is the implementation internally consistent?**
Largely, with confirmed exceptions: CLI vs MCP `why` line semantics, MCP error encoding vs spec, and exit-code conventions. These are localized.

**Would you approve this for a stable public release?**
No. The panic class on committed/shared input, the broken auto-hook, the re-anchor corruption, and the absence of observability are release-blocking for a stable 1.0.

**Would you approve it as the reference implementation?**
Not yet. A reference implementation must be panic-safe against its own committed data format and must have the core workflow proven end-to-end. Once Milestones 1–2 land and panic-safety + real-hook integration are CI gates, it is a credible reference candidate.

**Highest-priority improvements before release?**
(1) Fuzz and bound the parsers + add a panic boundary + skip-and-report corrupt files; (2) fix the PostToolUse stdin contract; (3) make re-anchoring idempotent from ground-truth positions; (4) fix the O(n²) extraction/write paths; (5) add a diagnostic logging channel. In that order.

**What work remains before 1.0?**
Milestones 1–4 above. Realistically 4–8 focused weeks. The critical path is panic-safety + workflow correctness (M1–M2).

**What risks remain after release (assuming blockers fixed)?**
Cross-platform behavior (Windows/macOS/arm/musl) remains CI-only and the Windows crash-consistency path is weaker; the Rust/C/C++ extractors stay oracle-light until an independent ground truth exists; the hand-rolled git/DEFLATE code is a maintenance hotspot that needs an ongoing differential-against-`git` fuzz harness; and concurrency/crash-consistency guarantees rest on reasoning until fault-injection tests exist.

**How does engineering quality compare with mature, well-regarded OSS in the same domain?**
At the *craft* level (code cleanliness, lint discipline, documentation, the differential-testing instinct) it compares favorably with well-run early-stage OSS and exceeds many. At the *production-hardening* level it is behind mature tools like ripgrep, gitoxide/`gix`, or tree-sitter, which earned trust through extensive fuzzing, fault injection, and battle-tested robustness against adversarial input — exactly the layer Archiva has not yet built. It has the bones of a top-tier project and an unusually honest self-assessment; it has not yet done the hardening that separates a strong prototype from a definitive reference.

---
---

# APPENDIX — Full Verified Findings

Every finding below survived independent adversarial verification (reproduction against the release binary or direct code read). `verdict` is the second agent's call; `severity` is the corrected severity. Findings are grouped by subsystem/dimension, then sorted high → info.

,alphabetic), and the multi-char punctuation match at 7073-7089 has no `~` case, so it falls to the single-char catch-all @7090-7094.
- c_family_qualified_name_before @637-667: starting at param_open-1, the while loop @642-647 only skips `*`,`&`,`&&`. For `~A(...)` the token immediately before `(` is the identifier `A`, so name_index lands on `A`; the preceding `~` token is never consumed and never reached. parts = ["A"]. c_family_anchor_name @669 just returns the string unchanged. Result name = "A" for both constructor `A()` and destructor `~A()`.

Binary (/home/ubuntu/archaeo/target/release/archiva) in /tmp/cpptest:
- b.cpp = class A with `A(){...}` (line 3), `normal(){}` (line 4), `~A(){...}` (line 5). write-decision against a bogus anchor enumerated: "Available anchors in b.cpp: class:A, fn:A.A, fn:A.A#2, fn:A.normal." -> ctor and dtor collapse to fn:A.A and fn:A.A#2.
- `why b.cpp 3` -> fn:A.A; `why b.cpp 5` -> fn:A.A#2. So line-3 constructor = fn:A.A, line-5 destructor = fn:A.A#2.
- Destructor-only d.cpp (no constructor) -> "Available anchors ... fn:A.A" i.e. the lone destructor is named fn:A.A, identical to what a lone constructor would yield.
- Order-dependence confirmed: rev.cpp with destructor first (line 2) then constructor (line 3) -> anchors fn:A.A and fn:A.A#2 again, but now fn:A.A is the destructor. Reordering members swaps which member owns which anchor.
This exactly matches the claimed harness output (destructor named A.A; ctor+dtor collapse to fn:A.A / fn:A.A#2, order-dependent).

**Verifier notes / severity correction:** CONFIRMED as a real defect at the cited location with the cited mechanism; severity low is correct. Refinements: (1) The `~` is dropped because it is a separate punctuation token the qualified-name walker never inspects (only `*`/`&`/`&&` are skipped) — accurately described. (2) No data loss occurs: the AnchorBuilder `#2` disambiguation gives the two members distinct anchors, so decisions don't silently overwrite each other. The harm is semantic/operational: a constructor and destructor are name-indistinguishable (both base to A.A), and the `#1` vs `#2` assignment is purely source-order dependent. Adding, removing, or reordering a same-named member (ctor/dtor, or overloads) shifts the suffix, which will orphan or mis-attach previously recorded decisions on subsequent re-extraction. (3) Scope: only affects C/C++ files (native-only support) where a class defines a constructor and destructor (or multiple same-named members) AND decisions are recorded against them — a genuinely narrow case for an agent-decision-memory tool whose primary languages are TS/JS/Rust, justifying low. (4) Recommended resolution: have c_family_qualified_name_before detect a `~` token immediately preceding the destructor identifier and incorporate it into the anchor name (e.g. emit fn:A.~A or fn:A.dtor) so constructor and destructor are distinguishable and order-stable; the change is localized to anchor.rs around 637-667/669.

**Recommended resolution:** Detect a leading `~` token and incorporate it into the anchor name (e.g. fn:A.~A) so destructors are distinct from constructors. Add a C++ destructor test.

---

### F19. [LOW] Fingerprint normalization diverges from the TS oracle on U+FEFF (BOM), causing false drift across the TS->Rust migration

`defect` · location `src/core/fingerprint.rs:3-10 (normalize_code uses str::split_whitespace) vs src/core/fingerprint.ts:3-9 (uses JS /\s+/ and String.trim)` · reporter-confidence high · verification **CONFIRMED**

**Description:** normalize_code collapses whitespace using Rust's split_whitespace (Unicode White_Space property), while the TS implementation uses the JS regex `\s` and `String.prototype.trim`. These character classes differ: JS `\s` matches U+FEFF (BOM / zero-width no-break space) but Rust char::is_whitespace does not. Any code region whose normalized text contains U+FEFF therefore hashes differently between the two implementations.

**Why it matters:** The fingerprint is the drift/staleness signal stored in .dlog. Archiva v2 is explicitly a re-engineering of the TS tool; decisions written by the TS version (or v2-published fingerprints expected to match TS semantics) will be judged stale by the Rust version (and vice versa) for files containing a BOM, even when the code is byte-identical. A leading BOM is common in Windows-authored UTF-8 files.

**Impact:** False-positive staleness (or missed staleness) on BOM-containing files; undermines the core promise that fingerprints are stable and TS-compatible. Silent: shows up as a phantom STALE in status/lint.

**Likelihood:** Low-medium; mid-line FEFF is rare, but leading UTF-8 BOM is a realistic, common case.

**Evidence (reporter):** Compared normalize_code/fingerprint against the JS oracle using codepoint-built inputs. Leading BOM: Rust fingerprint('\u{FEFF}abc')=1c28dc3f but fingerprint('abc')=ba7816bf (BOM preserved), while JS fp(BOM+abc)=ba7816bf == fp('abc') (BOM trimmed by \s). Mid-line: Rust norm('a\u{FEFF}b')='a\u{FEFF}b' fp=47a12dcb vs JS norm='a b' fp=c8687a08. U+2028/2029/00A0/3000/000B agree (both treat as whitespace); U+200B agrees (neither does). Only U+FEFF diverges among tested codepoints.

**Independent verification:** Read src/core/fingerprint.rs:3-9 (normalize_code maps each line through line.split_whitespace().join(" ")) and src/core/fingerprint.ts:3-9 (line.trim().replace(/\s+/g," ")). Rust char classification differs from JS \s on U+FEFF.

Reproduced both sides:
JS oracle (node v24.8.0): norm(BOM+"abc")="abc"; fp(BOM+"abc")=ba7816bf == fp("abc")=ba7816bf. norm("a"+BOM+"b")="a b"; fp=c8687a08 == fp("a b")=c8687a08. BOM stripped by trim/\s+.

Rust (extracted exact split_js_lines+normalize_code into /tmp/fptest/t.rs, rustc -O): norm(BOM+"abc")="\u{feff}abc"; norm("a"+BOM+"b")="a\u{feff}b". '\u{FEFF}'.is_whitespace()=false, so split_whitespace preserves BOM.

Fingerprint hex via sha256-of-normalized: Rust fp(BOM+"abc")=1c28dc3f, fp("abc")=ba7816bf; Rust fp("a"+BOM+"b")=47a12dcb, JS fp("a b")=c8687a08. All four values match the claim exactly.

grep -rn for FEFF/BOM/strip/trim_start over src/*.rs returned nothing: no read-time BOM stripping neutralizes the divergence.

**Verifier notes / severity correction:** Claim is technically accurate: code locations, mechanism (Rust char::is_whitespace excludes U+FEFF while JS \s/trim include it), and all four reproduction hex values are correct. The only correction is severity calibration. The divergence produces false drift ONLY cross-implementation, i.e. exactly the TS->Rust migration scenario the claim scopes to (a fingerprint persisted by the old TS tool, then re-checked by Rust). In pure-Rust steady state the tool stores and recomputes with the same normalizer, so it is self-consistent and no phantom STALE appears. Combined with the precondition that a BOM must survive inside an anchored region (leading-BOM is the realistic vector; mid-line BOM is exotic), the practical blast radius is narrow and the symptom is a one-time migration recheck artifact rather than ongoing corruption. I'd rate this low (borderline medium), not medium. Recommended fix: strip a leading U+FEFF at file-read time and/or replace split_whitespace with a normalizer whose whitespace class matches JS \s (add U+FEFF), then add a TS-oracle parity test covering U+FEFF.

**Recommended resolution:** Make the Rust whitespace class match JS `\s` exactly for the characters that can appear in source: treat U+FEFF as whitespace during normalization (and audit the full JS `\s` set: it includes \t\n\v\f\r, space, U+00A0, U+1680, U+2000-200A, U+2028, U+2029, U+202F, U+205F, U+3000, U+FEFF). Add a cross-impl fingerprint test covering BOM. If TS-exact parity is a release requirement, this should block release.

---

### F20. [LOW] Hand-rolled 12k-line multi-language parser concentrates maintenance and extension risk

`architecture` · location `src/core/anchor.rs (entire module, 7,249 non-test lines, 100+ helper fns)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** A single file implements five language grammars via interdependent hand-written token scanners with no shared grammar abstraction. The TS path is validated against a real AST oracle (ts-morph) and is excellent; the Rust and C/C++ paths have no oracle and rely solely on in-file unit tests. The C-family path is the thinnest-tested (~3 test references) and is exactly where the correctness defects (enum class, destructors) were found.

**Why it matters:** Correctness of un-oracled languages depends entirely on hand-enumerated cases; gaps are invisible until a user hits them. The density of mutually-recursive helpers (rust_*, c_family_*, tsx_*) raises the cost and risk of every future change, and there is no fuzz/property harness wired into CI for the non-TS paths.

**Impact:** Elevated long-term maintenance cost and latent-defect risk in Rust/C++ extraction; the same class of bug (untested-branch) will likely recur as languages/constructs are added.

**Likelihood:** n/a (structural/operational risk).

**Evidence (reporter):** wc -l src/core/anchor.rs = 12341; test section starts line 7250 (5091 test lines, 57 #[test]). Differential testing against ts-morph found 0 TS divergences across 300 random + many hand cases, but C-family (no oracle) yielded two real bugs. grep for C-family test references in the test section: 3.

**Independent verification:** All structural claims verified against the actual source, and the cited C-family defects reproduce in the shipped binary.

LINE/STRUCTURE COUNTS (src/core/anchor.rs):
- `wc -l src/core/anchor.rs` = 12341 (matches claim exactly).
- `#[cfg(test)]` begins at line 7250 (matches). Non-test region = lines 1..7249 (7249 lines); test region = 7250..12341 = 5092 lines (claim said 5091; off-by-one, negligible).
- `grep -c "#[test]"` = 57 (matches).
- Helper fns in non-test region: `awk 'NR<7250' | grep -cE '^\s*(pub )?fn '` = 240, comfortably exceeding the claimed "100+".

NO SHARED GRAMMAR ABSTRACTION (verified): `grep -nE "trait (Grammar|Parser|Language|Lexer)|impl Grammar"` returns nothing. Dispatch is a hand-written if-chain in extract_anchors (anchor.rs:241-247): is_rust_file -> extract_rust_anchors (anchor.rs:409, own rust_tokenize_with_diagnostics), is_c_family_file -> extract_c_family_anchors (anchor.rs:455, reuses the TS tokenize_with_diagnostics), else the TS/TSX path. Three interdependent hand-written scanners, no shared grammar layer.

TS HAS AN AST ORACLE; RUST/C-FAMILY DO NOT (verified):
- src/core/anchor.ts:1 imports ts-morph (real AST: `import { Node, Project, SourceFile, SyntaxKind } from "ts-morph"`). tools/archiva-differential.ts exists with ~79 scenarios comparing the Rust binary's observable behavior.
- C-family has NO oracle: `grep -cE '\.cpp|\.cc|enum class' tools/archiva-differential.ts src/core/anchor.ts` = 0/0. anchor.ts handles only TS/JS (no .rs/.c branches).
- Property/fuzz testing excludes C-family entirely: src/core/property_tests.rs:185 `enum SourceKind { Ts, Tsx, Rust }` — no Cpp variant; the malformed-source fuzzer (line 142) iterates only Ts/Tsx/Rust at DEFAULT_CASES=128 each.

C-FAMILY IS THINNEST-TESTED (verified): exactly 2 dedicated `#[test]` fns — extracts_c_family_functions_types_methods_and_blocks (anchor.rs:12224) and extracts_cpp_inline_and_qualified_methods (anchor.rs:12270). Neither exercises `enum class` or destructors.

CITED DEFECTS REPRODUCE IN THE SHIPPED BINARY (/tmp/anchortest, git-init'd, archiva init): wrote src/widget.cpp containing `enum class Color { Red, Green };`, a `class Widget` with declared `~Widget();`, and an out-of-line `Widget::~Widget()` definition.
- enum class: `archiva write-decision --json '{"file":"src/widget.cpp","anchor":"enum:Color",...}'` -> "Recorded dec_001." but the available-anchor listing shows BOTH `class:Color` AND `enum:Color` for a single `enum class Color` — a spurious duplicate/wrong-kind anchor (worse than a plain miss).
- destructor: recording against `fn:Widget.~Widget` fails: "Anchor \"fn:Widget.~Widget\" does not exist ... Available anchors: class:Color, class:Widget, enum:Color, fn:Widget, fn:Widget.value." The destructor is not extracted, and the constructor declaration is mis-surfaced as `fn:Widget`. This confirms untested C-family branches ship live defects.

**Verifier notes / severity correction:** CONFIRMED as written; severity architecture/low is appropriate and kept. The claim is correctly scoped as a long-term maintenance/latent-defect-risk observation about the monolithic, abstraction-free, oracle-asymmetric design — not as a standalone functional bug (the enum-class/destructor bugs are separate findings, here used only as supporting evidence, and they do reproduce).

Minor corrections to the claim's figures: test region is 5092 lines, not 5091 (off-by-one); non-test helper-fn count is ~240 `fn` definitions, materially higher than the stated "100+"; C-family has 2 dedicated `#[test]` fns (the "~3 references" is in the right ballpark counting fixture/extension mentions). The "300 random ts-morph cases" figure I could not directly verify from the in-repo Rust tests (the in-tree fuzzer uses DEFAULT_CASES=128 per kind across Ts/Tsx/Rust); the differential harness is a CLI/MCP behavioral comparator with ~79 scenarios rather than a per-call AST diff, so treat the exact "300" as unverified though the qualitative TS-has-oracle / C-family-has-none asymmetry is firmly confirmed.

One sharpening worth recording: the enum-class behavior is not a benign miss but emits a redundant/incorrect anchor pair (class:Color + enum:Color), and the constructor declaration is surfaced as fn:Widget while the destructor is dropped — i.e. the untested C-family path produces actively wrong anchors, reinforcing the claim's thesis that the same untested-branch defect class will recur as constructs/languages are added. Recommended resolution stands: extract a shared tokenizer/grammar seam and add a C/C++ differential oracle (e.g. tree-sitter or libclang) plus C-family fuzz coverage in SourceKind before extending the C-family grammar further.

**Recommended resolution:** Add a differential/property test harness for the Rust path (e.g. compare against rustc/syn output offline, or property tests asserting anchor invariants: every anchor's [start,end] brackets a balanced token range, no duplicate base without #N, line ranges monotonic). Add focused C/C++ fixtures (enum class, destructors, operator overloads, templates). Consider splitting per-language extractors into submodules to bound change blast radius.

---

## Native Git object reader  — score 8/10

> src/core/git.rs is a 4329-line std-only reimplementation of the git plumbing needed to read `HEAD:path` without spawning git: loose + packed objects, v1/v2 pack indexes, OFS/REF deltas, alternates, linked-worktree common dirs, packed-refs, chained symbolic refs, plus from-scratch zlib/DEFLATE inflate, SHA-1, and a SHA-256 hook. I verified correctness empirically with a harness calling the public `read_git_head_file` against real `git show HEAD:path` across loose, packed, deltified, SHA-1, SHA-256, v1-index, deep-path, symlink, missing-file, unborn-HEAD, packed-refs, alternates, and linked-worktree cases — 200+ files, all byte-exact. Malformed inputs (truncated/garbage loose objects, corrupted pack bodies, bad checksums, decompression bombs) all produce clean typed errors with no panics; a 50MB compressible bomb is rejected from the pack size header at 2MB peak RSS. The implementation is unusually careful and well-tested (42 module tests pass), with strong checksum/layout validation and bounded recursion. The one concrete correctness defect I found is a delta-chain depth cap (32) set below git's documented default pack.depth (50): I built valid packs that real git reads but this reader rejects at depth >=33. Reimplementing git plumbing here is defensible (avoids a git dependency and process spawns) and the execution quality is high, but the depth cap plus a silently-swallowed error in the sole caller is the main release risk.

*Score rationale:* Correct and robust across an exhaustive matrix I tested empirically (loose/packed/delta/SHA-1/SHA-256/v1+v2 index/alternates/worktrees/packed-refs/symrefs), with disciplined checksum/layout/hash validation, bounded recursion, incremental size caps, and no panics on malformed input. Deductions: a real correctness divergence from git on default-depth (>32) delta chains, a caller that silently swallows read errors (masking that divergence), an O(n) per-symbol Huffman decoder, and the inherent maintenance/format-drift burden of ~4300 lines of hand-rolled plumbing. None are security holes; the main one is a faithfulness gap with a safe-failing but silent downstream effect.

**Verified behaviors (checked, not assumed):**

- Built a release example harness calling the public read_git_head_file and compared to `git show HEAD:path`: loose objects match byte-exact (src/sub/a.ts test)
- Packed + deltified: 61 similar files force OFS deltas; all match git after repack -adf --window=50 --depth=50
- SHA-256 repos (git init --object-format=sha256): 40 packed files match; loose also covered by module tests
- v1 pack index (pack.indexVersion=1, magic bytes 00000000): 20 files match
- Randomized corpus of 121 UTF-8 files (sizes 0..~5000 + a 100KB blob), packed: all byte-exact vs git
- Decompression-bomb resistance: 50MB compressible blob rejected at the pack size header (error, 2MB peak RSS); incremental push_output cap also enforced
- Malformed loose objects (truncate to 0/2/3 bytes, byte-flip, 50 random bytes): clean typed errors ('zlib stream is truncated', 'invalid zlib header', 'Invalid deflate back-reference distance'), no panic
- Corrupted pack body (5 random 2-byte injections): all caught by pack trailer checksum -> clean error, no panic
- Symlink HEAD:path returns link target (matches git); directory leaf -> 'resolved to a tree object'; missing path -> 'does not exist'; unborn HEAD -> 'Git ref refs/heads/master not found'; packed-refs-only HEAD resolves correctly
- Depth boundary precisely pinned with synthetic git-valid REF_DELTA packs: depth 31/32 succeed, depth 33/40/50 fail in the reader while `git show` succeeds
- End-to-end via the binary: wrote a decision (fn:login), edited file, `archiva hooks post-tool-use` reported re-anchoring correctly for both loose and packed HEAD blobs
- cargo test --release --lib core::git: 42 passed, 0 failed
- grep of non-test code (lines 1-2617) for unwrap/expect/panic: only two .expect on internal invariants (cache lookup, SHA-1 length overflow), no attacker-reachable panics found

### F21. [MEDIUM] Delta-chain depth cap (32) is below git's default pack.depth (50); valid packed objects fail to read

`defect` · location `src/core/git.rs:30 (const), :1333 (enforcement)` · reporter-confidence high · verification **CONFIRMED**

**Description:** GIT_PACK_DELTA_MAX_DEPTH is hardcoded to 32. read_pack_object_at returns the error "Git pack delta chain exceeded maximum depth" once recursion depth exceeds 32. Git's built-in default pack.depth is 50 (and `git gc --aggressive` uses 250), so a normally-packed repository can legitimately contain delta chains deeper than 32. When a HEAD-reachable blob/tree sits at depth >=33, this reader fails where real `git show HEAD:path` succeeds — a direct divergence from the project's stated goal of correctness vs real git.

**Why it matters:** The whole module exists to reproduce `git show HEAD:path` faithfully. A blob reachable from HEAD at depth >=33 yields an error instead of the file content, so post-tool-use re-anchoring cannot obtain the prior version of the file.

**Impact:** On affected repos, the native reader returns an error for valid objects. Because the sole caller swallows the error (see related finding), re-anchoring silently degrades rather than failing loudly.

**Likelihood:** Medium. Git's greedy packer tends to keep tip/HEAD objects shallow (a freshly aggressive-gc'd clone of this repo showed max depth 6, 0 objects >32), but default pack.depth=50 and large/older files can place HEAD-reachable objects beyond 32; it is not guaranteed to be shallow.

**Evidence (reporter):** Built synthetic but git-valid REF_DELTA chains via index-pack and tested with a harness calling read_git_head_file. Exact boundary: depth=31 -> OK ('BASE\nL31'), depth=32 -> OK, depth=33 -> 'ERR: Git pack delta chain exceeded maximum depth' while `git show HEAD:f.txt` returns 'L33'. Also confirmed depth=40 and depth=50 (git's default) both fail in the reader while git reads them. Reproduction script: python builds N-deep chain, then /target/release/examples/gitprobe vs `git -C show HEAD:f.txt`.

**Independent verification:** CODE (read directly):
- src/core/git.rs:30 -> `const GIT_PACK_DELTA_MAX_DEPTH: usize = 32;`
- src/core/git.rs:1330-1335 -> `read_pack_object_at(... depth: usize ...)` opens with `if depth > GIT_PACK_DELTA_MAX_DEPTH { return Err(git_error("Git pack delta chain exceeded maximum depth")); }`
- Depth semantics: read_git_object() (line 553) calls read_git_object_inner(.., 0, ..); each delta resolution recurses with depth+1 (OFS_DELTA at :1419 `depth + 1`, REF_DELTA at :1451 `depth + 1`). So `depth` == delta-chain index, and the guard rejects chains where the deepest object sits at index >= 33 (i.e. depth 33+). Boundary: depth 32 still allowed, depth 33 rejected.

GIT DEFAULT (verified, not assumed): `man git-pack-objects` on this host (git 2.34.1): "The default value for --window is 10 and --depth is 50. The maximum depth is 4095." `git config --get pack.depth` returns nothing (built-in default 50 in effect). `man git-gc`: gc.aggressiveDepth defaults to 50, aggressiveWindow 250. So git writes chains up to 50 by default and reads chains up to 4095.

EMPIRICAL REPRODUCTION (built a git-valid REF_DELTA chain via `git index-pack --stdin`, then a tree+commit pointing f.txt at the deepest blob; verified depth with `git verify-pack -v`; compared `git show HEAD:f.txt` against a `pub fn read_git_head_file` probe binary I compiled from examples/gitprobe.rs and then deleted):
  N=31 depth=31 git_lastline='L31' probe=OK ...L31
  N=32 depth=32 git_lastline='L32' probe=OK ...L32
  N=33 depth=33 git_lastline='L33' probe=ERR:Git pack delta chain exceeded maximum depth
  N=40 depth=40 git_lastline='L40' probe=ERR:Git pack delta chain exceeded maximum depth
  N=50 depth=50 git_lastline='L50' probe=ERR:Git pack delta chain exceeded maximum depth
At depth 32 the probe returned the complete content (`OK:BASE\nL1...L32`), confirming it is not an off-by-one truncation but a hard rejection one past the cap. `git verify-pack -v` confirmed the deepest blob's chain depth column matched N in each case. Git read all of L31..L50 successfully where the native reader failed at >=33.

CALLER-SWALLOW (confirmed, src/core/project.rs:273): `let old_content = read_git_head_file(project_root, old_git_file).unwrap_or_else(|_| new_content.clone());` — on the depth error the baseline silently becomes new_content, so `diff_lines(old, new)` yields no line changes and re-anchoring degrades silently rather than failing loudly. The .dlog itself is not corrupted; only the drift/diff baseline is wrong.

**Verifier notes / severity correction:** The defect is real and reproduced exactly as described: cited file:line locations are accurate, the depth==chain-depth mapping is correct, the 32/33 boundary is exact, git's default pack.depth=50 (max 4095) is confirmed from the installed man pages, and the sole caller swallows the error (project.rs:273). This is a genuine divergence from the stated "correctness vs real git" goal: git's pack.depth only bounds what git WRITES; at read time git follows chains up to 4095, so a cap of 32 is too low to faithfully read normally-packed (or aggressively gc'd) repositories.

Severity correction high -> medium. Rationale: (1) Triggering requires a HEAD-reachable blob whose delta chain index is >=33. In practice git's window-based delta selection resists building deep LINEAR chains from ordinary histories — when I let real git pack 70 shrinking and 70 appending commits with `repack --depth=50 --window=250`, the resulting blob chains were only depth 2-3, not 33+. Deep chains on a reachable file realistically require pathological content or aggressive repacking, so field likelihood is low-to-moderate, not high. (2) Impact is silent degradation of one derived feature (drift baseline / re-anchoring), not a crash, data loss, or corruption of the authoritative .dlog. The high label is defensible given the correctness-parity goal, but medium better reflects likelihood x impact.

Recommended fix: raise GIT_PACK_DELTA_MAX_DEPTH to at least git's documented maximum (4095) — it should function only as an anti-runaway/cycle guard, not a correctness limiter — and, separately, fix the error-swallowing at project.rs:273 so a genuine git-read failure surfaces (or at least is distinguished from "file absent at HEAD") rather than masquerading as "content unchanged." These are two coupled findings; fixing only the cap still leaves silent degradation for other read failures.

**Recommended resolution:** Raise the cap to at least git's documented maximum (>=255, since the on-disk depth field and git's ceiling allow it) or make it configurable, and prefer an iterative base-resolution loop with a visited-offset set over fixed-depth recursion. The cap should bound work/cycles, not reject objects git itself produced by default.

---

### F22. [LOW] Caller silently swallows git-read errors, masking the depth cap and other failures as 'no drift'

`defect` · location `src/core/project.rs:273` · reporter-confidence high · verification **PLAUSIBLE**

**Description:** The only production caller does `read_git_head_file(project_root, old_git_file).unwrap_or_else(|_| new_content.clone())`. Any error from the native reader (deep delta chain, >10MB blob, non-UTF-8 content, corrupt object) is discarded and the *current* working-tree content is substituted as the 'old' HEAD content. The subsequent diff (`diff_lines(&old_content, &new_content)`) then sees zero changes, so line-shift/staleness re-anchoring is computed against the wrong baseline with no warning.

**Why it matters:** Individually the reader 'correctly' errors and the caller 'correctly' has a fallback, but combined they produce wrong system behavior: decisions silently fail to re-anchor accurately whenever the HEAD read fails for any reason, including the depth-cap defect above.

**Impact:** Silent correctness degradation of the core decision-drift feature; users get no signal that re-anchoring used a degraded baseline.

**Likelihood:** Medium — triggers on every HEAD read failure, which is exactly the population of the depth-cap and >10MB cases.

**Evidence (reporter):** Code path traced: project.rs:273 fallback; reproduced end-to-end — wrote a decision against fn:login, edited the file, ran `archiva hooks post-tool-use`; with a normal HEAD it reported '1 stale, 0 orphan', but a deep-chain HEAD blob (gitprobe shows 'ERR: ... exceeded maximum depth') would fall through to new_content with no error surfaced.

**Independent verification:** Code: src/core/project.rs:273 `read_git_head_file(project_root, old_git_file).unwrap_or_else(|_| new_content.clone())` — error is discarded, no warning, confirmed. read_git_head_file (src/core/git.rs:153-189) can legitimately error: blob >10MB (git.rs:444), pack delta depth >32 (git.rs:1333), non-UTF-8 blob (git.rs:186). When swallowed, old_content==new_content so diff_lines yields an empty change set.

CRITICAL refutation of the claimed IMPACT: staleness is NOT computed from the diff baseline. is_fingerprint_stale (src/core/decision_status.rs:8-14) compares fingerprint(get_lines(new_content, lines_hint)) against the recorded fingerprint — pure current-content check. Orphan detection is anchor-existence based (project.rs:306-312). Both are independent of old_content. The diff only feeds apply_line_changes_to_range, i.e. lines_hint line-number shifting.

Reproductions in /tmp scratch repos with the release binary:
1. Tracked file, body edited, normal HEAD: `Re-anchored app.js: 1 stale, 0 orphan.` (correct).
2. UNTRACKED file newfile.js (git cat-file -e HEAD:newfile.js => "Not a valid object name", so read_git_head_file errors and the fallback fires), body changed: `Re-anchored newfile.js: 1 stale, 0 orphan.` — drift still flagged, NOT masked as "no drift".
3. Pure line-shift, body unchanged, prepend 3 comment lines:
   - Tracked (correct HEAD baseline): `0 stale`, lines_hint re-anchored 1-3 -> 4-6 (correct).
   - Untracked (fallback baseline): `Re-anchored untracked.js: 1 stale, 0 orphan.`, lines_hint LEFT at 1-3 (wrong) and a FALSE STALE emitted.
So the fallback degrades line-shift re-anchoring into FALSE POSITIVES, the opposite of the claimed "masked as no drift."

**Verifier notes / severity correction:** The underlying code fact is real and worth fixing: project.rs:273 silently swallows every git-reader error with no log/warning. But the finding is mis-scoped on impact and its headline is refuted. (1) Drift is NEVER masked as "no drift" — staleness (fingerprint) and orphan (anchor-existence) detection are independent of the git diff baseline; my repro shows the fallback still flags body changes as stale. (2) The actual consequence of the wrong baseline is the OPPOSITE: false-POSITIVE staleness and unshifted lines_hint for pure line-shifts on files whose HEAD can't be read. (3) Realistic blast radius is small: a readable HEAD never hits the fallback; the dominant trigger is untracked files, where there is genuinely no HEAD baseline and substituting current content (empty diff) is a defensible choice; the only true defect sub-case is a tracked file with a corrupt/>10MB/delta-depth>32 HEAD blob, and even then the false positive self-corrects on the next real edit. Recommend: distinguish "no HEAD baseline available" (expected, untracked) from a genuine reader error; on a genuine Err, surface a one-line warning to stderr and skip line-shift re-anchoring rather than substituting new_content, so line-shifts aren't mis-flagged. Severity corrected medium->low; it is a real but minor, self-correcting correctness/observability gap, not silent core-feature degradation.

**Recommended resolution:** Distinguish 'not in git / not found' (legitimately fall back) from hard read errors (deep chain, oversize, corruption). For the latter, log a warning or surface a degraded-mode notice rather than silently treating current==old.

---

### F23. [LOW] Huffman decoder does an O(entries) linear scan per code, O(entries x bit-length) per symbol

`operational` · location `src/core/git.rs:2093 (Huffman::decode)` · reporter-confidence high · verification **CONFIRMED**

**Description:** decode() walks `self.entries` with `.find()` for every bit length from 1..=max_len for every symbol. The literal table can hold ~286 entries, so each decoded symbol costs hundreds of comparisons. A standard inflate uses a precomputed code->symbol lookup table (O(1) per symbol).

**Why it matters:** post-tool-use inflates one HEAD blob per invocation; large or poorly-compressible source files make this the dominant cost.

**Impact:** Measured ~0.2s for a 1MB literal-heavy packed blob and ~2.0s for a 9MB one (loose or packed). Bounded by the 10MB output cap to roughly ~2.2s worst case, but that is a noticeable per-hook latency on large files.

**Likelihood:** Low-Medium — only matters for large source files; typical sources are well under 1MB (sub-100ms).

**Evidence (reporter):** `/usr/bin/time` on gitprobe: 9.6MB compressible blob 0.09s; 9MB base64 (literal-heavy) 2.02s; 1MB base64 0.22s. Code at decode() confirms `self.entries.iter().find(...)` inside a `for len in 1..=self.max_len` loop.

**Independent verification:** CODE (src/core/git.rs:2093-2110, Huffman::decode): exact match to the claim. For `len in 1..=self.max_len` it reads one bit then calls `self.entries.iter().find(|entry| entry.len == len && entry.code == code)`. This is an O(entries) linear scan repeated for every bit length, so O(entries x bit-length) per decoded symbol. The literal alphabet is up to 288 entries (line 1979: `literal_count = read_bits(5) + 257` => max 288; fixed table at line 2186 is `vec![0u8; 288]`), and max_len can be up to 15 (line 2047). No precomputed code->symbol lookup table exists; from_lengths (2042-2081) builds a flat Vec<HuffmanEntry>. has_symbol/has_symbol_range (2083-2091) are also linear but off the hot path.

REPRODUCTION (real binary /home/ubuntu/archaeo/target/release/archiva, not the prior auditor's gitprobe). The decode path is reachable via `hooks post-tool-use`, which calls read_git_head_file -> zlib_inflate -> Huffman::decode on the committed loose blob. To isolate Huffman cost from the hook's line-diff (which is separately O(n*m) and dominates multi-line files), I used files whose large literal body is on a SINGLE line so the line-diff is trivial:
- LITERAL (base64, literal-heavy) 1,000,000 bytes: 0.24 s
- LITERAL 2,000,000 bytes: 0.47 s
- LITERAL 4,000,000 bytes: 0.99 s
- COMPRESSIBLE 6 MB blob (repeated 'A'): 0.08 s
Linear in literal-byte count (~0.24 s/MB), and ~30x faster for an equal-size compressible blob, exactly matching a literal-count-bound cost. Extrapolating to the ~11 MB output cap (GIT_OBJECT_STORAGE_MAX_BYTES, git.rs:12) gives ~2.4 s worst case. These figures align with the prior auditor's numbers (1MB ~0.22s, 9MB ~2.02s, 9.6MB compressible ~0.09s).

Separately observed but OUT OF SCOPE for this finding: a multi-line 8 MB file took 63-80 s in post-tool-use, but that is the line-diff/anchor path, not the Huffman decoder; the Huffman finding itself is correctly bounded to ~2s by the output cap.

**Verifier notes / severity correction:** CONFIRMED as stated, severity low is accurate. The algorithm, the ~288-entry table bound, and all cited timings reproduce on the real binary. Minor location correction: the actual scan loop is git.rs:2098-2108; line 2093 is just the fn signature. The prior auditor's "gitprobe" harness was unavailable, but I reproduced the same behavior through the shipped `hooks post-tool-use` code path. Recommended fix is correct: replace the linear .find() with a canonical-Huffman fast lookup table (e.g. a flat array indexed by the next max_len bits, or per-length first-code/base-index arrays via the counts/next_codes already computed in from_lengths) for O(1)-amortized decode. Caveat for the release audit: this finding is the smaller of two latency issues on the post-tool-use hook; the line-diff cost on large multi-line files (tens of seconds) is a more severe, separate problem that this finding does not cover.

**Recommended resolution:** Build a flat lookup table (canonical Huffman fast table, e.g. 9-bit root with sub-tables) or at least index entries by length. Pure quality/perf improvement; correctness is unaffected (adler32 + size checks guard output).

---

### F24. [LOW] 10 MiB object size cap rejects HEAD blobs that real git reads

`tradeoff` · location `src/core/git.rs:11 (GIT_OUTPUT_MAX_BYTES), :444, :1525, :1586` · reporter-confidence high · verification **CONFIRMED**

**Description:** Objects whose inflated size exceeds 10 MiB are rejected (loose, packed, and delta target all enforce GIT_OUTPUT_MAX_BYTES). This is a deliberate decompression-bomb / resource guard, but it means `HEAD:path` for a legitimately large tracked file (>10MB) errors where `git show` would succeed.

**Why it matters:** Another source-of-divergence from real git, and another trigger for the silently-swallowed-error path in project.rs:273.

**Impact:** Large tracked files (minified bundles, generated data, lockfiles) over 10MB cannot be re-anchored against HEAD; the caller silently falls back to current content.

**Likelihood:** Low — most source files are far below 10MB.

**Evidence (reporter):** 50MB compressible blob: reader returns 'Git pack object inflated size 50000000 exceeds 10485760 bytes' (rejected from the pack size header, 2MB peak RSS) while `git cat-file -s HEAD:bomb.txt` = 50000000. Confirms both the protection and the divergence.

**Independent verification:** CODE (src/core/git.rs): The 10 MiB cap and its three enforcement points are exactly as cited.
- :11 `const GIT_OUTPUT_MAX_BYTES: usize = 10 * 1024 * 1024;` (= 10485760).
- :444-447 loose path: after inflating up to GIT_OBJECT_STORAGE_MAX_BYTES (11 MiB), parses the object header size and rejects `if size > GIT_OUTPUT_MAX_BYTES` -> "Git object {oid} size {size} exceeds {GIT_OUTPUT_MAX_BYTES} bytes".
- :1522-1528 packed path: blob type (3) maps to `size_limit = GIT_OUTPUT_MAX_BYTES`; rejects from the pack object's varint size header BEFORE inflating -> "Git pack object inflated size {size} exceeds {size_limit} bytes" (matches the claimed evidence string verbatim for a 50 MB packed blob: "...50000000 exceeds 10485760 bytes"). Only delta types 6/7 get the larger 11 MiB storage budget.
- :1586-1589 delta target: `if target_size > GIT_OUTPUT_MAX_BYTES` -> rejects. zlib_inflate also hard-caps output via push_output (:exceeds {max_output} bytes), so the guard is genuinely a resource cap, not cosmetic.

RUNTIME (end-to-end, not just unit code): /tmp/e2ecap, a real git repo with src/big.js = 11,534,401 bytes tracked at HEAD (git cat-file -s HEAD:src/big.js = 11534401 > cap). Recorded a decision on anchor fn:configure at lines [1,3], then inserted 5 lines at the top and ran `archiva hooks post-tool-use src/big.js`. Result: exit 0, output "Re-anchored src/big.js: 1 stale, 0 orphan." and stored lines_hint stayed at 1-3 even though configure is now at lines 6-8.
CONTRAST (/tmp/smallcap): identical scenario with a 3-line file (under cap) -> "0 stale", lines_hint correctly shifted to 6-8.
This proves both halves of the claim: (a) >10 MB HEAD blobs are rejected by the native reader, and (b) the caller (src/core/project.rs:273 `read_git_head_file(...).unwrap_or_else(|_| new_content.clone())`) silently falls back to current content, so old==new, no diff is computed, and the line hint is left wrong AND the decision is mis-marked STALE. git show / git cat-file would have supplied the real baseline.

Also confirmed at git layer (/tmp/gitcaptest): a 50 MB tracked file -> git cat-file -s HEAD:bomb.txt = 50000000, far above the cap.

**Verifier notes / severity correction:** CONFIRMED as stated; severity/category (tradeoff/low) is correct and should stand. The cap is a deliberate, well-implemented decompression-bomb guard (packed objects reject cheaply from the size header before inflating, matching the claimed ~2 MB peak RSS; loose objects inflate up to ~11 MiB then reject). The git divergence is real and only affects tracked files >10 MiB (minified bundles, lockfiles, generated data) — uncommon, and the failure degrades gracefully (no crash, exit 0).

One refinement to the claimed IMPACT, slightly worse than "silently falls back": because the fallback sets old_content == new_content, post-tool-use computes no line diff, so it not only fails to shift the stored range but actively reports the unchanged-but-shifted anchor as STALE (my run: "1 stale"), whereas the under-cap control reported "0 stale". So the user-visible effect is an incorrect STALE marking plus a stale line hint, not merely a silent no-op. Still low severity given the >10 MiB precondition.

Minor scope note for the report: the claimed evidence's exact error string ("Git pack object inflated size ... exceeds ...") is specifically the PACKED-blob path (git.rs:1527). The loose-blob path emits a different message ("Git object {oid} size {size} exceeds ..."), but enforces the same 10 MiB cap — both were verified.

**Recommended resolution:** Reasonable default; document it and, for the caller, treat 'too large' as a known-degraded baseline (warn) rather than silent fallback. Consider making the cap configurable for repos with large tracked assets.

---

### F25. [INFO] Reimplementing git plumbing is warranted here, with caveats

`architecture` · location `src/core/git.rs (whole module)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** The module replaces `git show HEAD:path` with a from-scratch reader (own zlib/DEFLATE, SHA-1, pack walking). I verified it is correct across loose/packed/deltified/SHA-1/SHA-256/v1+v2 index/alternates/linked-worktree/packed-refs/symbolic-ref/unborn-HEAD cases (200+ files byte-exact vs git), robust against malformed input (no panics found; clean typed errors on truncation, garbage, corrupt checksums, header-count mismatch, oversubscribed Huffman, trailing deflate bytes), and hardened (incremental output cap, pack/index checksum + trailer + header-count validation, bounded alternates/symref recursion, ref-segment path validation, OID hash verification of every returned object).

**Why it matters:** Avoiding a git subprocess removes a runtime dependency, a PATH/version-skew surface, and shell/argv injection risk, and gives deterministic resource limits. The cost is ~4300 lines of security-sensitive parsing that must track git's format evolution (e.g., multi-pack-index/MIDX, reftable, pack v3/SHA-256 details).

**Impact:** Maintenance burden and format-drift risk vs git; offset by strong validation and test coverage.

**Likelihood:** n/a

**Evidence (reporter):** 42 module tests pass; my independent harness (loose/packed/sha256/v1/deltas/edge cases) matched git in every UTF-8 case; malformed-input matrix produced only typed errors. No use of `git show`/spawn confirmed by grep.

**Independent verification:** I independently reproduced every load-bearing claim rather than trusting the in-repo tests.

1) No production git spawn. grep across the ENTIRE src tree for `Command::new`/`process::Command` returns hits only at git.rs:2634/3881/3896, project.rs:702/2226, fs.rs:836/1317 — every one inside a `#[cfg(test)]` module (verified test-mod start lines all precede the call lines). The production read path read_git_head_file -> read_git_head_file_native (src/core/git.rs:153-189) is pure native: resolve_head_oid -> read_git_object_with_context -> commit_tree_oid -> tree_blob_oid -> blob. Its sole production caller is project.rs:273 (drift detection), used with unwrap_or fallback.

2) Byte-exact correctness vs real `git show`. I wrote my OWN harness (tests/zz_audit_git_harness.rs, since deleted) building real repos with git 2.34.1 and comparing read_git_head_file output to `git show HEAD:path` byte-for-byte. 15 independent cases ALL PASS: loose (small/compressible/incompressible), packed+deltified, deltified HEAD blob at depth>=1 (verify-pack confirmed the detached-HEAD blob 0d29326e was a depth-1 delta against base 4aa7ff), sha256 repo, nested paths, packed-refs + symbolic HEAD, linked worktree (commit in linked tree), empty blob, alternates (clone --shared, blob only in origin pack), v1 pack index (forced pack.indexVersion=1, confirmed magic != \377tOc), unborn HEAD, missing-file.

3) Malformed input -> clean typed errors, no panics. Truncated loose object -> "Deflate stream ended unexpectedly"; garbage deflate -> "Git object uses reserved deflate block type"; byte-flipped pack -> "Git pack ... trailer checksum mismatch"; unborn HEAD -> "Git ref refs/heads/master not found"; missing path -> "HEAD:...does not exist". No test thread panicked inside the reader.

4) Hardening present in code: GIT_OUTPUT_MAX_BYTES=10MB / storage cap / marker cap (git.rs:11-13), GIT_PACK_DELTA_MAX_DEPTH=32, GIT_ALTERNATES_MAX_DEPTH=8, GIT_SYMBOLIC_REF_MAX_DEPTH=8 (git.rs:30-32), pack index checksum/trailer/header-count validation (validate_pack_index_checksum, validate_pack_trailer_matches_index_once, header-count mismatch), and OID hash verification of EVERY returned object — verify_git_object_hash(oid,&object,object_format) is called in read_git_object (git.rs:579) which dispatches to BOTH loose and packed/delta paths (read_git_object_from_dir, git.rs:590-605), so loose and packed/delta-resolved objects are both hash-checked.

The in-repo git tests also pass: `cargo test --release --lib core::git` = 42 passed, 0 failed.

**Verifier notes / severity correction:** CONFIRMED and correctly scoped/severitied. This is an architectural observation (info), not a defect — the prior auditor's framing is accurate. Every substantive correctness, robustness, and hardening claim reproduced under my own adversarial harness against real git, including the harder cases (depth>=1 delta reconstruction, v1 index, alternates, linked worktree, sha256). The cited caveats (maintenance burden + format-drift risk offset by validation and coverage) are a legitimate design tradeoff, not understated. Two minor accuracy nits, neither material: (a) the "200+ files byte-exact" figure is the prior auditor's count — I verified ~15 independent scenarios spanning multiple files/commits each plus the repo's 42 module tests, which corroborate the breadth but I did not literally diff 200 files; (b) correctness is scoped to UTF-8 blobs by design — read_git_head_file_native returns a typed Git error on non-UTF-8 content (git.rs:186-188), so binary HEAD files are rejected, not mis-decoded. This UTF-8 scoping matches how the only production caller (drift detection on source text) uses it.

**Recommended resolution:** Keep the native reader. Address the depth cap; add a regression test using a synthetic depth>=50 pack; and note unsupported features explicitly (MIDX/.midx is not consulted — only .idx files are scanned, which is correct for standard repos but would miss midx-only layouts).

---

## Decision logic: validation, supersession, history  — score 7/10

> This subsystem builds, validates, supersedes, and audits decision records. The pure helpers in the four assigned files are clean, well-tested, and faithful to the TypeScript contract; the real orchestration lives in project.rs and storage.rs. I verified end-to-end that fingerprint drift flips a decision to STALE and persists it, that deleting/renaming an anchor produces ORPHAN (via post-tool-use) and arc/orphan warnings (via lint), that supersession correctly chains history, preserves prior records in the history array, prevents superseding historical-only ids (no dangling-ref cycles), and that id generation skips non-matching/history ids. Recovery (restoring source) cleanly clears STALE. However I found a genuine data-corruption defect: the std-only YAML emitter does not single-quote strings with leading/trailing/only ASCII whitespace, so write-decision either silently truncates whitespace from user free-text fields or hard-fails the write — a regression versus js-yaml, which round-trips these losslessly. I also confirmed two data-loss paths (re-deciding an anchor, or superseding into an occupied anchor) that are faithful to the TS original but undocumented and silent, plus a status/lint orphan-count inconsistency and an unvalidated line-range "zombie decision" that can never go stale. The supersession/history/status core logic itself is correct.

*Score rationale:* The assigned files (decision.rs, decision_status.rs, status.rs, lint.rs) are clean, cohesive, exhaustively unit-tested, and faithful to the TS contract; supersession history-chaining, cycle prevention, id generation, and STALE/recovery transitions are all correct and verified end-to-end. The score is held back by one genuine high-severity data-corruption defect in the shared YAML emitter that this subsystem depends on (whitespace truncation/write failure for decision text — a real regression vs js-yaml), plus several silent data-loss paths (anchor overwrite, supersede-into-occupied-anchor), a status/lint orphan inconsistency, an unvalidated line range that disables drift detection, and read-commands that mutate files. Most are inherited faithfully from the TS original, but they are real correctness/safety gaps for a release.

**Verified behaviors (checked, not assumed):**

- Ran the release binary: init -> write-decision -> dlog/.dmap created with correct schema:1 YAML and compact dmap index (1-4:fn:compute).
- Fingerprint drift: edited function body, `lint` reported ERROR arc/stale and persisted status: STALE + stale_since to dlog and STALE to dmap (verified file contents).
- Second `lint` on an already-stale decision emitted ERROR arc/supersede ('stale and has not been superseded') as designed.
- Recovery: restoring original source then `lint` cleared status cleanly (no status field), exit 0 — clear_recovered_status works end to end.
- ORPHAN via post-tool-use: renamed anchor + `hooks post-tool-use` => 'Re-anchored: 0 stale, 1 orphan', dlog/dmap show ORPHAN, `status` reports 1 orphan.
- ORPHAN via lint diverges: same rename, `lint` warns arc/orphan but never persists ORPHAN; `status` shows 0 orphan — confirmed the two paths disagree.
- Supersession happy path: chained supersedes (dec_001->dec_002->dec_003) accumulate history correctly; superseding a history-only id is rejected ('Cannot supersede unknown decision id'), preventing dangling refs/cycles.
- next_decision_id scans only live decisions (not history ids) and correctly produced dec_004 after a 3-deep supersede chain; u128 parse guards the format!('dec_{:03}').
- Data loss A: re-decided same anchor without supersedes => prior decision and history silently gone (history command shows only latest).
- Data loss B: supersede dec_001 into anchor occupied by unrelated dec_002 => dec_002 silently destroyed, not in history.
- YAML whitespace regression: ' leading' chose silently truncated to 'leading' on round-trip; '   ' chose hard-fails write with 'decisions.fn:alpha.chose: expected string'; js-yaml round-trips all three losslessly (compared via node).
- Out-of-range lines [50,100] on a 3-line file accepted; fingerprint = empty-input hash e3b0c442; decision never goes STALE under edits (zombie).
- `status` (no --fix) mutated dlog (md5 changed, status: STALE written) while printing 0 stale — read command has write side effects.
- Lock hygiene: failed whitespace write left no .lock in scratch dirs (RAII release on the pre-write round-trip failure).
- Ran cargo test --release --lib yaml: 17 passed; confirmed the roundtrip property test (random_yaml_edge_char) excludes space from edge positions, explaining why the whitespace bug was uncaught.

### F26. [HIGH] YAML emitter corrupts or rejects decision text with leading/trailing whitespace (port regression vs js-yaml)

`defect` · location `src/core/yaml.rs:963 (needs_single_quotes) and src/core/storage.rs:131-134 (write_dlog round-trip guard)` · reporter-confidence high · verification **CONFIRMED**

**Description:** render_scalar (yaml.rs:918) emits a String as a bare plain scalar unless needs_single_quotes returns true. needs_single_quotes (yaml.rs:963-977) checks for empty, leading special chars, ': ', ' #', booleans, numbers, timestamps — but NOT leading or trailing ASCII whitespace, and not whitespace-only-but-nonempty strings. The YAML block parser strips surrounding whitespace after 'key: '. Result: any user-supplied free-text field (chose, because, approach, reason, expires_if, session) that has leading/trailing spaces is silently truncated on the render->parse round-trip, and a whitespace-ONLY field renders to something that re-parses as a non-string, tripping the write_dlog round-trip guard (parse_dlog_yaml at storage.rs:133) and failing the entire write-decision with a confusing internal error 'decisions.<anchor>.chose: expected string'. The TypeScript original uses js-yaml, which single-quotes all three cases and round-trips them losslessly (verified directly).

**Why it matters:** This is a v2 re-engineering whose explicit goal is byte/behavior parity with the TS tool. Decision text is the entire product value. Silent truncation corrupts the recorded rationale; the hard-fail case blocks legitimate writes with an error that points at the wrong layer.

**Impact:** Silent data corruption of decision rationale (leading/trailing space) or unexpected write-decision failure (whitespace-only field). Affects both the CLI and the MCP write path since both go through write_decision_record_locked.

**Likelihood:** Medium — agents and humans routinely paste text with stray leading/trailing whitespace or newlines-trimmed-to-spaces; whitespace-only is rarer but produces a hard failure.

**Evidence (reporter):** Reproduced in /tmp: `archiva write-decision --json '{...,"chose":" leading",...}'` then `archiva why` prints `Chose: leading` (leading space gone; dlog line is `    chose:  leading` which re-parses trimmed). `{...,"chose":"   ",...}` exits 1 with `decisions.fn:alpha.chose: expected string` and writes no dlog. Comparison: `node -e` with js-yaml dump+load of '   ', ' leading', 'trailing ' all round-trip OK with single-quoting. The roundtrip property test (property_tests.rs:401-403 random_yaml_edge_char) deliberately excludes space from first/last char positions, so the gap is untested.

**Independent verification:** Verified by reading source and running the release binary + MCP server.

CODE PATH:
- src/core/yaml.rs:963-977 needs_single_quotes() has NO check for leading/trailing ASCII whitespace nor whitespace-only strings. render_scalar (yaml.rs:918) therefore emits such strings as bare plain scalars.
- src/core/yaml.rs:280 parse_mapping_value() does value_part.trim_start(); parse_scalar_with_options (yaml.rs:388) does .trim_end(). So a round-trip strips surrounding whitespace. A whitespace-only value collapses to "" which parse_scalar_value (yaml.rs:431) returns... actually re-parses the now-empty mapping value as a nested block -> non-string, failing dlog.rs:335 expect_non_empty_string with "expected string".
- src/core/storage.rs:131-134 write_dlog() renders then immediately parse_dlog_yaml() as a guard; on the whitespace-only case this guard rejects and aborts the write (no dlog written).
- Validation does NOT trim: src/core/decision.rs:381-387 expect_non_empty_string only rejects value.is_empty(); "   " and " leading" pass through to the emitter unchanged.

LIVE REPRODUCTION (/tmp/wsaudit, real fn:alpha anchor in foo.rs):
1. Leading space, chose=" leading": exit 0, "Recorded dec_001." dlog line is `    chose:  leading` (cat -A confirmed). `archiva why foo.rs` prints `Chose: leading` — leading space SILENTLY LOST. Trailing space "trailing " behaves identically (dlog `    chose: trailing # Archiva v2 — Release-Readiness Audit

**Auditor role:** Independent Principal Software Architect / Release Auditor
**Subject:** Archiva v2 — std-only Rust re-engineering of a TypeScript "decision memory for AI coding agents" tool
**Date:** 2026-07-01
**Branch audited:** `codex/archiva-v2-rust-validation` (HEAD `33f160e`, version 0.2.0)

**Method:** 115 independent agents across 17 subsystem/dimension reviews; every substantive finding adversarially re-verified by a second agent that reproduced it against the compiled release binary or read the cited code. Documentation and the team's own `docs/archiva-v2-review-status.md` were treated as **unverified claims**, not evidence.

**Independently verified baseline (by the auditor before fan-out):**

- `cargo build --release` — clean.
- `cargo clippy --all-targets --locked -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `cargo test` — 301 lib tests pass (1 ignored) + 9 + 1 + 3 integration tests pass.
- Binary functional: `archiva --version` → `0.2.0`; `archiva status` on the repo → 537 decisions, 0 stale, 0 orphan, 0 issues across 56 `.dlog` files.

**Verification outcome across the panel:** 96 findings `CONFIRMED`, 1 `PLAUSIBLE`, **0 `REFUTED`**. Severity distribution (corrected, non-refuted): **17 high / 27 medium / 44 low / 9 info**, plus 10 audit-coverage gaps from a completeness critic.

---

## 1. Executive Summary

Archiva v2 is a genuinely impressive piece of engineering: a zero-dependency, std-only Rust implementation of a multi-language anchor extractor, a from-scratch git object reader (SHA-1 **and** SHA-256, packs, deltas, alternates), hand-written JSON/YAML parsers, a CLI, and a stdio MCP server — all clean-compiling, clippy-strict, well-formatted, and backed by 300+ tests and an elaborate differential/stress/scale/corpus validation harness. The code quality at the unit level is high and the discipline is real.

**But it is not ready for a stable 1.0 public release, and it is not yet the reference implementation.** The audit confirmed a coherent cluster of release-blocking problems that the existing test strategy structurally cannot see:

1. **A class of trivially-reachable process aborts (panics / stack overflows) triggered by ordinary committed, team-shared data.** Three distinct crashes were reproduced (a lone `'` in a `.dlog` → `yaml.rs:700`; a mid-codepoint UTF-8 slice → `yaml.rs:311`; deeply-nested source → unbounded recursion in the anchor extractor), and the completeness critic found a **fourth** (empty block scalar) in minutes. Because `.decisions/` is git-tracked by default and shared across a team, a single malformed byte in one file aborts `status`, `lint`, the per-session `session-start` hook, and — most seriously — **kills the long-lived MCP server mid-session**, dropping all in-flight agent context. This is a class, not a list.

2. **The product's headline automatic workflow is broken end-to-end.** The auto-wired `PostToolUse` re-anchor hook is a confirmed no-op under real Claude Code: `init` wires it with no argument relying on `ARCHIVA_FILE`, but Claude Code delivers the edited path as JSON on stdin, which `post-tool-use` never reads. It errors on every edit and silently never re-anchors. Compounding this, even when invoked correctly, re-anchoring is **non-idempotent** and **falsely marks correct decisions STALE** whenever there is no committed HEAD baseline (new files, or multiple edits between commits) — and the corruption compounds and does not self-heal.

3. **Performance cliffs contradict the "scales to large repos" claim, and the scale harness is blind to both of them.** Anchor extraction is O(n²) per file (a 1.4 MB file ≈ 55–66 s for one file); the hot-file write path is O(n²) cumulative (1,200 decisions in one file ≈ 86 s). The scale-smoke harness uses tiny one-function files and skips any file > 256 KiB, so neither bottleneck is ever exercised.

4. **Operational diagnosability is essentially zero** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery (dmap repair, stale-lock takeover) — for a tool that runs unattended as an agent hook.

None of the high-severity findings are memory-unsafety or RCE — Rust aborts cleanly — so the worst case is availability/DoS and silent metadata corruption, not exploitation. The defects are concentrated, well-understood, and individually fixable without architectural change. With a focused 4–8 week remediation pass (panic-safety hardening, hook stdin contract, idempotent re-anchoring, the O(n²) fixes, and a logging channel), this can become an excellent 1.0.

**Verdict: Do not ship as-is. Strong foundation; specific, fixable blockers.**

---

## 2. System Overview

Archiva stores *why code exists* beside the code. Per source file it maintains `.decisions/<path>.dlog` (authoritative YAML, schema:1) and `.decisions/<path>.dmap` (compact derivative index). Decisions are anchored to AST identities (`fn:foo`, `struct:Bar`, `block:if_x`) rather than line numbers, carry a fingerprint for drift detection, and form supersession chains. The same core operations are reachable three ways — CLI (`init`/`why`/`history`/`lint`/`status`/`hooks`/`write-decision`/`mcp`), a stdio JSON-RPC MCP server (`why`/`write_decision`/`ghost_check`), and Claude Code hooks (`session-start`/`post-tool-use`). Distribution is via an npm wrapper that selects a platform-specific native binary; the runtime is a single native binary with **no dependencies** (`Cargo.toml` `[dependencies]` is empty).

The architecture is sound and the product thesis is coherent. The problems are in robustness, the hook integration contract, performance at scale, and observability — not in the concept or the module decomposition.

**Module sizes (Rust, src/):** `anchor.rs` 12,341 (incl. ~5,090 test lines) · `git.rs` 4,329 · `project.rs` 2,261 · `fs.rs` 1,487 · `yaml.rs` 1,465 · `mcp.rs` 1,174 · `cli.rs` 1,030 · `decision.rs` 963 · `storage.rs` 959 · `json.rs` 722 · `diff.rs` 657 · `property_tests.rs` 540 · `dlog.rs` 507 · `paths.rs` 487. Total ~31,464 lines.

---

## 3. Architectural Assessment — **8/10**

Module boundaries are clean and cohesive: `cli`/`mcp` entrypoints → `core::project` orchestration → typed core modules (`decision`, `storage`, `dlog`/`dmap`, `anchor`, `git`, `paths`, `fs`). Coupling is low and the data-flow ownership (`.dlog` authoritative, `.dmap` rebuildable, request-scoped git reader) is well-reasoned.

The central architectural tension is the **zero-dependency, reimplement-everything-by-hand** stance: ~2.6k lines of git plumbing including a from-scratch DEFLATE inflater (`git.rs`), hand-written JSON/YAML parsers, and a multi-thousand-line multi-language anchor tokenizer. This is defensible *as a product tradeoff* (trivial supply chain, tiny binary, no transitive CVEs) but it concentrates the entire bug surface in hand-rolled parsers that have **not been fuzzed** — and that is precisely where every confirmed panic lives. The std-only purity is the root cause of the dominant risk class.

Two concrete architectural weaknesses (both verified):

- **No schema migration story.** `DLOG_SCHEMA_VERSION` is a hardcoded `1` and the parser hard-rejects anything else; a single forward-version file aborts every whole-repo command. There is no migrate-on-read and no skip-with-warning.
- **The ground-truth anchor range is computed and then discarded** in the normal re-anchor path (`project.rs:290-304`) — the parser already knows the anchor's exact current position, but the code trusts a fragile HEAD-diff shift instead. This is the root cause of the idempotency/STALE corruption.

Top simplification opportunity: prefer the extractor's live anchor position over diff-shifting; this single change fixes two high-severity findings at once.

---

## 4. Workflow Assessment — **5/10**

The intended loop is *read map → ask why → edit → write decision → lint drift*. Traced end-to-end:

- **`init` → first decision → `why`**: works cleanly; idempotent on re-run.
- **`write-decision`**: works, but is **non-atomic across `.dlog`/`.dmap`** (a torn write durably commits the decision while reporting failure, exit 1) and the natural retry is **non-idempotent** (overwrites the just-committed record with a new id, losing the original reasoning with no history entry).
- **Auto re-anchor on edit (the core promise)**: **broken under real Claude Code** (hook ignores the stdin payload) and **corrupts line attribution** even when invoked correctly (false STALE + compounding line drift with no committed baseline).
- **Re-deciding an anchor**: silently destroys the prior decision and its entire history unless `supersedes:<id>` is passed; superseding *into* an anchor occupied by a different live decision silently deletes that unrelated decision.

The "happy path" demos work; the realistic agent workflow (uncommitted files, multiple edits per commit, re-decisions) has multiple silent-data-integrity failures.

---

## 5. Feature Completeness Assessment — **7/10**

The advertised command set is fully present and the CLI surface is consistent and complete. Gaps are in fidelity rather than coverage: MCP `why` cannot do line-based lookup (the `line` field is silently dropped and it returns a *confidently wrong* whole-file result rather than "not found"); no `--fix` audit trail; no migration tooling. Feature breadth is appropriate and intentionally narrow (the README's positioning vs. broad memory tools is honest and well-argued).

---

## 6. Behavioral Consistency Assessment — **6/10**

CLI ↔ MCP ↔ hooks largely agree on validation and path normalization (verified: `.//src/a.ts` and `src\a.ts` normalize to one identity across entrypoints). Confirmed divergences:

- **MCP `why` ignores `line`** and returns the wrong decision; CLI `why <file> <line>` is correct.
- **MCP tool errors are returned as JSON-RPC protocol errors (`-32000`)** instead of the MCP convention `result.isError:true` — a spec inconsistency for tool *execution* failures.
- **`lint` exit code conflates** "found issues" with "command failed"; `status` returns 0 even with outstanding issues. No exit-code taxonomy.

---

## 7. Data Model Assessment — **7/10**

The decision record (chose/because/rejected/anchor/fingerprint/lines_hint/history/supersession) is well-designed and matches the spec and the YAML schema. Two material risks: **unknown/forward-compat fields are silently dropped on every rewrite** (data loss for any external or future-version annotation), and the **no-supersede overwrite** path discards history. The model is good; its *evolution and preservation guarantees* are weak.

---

## 8. Storage & Persistence Assessment — **7/10**

Individual writes are atomic (temp + rename + fsync; Unix parent-dir fsync). Verified strengths: corrupt `.dmap` self-heals from `.dlog`; stale-lock recovery works. Verified weaknesses:

- **No cross-file atomicity** between `.dlog` and `.dmap` (torn-write false-failure).
- **PID-liveness lock veto can wedge all writes indefinitely** (PID reuse defeats the staleness check; no force-unlock).
- **Read-only `status`/`lint` acquire the per-file write lock** and fail on a read-only `.decisions/`.
- Parent-dir durability fsync is a **no-op on Windows** (crash-consistency claim is weaker there and untested in crash-injection form).

---

## 9. CLI Assessment — **8/10**

The strongest subsystem. Dispatch, help text, exit-code routing (0/1), `--` escaping, and unknown-flag/command handling are correct and extensively tested. Rough edges: `write-decision` **reads stdin before validating its own args** (a malformed call with an open stdin hangs), the `lint` exit-code conflation, inconsistent `error:`-prefixing between argument vs. semantic errors, and `why` accepting line `0`.

---

## 10. Protocol & Integration Assessment — **6/10**

JSON-RPC 2.0 framing, id handling, `notifications/*` swallowing, and method dispatch are correct and tested. **The integration contract with Claude Code is broken** (PostToolUse stdin payload ignored — the single most important integration defect). MCP tool-error encoding deviates from the MCP convention. And the long-lived server has **no panic isolation** — one crafted `tools/call` aborts the whole process.

---

## 11. Performance Assessment — **6/10**

Measured, not assumed:

- **Anchor extraction O(n²) per file** (`is_top_level`/`rust_depths_before` re-scan the token prefix per declaration): TS 10k/20k/40k lines = 0.5/2.3/9.9 s; a 1.4 MB file ≈ 55–66 s; a single huge Rust fn ≈ 47 s vs ~3 s for the TS equivalent. Memory stays modest (~70 MB) — purely CPU.
- **Hot-file writes O(n²) cumulative** (full render + redundant re-parse + full rewrite + fsync per write): #1,200 in one file ≈ 86 s cumulative; 1,500 timed out at 120 s. The `storage.rs:133` self re-parse of freshly-rendered YAML is unconditional wasted work.
- **`status` parses every `.dlog` twice** per invocation and sweeps the whole source tree.
- The `.dmap` index **is never read by any command** — every read re-parses the verbose `.dlog`, so the derivative index pays maintenance cost for zero read benefit.

Startup is excellent (~0.7 ms). For small/medium repos performance is fine; the cliffs are real and reachable.

---

## 12. Scalability Assessment — **5/10**

The "scales to 100k files / 1M decisions / Linux-kernel-and-LLVM corpora" claim is **not substantiated by the harness**, which uses tiny one-function files, skips files > 256 KiB, and defaults to 1 decision/file — exactly avoiding both quadratic paths. The blast radius is also wider than the per-command framing: because whole-repo commands re-extract anchors from *every* source file (decided or not), one large generated/minified/vendored file degrades `lint`/`status` repo-wide. Linear-in-file-count scanning is otherwise reasonable.

---

## 13. Reliability Assessment — **5/10**

Dominated by the panic class: `status`, `lint`, `session-start`, and the MCP server all abort (SIGABRT / exit 101 or 134) on malformed-but-committed input, and **a single corrupt `.dlog` aborts the entire repo-wide command** rather than being skipped and reported — taking down visibility for all healthy files. Recovery paths that exist (dmap repair, lock recovery) are correct but silent. Single-file commands are correctly scoped and limit some blast radius.

---

## 14. Failure Recovery Assessment — **6/10**

Good: atomic single-file writes, self-healing dmap, stale-lock recovery. Weak: no cross-file transaction; `lint --fix` is non-atomic across files and leaves a partially-fixed repo with no record of what changed on mid-run failure; no panic boundary so corruption aborts rather than degrades; no migration/forward-compat recovery. The recovery primitives are sound but not composed into command-level resilience.

---

## 15. Security Assessment — **6/10**

Threat model is correct (local CLI; parses untrusted repo/agent-supplied `.dlog`/source; no authn expected). No RCE, no memory unsafety, no command execution, no network. Confirmed issues:

- **DoS via panics** on committed/shared input (the dominant issue) — including killing the shared MCP server.
- **Write-side symlink escape**: the read path canonicalizes and rejects escapes, but the *write* path does not — a checked-in symlink under `.decisions/` writes `.dlog`/`.dmap` outside the repo and can clobber a same-named `.dmap` in the target. A clean bypass of the project's own advertised symlink control (asymmetric: `.dlog` clobber is gated by the parse step, `.dmap` is not).

Read-path path-validation hardening is genuinely strong (traversal, drive/UNC/device, reserved Windows names, trailing dot/space all rejected; verified). The git zlib path bounds delta depth (though the cap of 32 is *below* git's default 50 — a correctness bug, not a security one).

---

## 16. Testing Assessment — **6/10**

Volume is high and the differential-against-TS strategy is the right idea. But the confidence is narrower than the count implies:

- **The differential oracle covers only TS/JS.** The Rust and C/C++ extractors — the largest *novel* code the v2 effort added — have **no independent oracle**; correctness rests on same-team unit tests. A wrong range in a `.rs` file is silently accepted (verified: a decision with a deliberately wrong line range passes `lint`).
- **No fuzzing of the hand-written parsers** — the exact gap that let four panics survive into a release candidate.
- **No concurrency/crash-injection tests** — atomicity and locking verdicts rest on code reading.
- **The scale harness exercises a best case** (tiny files, 256 KiB skip, 1 decision/file).
- **The strongest suites (property soak, full scale, corpus matrix) run only weekly/on-dispatch, not on PRs.**
- **Cross-platform (Windows/macOS/arm/musl) is CI-only and unverified here**; the Windows crash-consistency path is a different (weaker) code path that is not crash-tested.

---

## 17. Documentation Assessment — **8/10**

README, spec, and architecture docs are clear, honest, and unusually well-written (the competitive-positioning table is fair; `docs/archiva-v2-review-status.md` candidly lists remaining gaps). Two doc-vs-reality drifts matter: the quick-start's auto-wired hook doesn't work as documented, and the "scales to large repos" claim is not borne out. Otherwise documentation is a strength.

---

## 18. Developer Experience Assessment — **7/10**

Onboarding is smooth and the CLI is pleasant and consistent. DX is undercut by: the broken auto-hook surfacing a recurring error on every edit, the absence of any diagnostic/verbose mode, confusing error messages with no file path on corrupt-store scans (`file: missing required field` reads as a sentence, not a schema field), and the silent data-loss footguns (re-decide without supersede). The bones are good; the failure-mode UX needs work.

---

## 19. Operational Readiness Assessment — **4/10**

The single biggest operational gap is **observability: there is none** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery. For an unattended agent hook, any non-crash misbehavior currently requires recompiling with instrumentation to investigate. Combined with the panic class and the no-path corrupt-file errors, field diagnosability is poor.

---

## 20. Production Readiness Assessment — **5/10**

Not production-ready for general/public use today. It is usable in a controlled, single-user, small-repo, all-committed-files setting where the panic triggers and perf cliffs are unlikely. It is not ready for team use over a shared `.decisions/` tree (the panic propagation vector) or for large repos (the perf cliffs) or for the documented Claude Code auto-workflow (the hook contract).

---

## 21. Open Source Readiness Assessment — **7/10**

Strong: MIT license, clean repo, clippy-strict, formatted, CONTRIBUTING-grade docs, a real CI/validation/publish pipeline, and a thoughtful architecture doc with explicit extension points. Adoption-readiness is gated almost entirely by the production-readiness blockers above plus the missing panic-safety and fuzz gates that an open-source contributor base would expect before trusting it with their decision history. Packaging has one sharp edge: `import.meta.dirname` in the install tooling hard-requires Node ≥ 20.11 while npm doesn't enforce `engines`, so older-Node users get a cryptic postinstall crash.

---

## 22. Prioritized Findings

**Severity distribution (adversarially verified):** 0 critical *as labeled* / 17 high / 27 medium / 44 low / 9 info. 96 CONFIRMED, 1 PLAUSIBLE, **0 REFUTED**. The completeness critic argues — and I concur — that the *consolidated* "panic on committed/shared input" theme meets a **critical / release-blocking** bar even though no single line-item was labeled critical.

### Release blockers (must fix before 1.0)

| # | Finding | Location | Why it blocks |
|---|---|---|---|
| B1 | **Panic/abort class on committed input** (lone `'` → `yaml.rs:700`; mid-codepoint slice → `yaml.rs:310`; empty block scalar; deep-nesting recursion → `anchor.rs:733`) | `yaml.rs`, `anchor.rs` | One malformed byte in a shared `.dlog`/source aborts `status`/`lint`/`session-start` and **kills the MCP server**. It's a class — must be closed by fuzzing + depth bounds + a panic boundary, not point patches. |
| B2 | **PostToolUse hook is a no-op under Claude Code** (ignores stdin `file_path`) | `settings.rs:5`, `main.rs:46-51`, `cli.rs:234-258` | The core advertised automation never runs in the documented environment. |
| B3 | **Re-anchor falsely marks STALE + non-idempotent line drift** (no HEAD baseline / multiple edits per commit; ground-truth range discarded) | `project.rs:271-304`, `diff.rs:17-44` | Silent corruption of the authoritative store under normal agent activity; breaks line-based `why`. |
| B4 | **O(n²) anchor extraction** (per-file prefix re-scan) | `anchor.rs:6678`, `1729/1760` | Tens of seconds–minutes on a single large file; hooks read as hangs. |
| B5 | **One corrupt `.dlog` aborts the whole repo command** + **errors carry no file path** | `project.rs:76-82/402-408/133-149`, `error.rs:88-110` | Localized corruption blinds the whole repo; operators can't locate the bad file. |

### High (fix before or immediately after 1.0)

B6 non-atomic `.dlog`/`.dmap` write (false-failure + retry id-churn, `storage.rs:256-258`) · B7 write-side symlink escape (`paths.rs`/`storage.rs`) · B8 O(n²) hot-file writes + redundant re-parse (`storage.rs:131-133`) · B9 no logging/diagnostics anywhere · B10 Rust/C/C++ extractors have no differential oracle · B11 silent data loss on re-decide without `supersedes` · B12 MCP `why` returns confidently-wrong result for a `line` query.

### Medium (notable)

PID-liveness lock wedge · read-only `status`/`lint` take write locks · MCP tool errors as `-32000` not `isError` · `lint`/`status` exit-code taxonomy · unknown-field drop on rewrite · no schema migration · anchored-gitignore patterns never match descendants · C++ `enum class` phantom anchor · git delta-depth cap 32 < 50 · `.dmap` never read · scale harness blind to both quadratics · `lint --fix` non-atomic · Node-version postinstall crash · case-insensitive-FS identity collisions · heavy validation runs only weekly.

---

## 23. Recommended Roadmap

**Milestone 1 — Panic-safety & robustness (release-blocking, ~2 wks)**

1. Fuzz the YAML/JSON parsers (`cargo-fuzz` or in-repo property soak over arbitrary bytes); fix every panic; add a depth/recursion bound to the anchor extractor and block-scalar paths.
2. Wrap MCP per-request handling in `catch_unwind` → return `isError`; never let one request abort the server.
3. Make whole-repo commands skip-and-report corrupt files (continue, name the file) instead of aborting; attach the file path to all parse/schema/IO errors.
4. **Make "no panic on any `.dlog`/source input" and "MCP server survives a malformed request" hard PR-CI gates.**

**Milestone 2 — Core workflow correctness (release-blocking, ~1–2 wks)**

5. Parse the Claude Code hook stdin JSON (`tool_input.file_path`) in `post-tool-use`; add an integration test feeding the real payload shape.
6. Re-anchor from the extractor's ground-truth position whenever the anchor still resolves; reserve diff-shift for the orphan/incomplete case. Add a regression test asserting two consecutive `post-tool-use` runs leave `lines_hint`/STALE unchanged.
7. Detect re-decide-without-supersede and supersede-into-occupied-anchor; refuse or auto-chain into history.
8. Make `.dlog`+`.dmap` a recoverable transaction (treat a `.dmap` write failure as success-with-warning, since reads self-heal).

**Milestone 3 — Scale & observability (~1–2 wks)**

9. Eliminate the O(n²) extraction (single-pass depth tracking) and the O(n²) write path (drop the `storage.rs:133` re-parse in release; consider per-file decision bounds); add a large-single-file and a dense-single-file perf regression to the PR gate.
10. Add an env-gated stderr diagnostic channel (files scanned/skipped, lock acquire/recover, dmap repair, git-HEAD fallback).
11. Either make `status`/`session-start` actually read `.dmap`, or drop it.

**Milestone 4 — 1.0 hardening (~1–2 wks)**

12. Independent oracle for Rust/C/C++ extraction (cross-check ranges vs tree-sitter or rustc/clang spans) over a real corpus.
13. Define a schema versioning/migration policy; preserve unknown fields on rewrite.
14. Canonicalize the write path (close the symlink escape); add concurrency + crash-injection tests; promote heavy validation into the release gate; fix the lock-wedge backstop, exit-code taxonomy, gitignore anchoring, git delta-depth cap, and Node-version preflight.
15. Run the multi-process lock and differential suites on Windows/macOS CI (not just `cargo test`).

---

## Subsystem Scorecard

| Subsystem / Dimension | Score |
|---|---|
| CLI surface & dispatch | 8/10 |
| MCP stdio JSON-RPC server | 7/10 |
| Anchor extraction engine | 7/10 |
| Native Git object reader | 8/10 |
| Decision logic (validation/supersession/history) | 7/10 |
| Storage / locking / atomicity / recovery | 7/10 |
| Project workflow orchestration | 8/10 |
| Serialization (JSON/YAML/dlog/dmap) | 6/10 |
| Path validation & portability | 7/10 |
| Diff / reanchor / line-shifting | 7/10 |
| Security (cross-cutting) | 6/10 |
| Performance & scalability | 6/10 |
| Reliability / recovery / observability | 7/10 |
| Testing strategy & confidence | 7/10 |
| Release engineering & packaging | 8/10 |
| API/CLI/behavioral consistency & DX | 8/10 |
| Overall architecture & maintainability | 8/10 |

*(Per-subsystem scores reflect each module's intrinsic quality. The system-level scores below are lower because the dominant failures are cross-cutting — they emerge from interactions, e.g. parser-panic × shared-committed-store × long-lived-server.)*

### Overall scores

- **Engineering quality (unit level): 8/10** — clean, disciplined, well-tested-in-volume, idiomatic, zero-warning.
- **Production readiness: 5/10** — concentrated, reproducible blockers; safe only in a controlled single-user setting today.
- **Validation confidence: 6/10** — broad and partly differential, but blind to the exact failure classes that bite (no fuzz, no scale realism, no Rust/C oracle, no concurrency/crash injection, no real-hook integration); heavy suites are off the PR path.

---

## Final Questions — Answered Explicitly

**Does the implementation fully achieve its stated goals?**
No. It achieves the *static* goal (a fast, repo-native, zero-dependency decision store with a working CLI and MCP surface) but not the *dynamic* goal: the automatic agent workflow it is built around (auto re-anchor on edit) is broken end-to-end under real Claude Code and corrupts line attribution even when invoked correctly, and the "scales to large repos" claim is not substantiated.

**Is the architecture appropriate for long-term evolution?**
Mostly yes. Module boundaries, ownership, and extension points are sound. The two evolution gaps — no schema migration path and the zero-dependency stance concentrating un-fuzzed parser risk — are addressable without restructuring.

**Is the implementation internally consistent?**
Largely, with confirmed exceptions: CLI vs MCP `why` line semantics, MCP error encoding vs spec, and exit-code conventions. These are localized.

**Would you approve this for a stable public release?**
No. The panic class on committed/shared input, the broken auto-hook, the re-anchor corruption, and the absence of observability are release-blocking for a stable 1.0.

**Would you approve it as the reference implementation?**
Not yet. A reference implementation must be panic-safe against its own committed data format and must have the core workflow proven end-to-end. Once Milestones 1–2 land and panic-safety + real-hook integration are CI gates, it is a credible reference candidate.

**Highest-priority improvements before release?**
(1) Fuzz and bound the parsers + add a panic boundary + skip-and-report corrupt files; (2) fix the PostToolUse stdin contract; (3) make re-anchoring idempotent from ground-truth positions; (4) fix the O(n²) extraction/write paths; (5) add a diagnostic logging channel. In that order.

**What work remains before 1.0?**
Milestones 1–4 above. Realistically 4–8 focused weeks. The critical path is panic-safety + workflow correctness (M1–M2).

**What risks remain after release (assuming blockers fixed)?**
Cross-platform behavior (Windows/macOS/arm/musl) remains CI-only and the Windows crash-consistency path is weaker; the Rust/C/C++ extractors stay oracle-light until an independent ground truth exists; the hand-rolled git/DEFLATE code is a maintenance hotspot that needs an ongoing differential-against-`git` fuzz harness; and concurrency/crash-consistency guarantees rest on reasoning until fault-injection tests exist.

**How does engineering quality compare with mature, well-regarded OSS in the same domain?**
At the *craft* level (code cleanliness, lint discipline, documentation, the differential-testing instinct) it compares favorably with well-run early-stage OSS and exceeds many. At the *production-hardening* level it is behind mature tools like ripgrep, gitoxide/`gix`, or tree-sitter, which earned trust through extensive fuzzing, fault injection, and battle-tested robustness against adversarial input — exactly the layer Archiva has not yet built. It has the bones of a top-tier project and an unusually honest self-assessment; it has not yet done the hardening that separates a strong prototype from a definitive reference.

---
---

# APPENDIX — Full Verified Findings

Every finding below survived independent adversarial verification (reproduction against the release binary or direct code read). `verdict` is the second agent's call; `severity` is the corrected severity. Findings are grouped by subsystem/dimension, then sorted high → info.

, why prints `Chose: trailing`).
2. Whitespace-only chose="   ": exit 1, stderr `decisions.fn:alpha.chose: expected string`, NO dlog written (ls .decisions/ empty).
3. MCP path identical: drove `archiva mcp` over stdio JSON-RPC; write_decision with chose="   " returned {"error":{"code":-32000,"message":"decisions.fn:alpha.chose: expected string"}}. Confirms both CLI and MCP go through write_decision_record_locked -> write_dlog (storage.rs:256).

SUPPORTING CLAIMS VERIFIED:
- Property test gap real: src/core/property_tests.rs:401-403 random_yaml_edge_char alphabet (first/last char) excludes space; random_yaml_inner_char (line 376) includes it. So the round-trip property test never places whitespace at a string boundary — the exact untested gap.
- js-yaml comparison real: `node -e` with js-yaml dump+load of "   ", " leading", "trailing " all single-quote and round-trip true. This is a genuine port regression vs the TS original (src/core/dlog.ts:2,66 uses js-yaml.dump).

**Verifier notes / severity correction:** CONFIRMED as written, including severity (high) and both locations (yaml.rs:963 needs_single_quotes, storage.rs:131-134 write_dlog guard). All claimed evidence reproduced verbatim, including the exact error string and the property-test exclusion.

Two small, non-material refinements to the prior auditor's framing:
1. The `lines` field is a 2-element array [start,end], not an object — the prior repro's JSON sketch elided this, but it does not affect the finding; once corrected the repro reproduces exactly.
2. Mechanism nuance for the whitespace-only failure: it is not that the rendered scalar re-parses as a "non-string number/bool"; rather the trailing-whitespace value after `chose: ` is trimmed to empty, the parser then treats `chose:` as a parent key with a nested block, yielding a non-string that trips dlog.rs:335 expect_non_empty_string -> "expected string". The observable behavior (write aborted by the storage.rs:133 round-trip guard, confusing internal error) is exactly as claimed.

Scope confirmed: affects all free-text fields validated by expect_non_empty_string/optional_non_empty_string (chose, because, expires_if, session) plus any field rendered via render_scalar; reaches both CLI write-decision and MCP write_decision. Severity high is justified: silent corruption of decision rationale directly undermines the trust model of a decision-memory tool, and the whitespace-only path surfaces an internal error to users with no actionable message.

Recommended resolution: extend needs_single_quotes to also return true when value != value.trim() (leading/trailing whitespace) or value.trim().is_empty() (whitespace-only nonempty). Optionally add an edge-case to the round-trip property test placing space at boundary positions.

**Recommended resolution:** Extend needs_single_quotes to also return true when value != value.trim() (i.e. has leading/trailing ASCII whitespace) or is non-empty but all-whitespace. Add a property/round-trip test that includes leading/trailing/only-space strings to lock parity with js-yaml.

---

### F27. [MEDIUM] Re-deciding an existing anchor without `supersedes` silently destroys the prior decision (no history, no warning)

`defect` · location `src/core/storage.rs:250-255 (apply_decision_record) and src/core/decision.rs:126-138; mirrors TS src/core/decision.ts:49` · reporter-confidence high · verification **CONFIRMED**

**Description:** write-decision inserts the new record at input.anchor via OrderedMap insert, overwriting any existing record at that anchor. When supersedes is omitted, history starts empty (storage.rs:242-245), so the previous decision's id, chose, because, timestamp, and its entire prior history chain are discarded with no warning and no trace. The only safe way to replace a decision is to pass supersedes:<old id>, but nothing requires or hints at this.

**Why it matters:** The product is 'decision memory.' Overwriting an anchor is the most common way to update a decision, and the natural call (same anchor, new chose/because, no supersedes) is exactly the lossy path.

**Impact:** Permanent loss of decision history for the anchor. why/history show only the latest record as if no prior decision ever existed.

**Likelihood:** High — re-recording at the same anchor is the obvious update gesture.

**Evidence (reporter):** Reproduced in /tmp/arc4: wrote dec_001 'first decision' at fn:alpha, then wrote 'second decision' at fn:alpha with no supersedes. Resulting dlog has only dec_002 with `history: []`; `archiva history src/n.ts fn:alpha` shows only dec_002.

**Independent verification:** Code path read end-to-end and reproduced live with the release binary.

CODE: write_decision_record_locked (src/core/storage.rs:229-261) computes history only from the supersede plan: `let history = supersede.as_ref().map(|plan| plan.history.clone()).unwrap_or_default();` (242-245). prepare_supersede (src/core/decision.rs:65-94) returns Ok(None) immediately when `supersedes` is absent (70-72), so history is empty. apply_decision_record (src/core/decision.rs:126-138) then calls `dlog.decisions.insert(anchor, decision)` (137), and OrderedMap::insert (src/core/ordered_map.rs:30-40) overwrites the value in place when the key already exists (`*existing = value; return;`). superseded_anchor is None, so the guarded remove_str branch (132-136) does not run, and the prior record is simply replaced. No warning/log/error exists on overwrite (grep for warn/overwrite/"already exists" in storage.rs/decision.rs/write CLI found only lock-file messages). This faithfully mirrors TS src/core/decision.ts:49 (`dlog.decisions[input.anchor] = decision;`).

LIVE REPRO in /tmp/arcverify (git init + `archiva init`, file src/n.ts):
1. write-decision {anchor:"fn:alpha", chose:"first decision", because:"reason one"} -> "Recorded dec_001."; dlog shows dec_001 with `history: []`.
2. write-decision {anchor:"fn:alpha", chose:"second decision", because:"reason two"} (NO supersedes) -> "Recorded dec_002." Resulting dlog contains ONLY fn:alpha -> dec_002 with `history: []`. dec_001's id/chose/because/timestamp are entirely gone.
3. `archiva history src/n.ts fn:alpha` -> shows only "dec_002 ... Chose: second decision". `archiva why src/n.ts fn:alpha` -> only dec_002. dec_001 left no trace.
4. Control: writing dec_003 WITH supersedes:"dec_002" correctly preserved the chain — `history` then showed dec_002 followed by dec_003. So supersedes is indeed the only path that retains history.

(Note: the claim's example used `lines:{start,end}` object form; actual schema requires `lines:[start,end]` array and a `rejected` field, but this is incidental to the finding — the overwrite behavior reproduces identically.)

**Verifier notes / severity correction:** Claim is accurate in mechanism, cited locations (storage.rs:242-255, decision.rs:126-138, OrderedMap::insert), evidence, and impact. Re-deciding an existing anchor without `supersedes` permanently and silently discards the prior decision's id/chose/because/timestamp and its entire history chain; why/history then render as if no prior decision existed. No warning, prompt, or trace. The only safe replacement path is `supersedes:<old id>`, which nothing requires or hints at, and the old id is itself destroyed by the overwrite (so after an accidental clobber the user cannot even recover the id to supersede).

Severity medium is correct and not inflated: it is silent, permanent, user-data loss, but it requires the same anchor to be re-used without supersedes (an agent/caller mistake rather than an always-on bug), and it is a deliberate behavioral parity with the TS original. Not a regression vs. TS.

Recommended resolution: in write_decision_record_locked, detect an existing record at input.anchor when `supersedes` is absent and either (a) error out instructing the caller to pass supersedes (consistent with the existing "Call why first and use the recorded decision id" guidance), or (b) auto-fold the existing record into history (treat same-anchor re-decide as an implicit supersede of the resident record). Option (a) is the safer, more explicit choice; if TS parity must be preserved, at minimum emit a warning to stderr on silent overwrite. Confidence: high.

**Recommended resolution:** Behavior is faithful to TS, so treat as a known parity item — but at minimum detect an existing record at the target anchor and, when supersedes was not provided, either auto-chain it into history or return an error directing the caller to pass supersedes. Document the overwrite semantics.

---

### F28. [MEDIUM] Superseding into an anchor already occupied by a DIFFERENT live decision silently deletes that unrelated decision

`defect` · location `src/core/decision.rs:126-138 (apply_decision_record) + storage.rs:250-255` · reporter-confidence high · verification **CONFIRMED**

**Description:** prepare_supersede only moves/deletes the OLD superseded anchor (decision.rs:132-136 removes superseded_anchor when it differs from the new anchor). The new record is then inserted at input.anchor (decision.rs:137). If input.anchor already holds a third, unrelated live decision, that decision is overwritten and lost — its rejected/history/rationale gone — even though it has nothing to do with the supersession.

**Why it matters:** Supersession is supposed to be a controlled history operation; instead it can collaterally erase an unrelated decision that happened to share the destination anchor.

**Impact:** Silent loss of a live, unrelated decision and its full history during a supersede that names a different decision id.

**Likelihood:** Low-medium — requires writing the superseding decision at an anchor that already has its own decision, but anchors collide naturally when functions are merged/renamed.

**Evidence (reporter):** Reproduced in /tmp/arc3: dec_001 at fn:alpha, dec_002 'beta choice IMPORTANT' at fn:beta. Then wrote a new decision at fn:beta with supersedes:dec_001. Final dlog contains only dec_003 at fn:beta; dec_002 and its content are gone, and dec_003.history contains only dec_001 (the superseded one), never dec_002.

**Independent verification:** Reproduced end-to-end with the release binary in /tmp/arcverify (git-init + `archiva init`, source file src.rs with fn alpha/beta/gamma).

Steps and actual output:
1. `write-decision` anchor=fn:alpha lines=[1,3] -> "Recorded dec_001."
2. `write-decision` anchor=fn:beta lines=[5,7] chose="beta choice IMPORTANT" rejected=[{approach:gamma-approach,reason:too slow}] -> "Recorded dec_002." dlog then contains BOTH fn:alpha(dec_001) and fn:beta(dec_002).
3. `write-decision` anchor=fn:beta supersedes=dec_001 -> "Recorded dec_003."

Resulting .decisions/src.rs.dlog contains ONLY fn:beta -> dec_003. dec_002 ("beta choice IMPORTANT", its rejected alternative, rationale, timestamp) is gone. dec_003.history holds only dec_001 (the superseded one) — dec_002 was never folded into history. `grep -rn "dec_002|beta choice IMPORTANT" .decisions/` returns nothing; `archiva history src.rs dec_002` -> "No decision found". The .dmap also shows only `5-7:fn:beta`, so the index has no trace either. Exit code 0, no warning.

Code path confirms the mechanism:
- storage.rs:241-255 (write_decision_record_locked): prepare_supersede returns the OLD anchor (fn:alpha) as superseded_anchor; the new record is then inserted at input.anchor (fn:beta).
- decision.rs:126-138 (apply_decision_record): only removes the superseded_anchor when it differs from the new anchor; then unconditionally `dlog.decisions.insert(anchor, decision)` at the new anchor.
- ordered_map.rs:30-40 (insert): if the key already exists it overwrites in place (`*existing = value`). So inserting at fn:beta clobbers dec_002 with no history merge and no error.

Scope note (verified separately in /tmp/arcverify2): a plain re-write at an already-occupied anchor (no supersedes) ALSO overwrites the prior decision with no history preservation — dec_001 -> dec_002 at same anchor, dec_001 content lost. That is the same insert-overwrite path. So the underlying defect (insert silently clobbers any live decision at the target anchor, dropping its history) is broader than just supersede; the supersede variant is the most dangerous because the destroyed decision (dec_002 at fn:beta) is unrelated to the named superseded id (dec_001).

**Verifier notes / severity correction:** CONFIRMED as described; location and medium severity are accurate. Two refinements: (1) The root cause is in apply_decision_record (decision.rs:126-138) relying on OrderedMap::insert (ordered_map.rs:30-40), which overwrites any existing entry at the target anchor with no merge/guard. (2) The defect is slightly broader than the claim states: even a NON-supersede write to an already-occupied anchor silently destroys the prior decision and its history via the same path. The supersede case is the worst manifestation because the collaterally-destroyed decision is unrelated to the id the caller named, making the data loss completely unexpected. Recommended fix: in apply_decision_record, before inserting, detect whether input.anchor already holds a different live decision; either reject the write with a CLI error (preferred, since silent overwrite of a live decision is never desired) or fold the displaced decision into history. Likelihood is moderate in real use — anchors collide whenever two decisions target the same code symbol, and superseding across anchors is a normal workflow.

**Recommended resolution:** Before insert, detect collision with an existing live decision at the destination anchor that is not the one being superseded, and refuse (or fold it into history). Add a regression test for supersede-into-occupied-anchor.

---

### F29. [LOW] `lint` detects orphans but never persists ORPHAN status, so `status` reports 0 orphans for orphaned decisions

`defect` · location `src/core/project.rs:482-502 (lint_dlog orphan branch) vs project.rs:306-312 (post_tool_use mark_orphan) and status.rs:42-58` · reporter-confidence high · verification **CONFIRMED**

**Description:** lint_dlog pushes an arc/orphan warning and, only under --fix, removes the anchor (project.rs:496-499). It never calls mark_orphan, so the decision's status stays None (or whatever it was). status() counts ORPHAN via status_summary_from_dlog (status.rs:51-55) which reads decision.status. Because lint never sets status=ORPHAN, `status` shows '0 orphan' for a decision that lint simultaneously reports as an orphan issue. Only the post-tool-use hook persists ORPHAN. The two orphan-detecting code paths thus disagree about persisted state.

**Why it matters:** status is the headline health command; an orphaned decision invisible in the orphan column (while counted as a generic issue) is misleading and inconsistent with the post-tool-use path that does persist ORPHAN.

**Impact:** Under-reported orphan counts in `status` after a plain `lint`; user sees an issue count but a 0 in the orphan column.

**Likelihood:** Medium — any anchor rename/removal followed by `status` (without running post-tool-use) shows this.

**Evidence (reporter):** /tmp/arc2: removed fn:alpha's anchor; `archiva status` => '0 stale 0 orphan ... 1 issues'; `archiva lint` => 'WARNING arc/orphan ... fn:alpha no longer exists'; dlog status field never set. Contrast /tmp/arc11 post-tool-use which writes 'status: ORPHAN' and status then shows '1 orphan'. Note: this is faithful to the TS reference (status.ts + rules.ts also never persist ORPHAN in lint), so it is a parity-preserving design inconsistency rather than a Rust-only regression.

**Independent verification:** CODE: Confirmed all three cited locations.

1. lint_dlog orphan branch (src/core/project.rs:482-502): when an anchor no longer exists in source, it pushes a LintIssue{rule: Orphan, ...} and, ONLY under `if fix`, pushes the anchor onto remove_anchors for deletion (lines 496-499). It never calls mark_orphan and never sets decision.status. By contrast, the stale branch (lines 511-524) DOES persist state: it calls mark_stale_now(decision) and sets changed=true on first detection. So lint persists STALE but not ORPHAN — exactly the asymmetry the claim describes.

2. post_tool_use hook (src/core/project.rs:306-312): the only path that calls mark_orphan(decision) (and writes the dlog at line 328). decision_status.rs:29-30 confirms mark_orphan sets status = Some(DecisionStatus::Orphan).

3. status path (src/core/project.rs:87-91 -> 399-409 -> status.rs:42-58): status() builds summaries via status_summary_from_dlog, which counts orphan by matching decision.status == Some(DecisionStatus::Orphan) (status.rs:51-55). Since lint never sets that field, the orphan column reads 0. issue_count is computed separately via lint_project_issue_count (project.rs:89), so the issue total still reflects the orphan warning.

RUNTIME (binary /home/ubuntu/archaeo/target/release/archiva in /tmp/arc_orphan): inited project, wrote fn:alpha + fn:beta decisions, then removed fn:alpha from src/app.ts.
- `status` (pre-lint): "src/app.ts  2 decisions  0 stale  0 orphan" / "Total: 2 decisions 0 stale 0 orphan  2 issues"
- `lint`: "WARNING arc/orphan src/app.ts fn:alpha: fn:alpha no longer exists in src/app.ts" (plus stale/supersede for beta whose body I also touched)
- `status` (post plain lint): "src/app.ts  2 decisions  1 stale  0 orphan" / "Total: 2 decisions 1 stale 0 orphan  3 issues"
- dlog after lint contains "status: STALE" for beta but NO status field for alpha — proving lint persisted STALE but not ORPHAN. The orphan column stayed 0 while lint simultaneously reported alpha as an orphan.

TS PARITY: Confirmed the claim's parity note. src/lint/rules.ts:40-55 only `delete dlog.decisions[anchor]` under options.fix in the orphan branch (no markOrphan), and src/cli/status.ts:18 counts orphan via decision.status === "ORPHAN". The Rust behavior is a faithful port of the TS reference, not a Rust-only regression.

**Verifier notes / severity correction:** The claim is accurate in every particular — location, mechanism, the STALE-vs-ORPHAN asymmetry, the runtime symptom, and the TS-parity framing all hold. Severity "low" is correct: the issue COUNT still increments (status showed "2 issues"/"3 issues"), so the user is signaled that something is wrong; only the orphan column under-reports. It is a parity-preserving design inconsistency rather than a defect that hides problems outright. One minor refinement to the claim's reproduction narrative: in my repro the orphaned decision (alpha) had no prior status, and a second decision (beta) became stale because I edited its body — that is incidental to the orphan point. A cleaner repro would orphan a decision while leaving all other anchors untouched, yielding status "...0 orphan ... 1 issues" with the lone orphan warning, matching the claim's /tmp/arc2 description exactly. The core defect — lint never persisting ORPHAN, so status's orphan column reads 0 after a plain lint — is solidly confirmed.

**Recommended resolution:** Either have lint_dlog mark_orphan (and persist) like post_tool_use does, or document that orphan status is only materialized by the hook. Align the two paths so `status` and `lint` agree.

---

### F30. [LOW] No validation that recorded `lines` fall within the source file; out-of-range ranges create a permanent non-staleable 'zombie' decision

`defect` · location `src/core/decision.rs:318-337 (parse_lines) and decision.rs:104-109 (build_decision_record); fingerprint.rs:19-26 (get_lines)` · reporter-confidence high · verification **CONFIRMED**

**Description:** parse_lines only validates the [start,end] tuple shape and end>=start. assert_anchor_exists (anchor.rs:347) checks the anchor name exists but does not check that the recorded lines correspond to the anchor's actual span or are within the file. build_decision_record fingerprints get_lines(source,start,end); when the range is entirely past EOF, get_lines returns '' and fingerprint('') = e3b0c442 (the empty-input hash). is_fingerprint_stale then compares '' to the same empty hash forever, so the decision can never drift to STALE no matter how the code changes.

**Why it matters:** A decision recorded against a bogus or stale line range silently becomes immune to drift detection — the exact failure the tool exists to catch — while still appearing in why/status as a healthy decision.

**Impact:** Drift monitoring silently disabled for that decision; why reports a line range that does not match the code.

**Likelihood:** Low-medium — agents can easily pass a wrong/stale line range; there is no feedback that it is wrong.

**Evidence (reporter):** /tmp/arc6: wrote a decision with lines [50,100] for a 3-line file; dlog fingerprint is e3b0c442 (== `echo -n '' | sha256sum` first 8). Editing the function body and running lint => 'No decision issues found.' status => 0 issues. The decision is permanently non-staleable.

**Independent verification:** Code trace matches the claim exactly:
- decision.rs:318-337 parse_lines only validates array-of-2, positive ints, end>=start. No EOF/file-bounds check.
- decision.rs:104,109 build_decision_record computes selected_source = get_lines(source, start, end) then fingerprint(selected_source). No span/bounds validation.
- fingerprint.rs:19-26 get_lines uses skip(start-1).take(...); when start > line count it yields "" .
- fingerprint.rs:12-17 fingerprint("") = sha256("").take(8).
- decision_status.rs:8-14 is_fingerprint_stale recomputes fingerprint(get_lines(...)) and compares to stored fingerprint; for an out-of-range range both sides are always e3b0c442, so it can never be stale.
- anchor.rs:347-351 assert_anchor_exists only checks the anchor name exists, not that lines match its span (project.rs:212 calls it before recording).

Empirical reproduction in /tmp/arcaudit (git-init'd, 3-line app.js with anchor fn:foo):
  archiva write-decision --json '{"file":"app.js","anchor":"fn:foo","lines":[50,100],"chose":"X","because":"Y","rejected":[]}'  => "Recorded dec_001."
  .decisions/*.dlog shows lines_hint [50,100] and fingerprint: e3b0c442
  printf '' | sha256sum | cut -c1-8  => e3b0c442  (confirms empty-input hash)
Then rewrote foo's body completely (return 999999 + extra line) and ran:
  archiva lint   => "No decision issues found."
  archiva status => "app.js 1 decisions 0 stale 0 orphan ... 0 issues"
  archiva why app.js => "fn:foo dec_001 (lines 50-100) ..." (reports a 50-100 range that does not exist in the 4-line file)
The decision is permanently non-staleable and why reports a bogus line range. (Note: the claim's evidence said "3-line file"; the anchor must still resolve, which it does via the anchor name, independent of the bogus line range.)

**Verifier notes / severity correction:** Claim is accurate in mechanism, location, and observable behavior; severity low is appropriate and confirmed. It is a real verified defect, not a style issue: there is no validation anywhere that lines_hint falls within the source file or corresponds to the anchor's actual span, so a range entirely past EOF fingerprints the empty string and is forever non-staleable, while why prints a nonsensical line range with no lint/status warning. Scope correction/nuance: (1) The bug is not specific to fully-past-EOF only — any recorded range that does not match the anchor's true span produces a fingerprint decoupled from the anchor body; the past-EOF case is the worst form because the fingerprint locks to the empty-string constant. (2) Impact is bounded by requiring erroneous/fabricated input — correctly recorded decisions (lines matching the anchor) are unaffected — which is why low (not medium) is justified, even though it silently defeats the tool's core drift-detection promise for the affected decision. Recommended resolution: in parse_lines or build_decision_record, validate that lines_hint.end (or at least .start) is <= the source line count, and ideally that the range overlaps the resolved anchor's span; reject out-of-range ranges as a schema/cli error, or at minimum flag the decision as orphan/invalid when get_lines returns empty for a non-empty source.

**Recommended resolution:** Validate that lines.end <= source line count (and ideally that the range overlaps the resolved anchor span) at write time, returning a schema error otherwise. At minimum, treat an empty selected-source slice as an error rather than fingerprinting the empty string.

---

### F31. [LOW] `status` and `lint` (read-intent commands) mutate decision files as a side effect

`operational` · location `src/core/project.rs:87-91 (status -> lint_project_issue_count) and project.rs:459-547 (lint_dlog writes dlog/dmap)` · reporter-confidence high · verification **CONFIRMED**

**Description:** status() calls lint_project_issue_count which runs lint_dlog, which marks STALE / clears recovered status and writes dlog+dmap when changed (project.rs:544-547). So running `status` (no --fix) rewrites .dlog/.dmap and flips status fields. The same command that mutates then reports the PRE-mutation count it computed from load_project_status_summaries, so the printed 'stale' number can disagree with the file it just wrote.

**Why it matters:** A command named/expected to be read-only writes to the repo and can produce a report inconsistent with the state it leaves on disk.

**Impact:** Unexpected working-tree churn (dlog/dmap rewrites) on `status`; transiently misleading stale counts. Faithful to TS (status.ts calls lintProject which writes), so parity-preserving, but still an operational surprise.

**Likelihood:** Medium — every `status` run after drift mutates files.

**Evidence (reporter):** /tmp/arc8: drifted source, dlog had no status field; `archiva status` printed '0 stale ... 1 issues' yet afterward the dlog contained 'status: STALE' + stale_since (md5 changed before/after). So status both mutated the file and reported 0 stale for the now-STALE decision.

**Independent verification:** Code path verified by reading source, then reproduced end-to-end with the release binary.

CODE (src/core/project.rs):
- status() at lines 87-91: line 88 `load_project_status_summaries` reads CURRENT (pre-mutation) statuses; line 89 `lint_project_issue_count(project_root, false)` (fix=false); line 90 formats report from the pre-mutation summaries + post-mutation issue_count.
- lint_dlog (lines 459-549): on fingerprint mismatch with `!was_already_stale`, calls `mark_stale_now(decision)` and sets `changed=true` (lines 520-524). Also `clear_recovered_status` sets changed=true (535-537). When `changed`, it writes BOTH files unconditionally of `fix`: `write_dlog` + `write_dmap` (lines 544-547). The `fix` flag only gates orphan anchor REMOVAL (lines 496-499), not STALE marking.
- lint_dlog_locked (412-427) is reached for status via lint_project_issue_count -> lint_project_inner (collect_issues=false) -> lint_dlog_locked, so the same writing lint_dlog runs.

REPRODUCTION (/tmp/arcaudit, drifted app.js, decision fn:login fingerprint e4fb880b):
  BEFORE status: dlog md5 7366883e..., NO `status` field.
  `archiva status` (no --fix) printed: "1 decisions 0 stale 0 orphan ... Total: ... 0 stale ... 1 issues"
  AFTER status: dlog md5 b79e1932... (CHANGED), dlog now contains `status: STALE` and `stale_since: '2026-06-30T20:08:18.433Z'`, and a .dmap was written.
So a single read-intent `status` invocation (a) mutated .dlog AND .dmap on disk, and (b) reported `0 stale` for a decision it was simultaneously flipping to STALE.

Transient-count proof: a SECOND `status` (same source, no further drift) printed "1 stale ... 2 issues" — different output for identical observable source state, and the issue count rose 1->2 because the persisted STALE now triggers the Supersede issue (project.rs:526-533). Second run did NOT re-mutate (dlog/dmap md5 unchanged), confirming the churn is on the first run.

`lint` (no --fix) reproduced identically in /tmp/arcaudit2: BEFORE md5 728fd57d, hasStatus=0; after `lint` md5 d6167085, hasStatus=1 (status: STALE persisted). So both cited read-intent commands mutate.

Added nuance (strengthens the framing): `why` does NOT compute drift live — it only renders the persisted `status` field. In /tmp/arcaudit3, drifted code showed `why` WITHOUT [STALE] before any status/lint, and the dlog stayed hasStatus=0. So the side-effecting write performed by `status`/`lint` is what makes STALE visible to `why`/`history`; the mutation is functionally meaningful, not pure cosmetic churn.

**Verifier notes / severity correction:** CONFIRMED at the claimed severity (low) and location (project.rs:87-91 and 459-547). All three sub-claims reproduced exactly: (1) read-intent `status` and `lint` rewrite .dlog + .dmap as a side effect; (2) the rewrite flips persisted status fields (adds status: STALE + stale_since); (3) the printed stale count disagrees with the file just written (status printed "0 stale" while persisting STALE), because summaries are read pre-mutation (line 88) and issue_count post-mutation (line 89). The transient nature is real: a second run reports the new (post-mutation) numbers (0 stale->1 stale, 1 issue->2 issues).

Severity stays LOW and correctly scoped as operational, not a correctness defect: the final persisted state is correct and idempotent (the decision IS stale), no data loss occurs, supersession/history logic is unaffected, and the count converges after one extra run. The only harms are (a) unexpected working-tree churn (.dlog/.dmap modified) from commands a user reasonably expects to be read-only, which can surface as spurious VCS diffs, and (b) a one-shot misleading stale count on the first run after drift. One correction to the claim's emphasis: this isn't merely cosmetic — because `why`/`history` render only the persisted status and do not compute drift live, the side-effecting write is the mechanism by which staleness becomes visible to those commands. That makes the write functionally load-bearing, but it remains an operational surprise that a read command performs it. Confidence: HIGH.

**Recommended resolution:** Compute lint issue counts without persisting (a dry-run mode), or make status read-only and reserve mutation for lint. If parity with TS must be kept, document that status has write side effects.

---

### F32. [INFO] write-decision accepts whitespace-only free-text fields at validation but they are not meaningful content

`techdebt` · location `src/core/decision.rs:381-387 (expect_non_empty_string)` · reporter-confidence high · verification **CONFIRMED**

**Description:** expect_non_empty_string rejects only the empty string; '   ', '\t' pass as 'non-empty'. This matches the TS zod `.string().min(1)` contract (schemas.ts:13-14, which also checks length not trimmed), so it is faithful — but it means whitespace-only rationale is accepted by the validator and only later fails (or corrupts) at the YAML layer (see the high-severity finding). The validation layer and storage layer disagree about what is a valid string.

**Why it matters:** Defense-in-depth: the validator is the right place to reject meaningless content with a clear field-level message, instead of letting it reach the YAML round-trip and surface as an internal serialization error.

**Impact:** Confusing errors and accepted-but-meaningless content; minor.

**Likelihood:** Low.

**Evidence (reporter):** Code read: decision.rs:383 `JsonValue::String(value) if !value.is_empty()`. schemas.ts:13 `chose: z.string().min(1)`. Behavior confirmed in the high-severity YAML finding reproduction.

**Independent verification:** Code at src/core/decision.rs:381-387 matches the claim verbatim: `JsonValue::String(value) if !value.is_empty() => Ok(...)` — only the empty string is rejected; "   " and "\t\t" satisfy `!value.is_empty()`. Confirmed it is the validator used for chose/because (decision.rs:43-44).

TS reference confirmed: src/core/schemas.ts:13-14 `chose: z.string().min(1)`, `because: z.string().min(1)` — zod .min(1) checks length, not trimmed length, so whitespace-only passes there too. The Rust validator is faithful to the TS contract.

Runtime reproduction (binary /home/ubuntu/archaeo/target/release/archiva in /tmp/archiva_ws_test, git-init'd, src.rs with `fn main(){}`):
  write-decision --json '{"file":"src.rs","anchor":"fn:main","lines":[1,1],"chose":"   ","because":"real reason here","rejected":[]}'
  -> stderr: "decisions.fn:main.chose: expected string"  EXIT 1  (no .dlog written)
Same with because="   " -> "decisions.fn:main.because: expected string".

The JSON validator (expect_non_empty_string) accepted the whitespace-only value — its "expected non-empty string" error never fired. The failure instead surfaced one layer down at the YAML re-parse with the misleading message "expected string". Root cause located: src/core/yaml.rs:963-977 `needs_single_quotes` special-cases only `value.is_empty()`, not whitespace-only, so render_scalar (yaml.rs:918) emits "   " as a bare unquoted scalar; on re-parse the bare scalar is trimmed to empty -> null -> "expected string". The validation layer and storage layer genuinely disagree about what is a valid string, exactly as claimed.

**Verifier notes / severity correction:** Claim CONFIRMED at the stated info/techdebt severity. The cited code, the TS-parity rationale, and the layer-disagreement mechanism are all accurate. One scoping correction: for pure-whitespace fields the write FAILS cleanly (exit 1, no dlog written) with a confusing "expected string" error — I did not observe silent corruption for whitespace-only input. The claim hedges "fails (or corrupts)"; the "corrupts" branch is not exercised by pure whitespace and appears to depend on a separate high-severity YAML finding I was not asked to verify here. The net user-visible impact is exactly what the claim states: accepted-then-rejected with a misleading error pointing at the YAML layer instead of the validator. Recommended fix is to trim/reject whitespace-only in expect_non_empty_string (or have needs_single_quotes treat all-whitespace like empty) so the two layers agree and the error is reported at the validation boundary.

**Recommended resolution:** Optionally trim-and-check (reject all-whitespace) at expect_non_empty_string for a clear field-level error; coordinate with the YAML fix so the two layers agree.

---

## Storage, locking, atomicity, recovery  — score 7/10

> The storage layer stores decisions repo-local under .decisions/ as authoritative .dlog (YAML schema:1) plus a derivative .dmap index, with a per-source-file lock file (`<path>.lock`). Single-file writes use a correct temp+write+fsync+atomic-rename+parent-dir-fsync sequence; I verified via fault injection tests and direct experiments that an interrupted write never truncates the target. The decision-base lock is a create-new lock file guarded by a process-PID liveness check plus a 2-minute wall-clock staleness window, with a secondary `.recover` guard lock that closes the obvious TOCTOU during stale-lock breaking. I stress-tested 120 concurrent writers against one file and observed perfect serialization: 106 committed with unique sequential IDs, 14 cleanly rejected on lock timeout, zero corruption, zero lost-update beyond the rejected set, zero leaked locks/temp files. The architecture is sound overall, but I found three real defects: (1) the write-decision "transaction" is two independent atomic writes (.dlog then .dmap) and is NOT atomic across the pair — a failure between them durably commits the .dlog while returning a non-zero exit, so callers are told the write failed when it actually succeeded; (2) the PID-liveness veto in lock recovery is absolute and trumps the timestamp backstop, so a lock whose recorded PID happens to match ANY live local process (PID reuse, or a lock file propagated via git to another machine) is treated as held forever, wedging all writes to that file with no GC/force-unlock escape hatch; (3) the read-only commands `status` and `lint` (even without `--fix`) unconditionally acquire the per-file write lock to refresh the derivative, so they fail outright on a read-only/permission-restricted .decisions/ and needlessly contend with writers. The .dmap is correctly self-healing-on-read and `why`/`history` read the authoritative .dlog directly, so a stale/corrupt/missing .dmap never yields wrong answers — a genuine strength.

*Score rationale:* The single-file atomic-write and locking primitives are well-engineered and genuinely crash-safe, with strong test coverage including child-process abort fault injection and a TOCTOU-closing recovery guard. The dmap-is-derivative + self-heal-on-read design is a real strength that keeps reads correct regardless of derivative state. Points off for three concrete defects: the write 'transaction' is not atomic across the dlog/dmap pair and can report failure on a durable success (driving agent retries and id churn); the PID-liveness lock veto can wedge a file permanently with no GC/force path and is aggravated by locks being git-trackable by default; and nominally read-only status/lint take write locks and fail on read-only storage. None are data-corrupting under normal local single-host use, but each undermines the reliability guarantees an autonomous-agent tool needs.

**Verified behaviors (checked, not assumed):**

- Built binary at /home/ubuntu/archaeo/target/release/archiva; init creates .decisions/ (empty, no nested .gitignore) and does NOT gitignore .decisions/ by default.
- 120 concurrent write-decision processes against one source file: 106 committed with unique sequential dec_ ids, dmap (106 lines) exactly consistent with dlog, 14 clean lock-timeout rejections, no corruption, no leaked lock/temp files (/tmp/conc2).
- Torn transaction confirmed (/tmp/partial): forcing the .dmap rename to fail (dmap path made a directory) durably committed dec_001 to the .dlog while the command exited 1; retry then wrote dec_002 and overwrote the original anchor.
- PID-liveness veto confirmed (/tmp/pidtest): a lock with pid=1 and a 2000-01-01 timestamp (>26yr stale) was NOT recovered — write waited the full 1.006s and errored; dead-PID+ancient-timestamp recovered in 9ms (/tmp/deadpid); dead-PID+fresh-timestamp correctly waited then errored.
- Read-only .decisions/ (/tmp/rofs2): `status` and `lint` (no --fix) both fail with 'Failed to create lock file ... Permission denied' EXIT=1 even when the dmap is already current, because lint_dlog_locked unconditionally takes the write lock; `why` succeeds read-only by reading the dlog directly.
- Self-heal verified: `status` rewrites a stale/corrupt dmap from the authoritative dlog under lock; `why`/`history` read the dlog directly so a stale dmap never produces wrong output.
- git add .decisions stages a planted a.ts.lock (/tmp/killtest), confirming lock files are git-trackable and can propagate cross-host.
- Atomic write ordering in code (fs.rs:115-147): temp create -> write_all -> sync_all -> atomic rename -> best-effort parent dir fsync, with temp cleanup on error; write_dlog re-parses rendered YAML before persisting (storage.rs:131-135).

### F33. [HIGH] write-decision transaction is not atomic across .dlog and .dmap: a torn write durably commits the decision while reporting failure

`defect` · location `src/core/storage.rs:256-258 (write_decision_record_locked); 178-182 (write_dlog_and_dmap_locked)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Inside the lock, the write performs two independent atomic writes in sequence: write_dlog(...)? then write_dmap(...)?. Each is individually crash-safe (temp+rename), but there is no cross-file atomicity. If the second op (.dmap) fails after the first (.dlog) has already been renamed into place, the authoritative .dlog is durably updated yet the function returns Err and the CLI exits non-zero. The caller is told the decision was NOT recorded when in fact it WAS.

**Why it matters:** This tool is decision memory for autonomous AI agents that branch on the exit code. A false-negative write causes the agent to retry. On retry, next_decision_id recomputes from the now-committed dlog and the same anchor is overwritten with a new id (I observed dec_001 silently replaced by dec_002), or duplicate/divergent reasoning is written. The user/agent's mental model of 'this write failed' is wrong, which is worse than a clean failure.

**Impact:** Silent state divergence between what the agent believes happened and what is on disk; decision-id churn; possible loss of the first decision's reasoning when an anchor is overwritten on retry.

**Likelihood:** Low in normal operation (both writes target sibling files in the same dir, so .dmap rarely fails alone), but reachable on ENOSPC hit between the two syncs, on per-file permission/又state changes, or any condition that lets the dlog write succeed and the dmap write fail.

**Evidence (reporter):** Repro in /tmp/partial: created .decisions/src/a.ts.dmap as a DIRECTORY so the dmap rename fails. Ran write-decision; output: 'Failed to atomically replace file .../a.ts.dmap: Is a directory (os error 21)', EXIT=1. Yet `cat .decisions/src/a.ts.dlog` showed the fully committed dec_001 record. A subsequent retry of the identical write produced 'Recorded dec_002.' and the dlog then contained only dec_002 (dec_001's anchor overwritten).

**Independent verification:** Code: src/core/storage.rs:256-258 in write_decision_record_locked runs `write_dlog(...)?; write_dmap(...)?; lock.release()?;` — two independent atomic (temp+rename) writes with no cross-file transaction or rollback. write_dlog (storage.rs:131-135) and write_dmap (137-142) each call atomic_write_text on the .dlog and .dmap respectively. The same non-atomic pattern exists in write_dlog_and_dmap_locked (172-182). If write_dmap returns Err, the `?` propagates, lock.release() is skipped but FileLock::Drop (fs.rs:256-262) still releases the lock, so the lock does not jam and a retry can proceed.

Live repro with the release binary (/tmp/partial2): initialized a project, created src/a.ts, then made the dmap path a DIRECTORY (`mkdir .decisions/src/a.ts.dmap`) to force the second write's rename to fail. Ran write-decision:
  stderr: "Failed to atomically replace file /tmp/partial2/.decisions/src/a.ts.dmap: Is a directory (os error 21)"; EXIT=1
  Yet `cat .decisions/src/a.ts.dlog` showed dec_001 fully committed (id: dec_001, fingerprint 076fbe61, full chose/because). So the authoritative .dlog was durably written while the CLI reported failure and exited non-zero.

Retry-churn repro: removed the blocking directory and re-ran the identical write. Output: "Recorded dec_002." EXIT=0, and the .dlog now contained ONLY dec_002 — dec_001 was gone. Confirmed in code why: write-decision is not a supersede, and apply_decision_record (decision.rs:127-137) calls dlog.decisions.insert(anchor, decision); OrderedMap::insert (ordered_map.rs:30-40) overwrites the value at an existing key in place. So a retry against the same anchor overwrites the prior (already-on-disk) record with a fresh id and no history entry. If the retry carries different reasoning, the original decision's reasoning is silently lost (not preserved as history, since the supersede path was not taken).

**Verifier notes / severity correction:** CONFIRMED as reported, including both sub-claims (false failure signal + retry id-churn with potential reasoning loss). Both cited locations are accurate: the primary reproduced path is write_decision_record_locked (storage.rs:256-258); write_dlog_and_dmap_locked (172-182) shares the identical non-atomic two-write pattern. Severity high is appropriate: it is a correctness/data-integrity defect in the authoritative store that lies to an autonomous agent about whether a decision was recorded, and the natural retry is non-idempotent (id churn + possible loss of the first decision's reasoning when the anchor is overwritten without a history entry).

Scope/realism nuance worth recording: the trigger is a failure of the SECOND write (the derivative .dmap) AFTER the first (.dlog) succeeded. The directory-collision repro is contrived, but the same torn-write occurs naturally on ENOSPC mid-operation, a read-only/permission change on the .dmap, or a crash between the two renames — all plausible in real operation, so this is not purely synthetic. Note the .dmap is the non-authoritative derivative index, so a stale/missing .dmap is self-healing on the next successful write; the durable harm is the misleading failure exit code plus the non-idempotent retry against the authoritative .dlog. Recommended resolution: make the dlog+dmap update a single transaction — e.g. write both temp files first, then perform the two renames last (rename the derivative .dmap before the authoritative .dlog, or treat a post-dlog .dmap failure as success-with-warning and rebuild .dmap from .dlog), and/or make write-decision idempotent on retry (detect that the just-attempted record already exists for that anchor/fingerprint rather than minting a new id and overwriting).

**Recommended resolution:** Make the pair recoverable rather than ordered-best-effort. Options: (a) treat .dmap purely as a rebuildable cache and never fail the transaction on a .dmap write error — log/warn and return Ok, since reads already self-heal the dmap; or (b) write both temp files, fsync both, then rename .dlog first and rename .dmap second but swallow the second rename error (dmap is derivative); or (c) on dmap-write failure, attempt to roll the dlog back to its pre-write content before returning Err. Given the dmap is already self-healing on read, option (a)/(b) (never let a dmap write failure mark the dlog write as failed) is the smallest correct fix.

---

### F34. [MEDIUM] Lock recovery PID-liveness veto can wedge all writes to a file indefinitely (PID reuse / cross-host lock propagation), with no GC or force-unlock

`defect` · location `src/core/fs.rs:426-443 (lock_is_recoverable), 573-594 (process_is_live)` · reporter-confidence high · verification **CONFIRMED**

**Description:** lock_is_recoverable returns Ok(false) immediately if lock_owner_is_live(lock.pid) is true, BEFORE consulting the timestamp/mtime staleness backstop. process_is_live (Unix kill(pid,0)) only tells you SOME process with that PID exists, not that it is the original Archiva writer. So a lock whose recorded PID coincides with any live local PID is considered held forever regardless of age. There is no lock-GC command, no --force flag, and no max-age override path once a live PID is matched.

**Why it matters:** Two realistic paths to a permanent wedge: (1) the original writer is SIGKILLed leaving a lock file; the OS later reuses that PID for an unrelated long-lived process; every future write to that file is rejected until a human manually deletes the lock. (2) init does NOT gitignore .decisions/ by default and writes no .decisions/.gitignore, so a stray .lock can be committed and pulled onto another machine where its PID maps to a live unrelated process — permanent wedge there too.

**Impact:** Denial of writes to the affected source file's decisions until manual lock-file deletion; for an autonomous agent with no human in the loop, this silently disables decision recording for that file.

**Likelihood:** Low per-incident (requires a leaked lock + PID coincidence, or a committed lock), but the blast radius is total for the affected file and there is no built-in remedy.

**Evidence (reporter):** Repro in /tmp/pidtest: planted lock with pid=1 (init, always live) and timestamp 2000-01-01 (>26 years stale). write-decision waited the full 1.006s retry window and returned 'Archiva lock already exists ... retry later', EXIT=1; the 26-year-old lock was never broken. Contrast /tmp/deadpid: identical ancient timestamp but a dead PID (999999999) recovered in 0.009s. Also confirmed in /tmp/killtest that `git add .decisions` stages a.ts.lock (no nested .gitignore, default init does not ignore .decisions/).

**Independent verification:** Code at src/core/fs.rs:426-443 (lock_is_recoverable) matches the claim exactly: line 428-429 returns Ok(false) immediately when lock_owner_is_live(lock.pid) is true, BEFORE the timestamp staleness check (431) or the mtime backstop (442). process_is_live (fs.rs:577-594, Unix kill(pid,0)) returns true on kill==0 AND on any errno != ESRCH(3) — so EPERM(1), e.g. a pid owned by another user such as pid 1, counts as "live."

Repro 1 (wedge), /tmp/pidtest: inited project, recorded dec_001 against anchor fn:main, then planted .decisions/a.rs.lock with pid=1, timestamp=2000-01-01 (>26y stale). `python3 os.kill(1,0)` => errno 1 EPERM (confirms pid 1 is NOT ESRCH here). Attempting `write-decision` (supersedes dec_001, valid existing anchor) printed "Archiva lock already exists at /tmp/pidtest/.decisions/a.rs.lock ... retry later", EXIT=1, ELAPSED=1.009s (full LOCK_RETRY_TIMEOUT_MILLIS=1000 window), and the 26-year-old lock was NOT removed.

Repro 2 (contrast), /tmp/deadpid: identical ancient timestamp but pid=999999999 (os.kill => errno 3 ESRCH = dead). `write-decision` recorded dec_002, EXIT=0, ELAPSED=0.013s, lock removed. Proves the stale-recovery path works only when the PID is dead; a live-coinciding PID vetoes recovery regardless of age.

No recovery escape hatch: cli.rs:55-75 enumerates all subcommands (init/why/history/hooks/write-decision/status/lint/mcp) — no gc/unlock/force command. grep for force|unlock|gc|--max-age|max_age in cli.rs returns nothing. Constants (fs.rs:12-14): STALE_LOCK_AGE_MILLIS=120000, retry timeout 1000ms, sleep 20ms. Only backstops are timestamp (431) and mtime (442), both unreachable once the live-PID check fires.

Git-propagation vector, /tmp/killtest: default `archiva init` creates NO .gitignore at all (cat .gitignore => No such file; no nested .decisions/.gitignore). `git add .decisions` stages `.decisions/a.rs.lock` (git status: "A  .decisions/a.rs.lock"). A committed lock carries the writer host's pid (std::process::id() at fs.rs:694); on another host (or after PID reuse) that pid number can map to any live local process — and because EPERM counts as live, even a pid owned by root/another user wedges writes.

**Verifier notes / severity correction:** Claim is accurate in mechanism, evidence, and impact; medium severity stands. One correction/strengthening: the prior auditor framed liveness as "SOME process with that PID exists," but it is actually broader — process_is_live treats EPERM (kill returns -1 with errno != ESRCH) as live, so a recorded PID that maps to a process owned by ANY other user (e.g. system pid 1) is considered held, which is exactly why the pid=1 repro wedges. This widens the trigger surface beyond same-user PID reuse. Two mitigating realities that keep it at medium, not high: (1) the lock is created-and-deleted within a single write_decision call (storage.rs:238-258), so a persistent stale lock only arises from a crash mid-write or from committing .decisions while a write is in flight — not the common path; (2) normal locks record a real archiva user-process pid, whose later reuse by a long-lived live process within the 2-minute window is uncommon on a single host. The elevated risk is the cross-host/committed-lock case the claim already calls out. Recommended resolution: do not let a matched-live PID veto the staleness backstop — treat the lock as recoverable when it is older than STALE_LOCK_AGE_MILLIS regardless of PID liveness (PID liveness should only shorten, never extend, the wait), and/or record host identity (hostname/boot-id) so a foreign-host PID is never trusted as live; additionally add a `--force`/lock-GC escape hatch and exclude *.lock from tracking (write a .decisions/.gitignore that ignores *.lock by default).

**Recommended resolution:** Add an absolute mtime/timestamp ceiling that overrides the live-PID veto (e.g. if the lock is older than N minutes by both recorded timestamp AND file mtime, recover it even if some process now holds that PID — the original holder cannot still be mid-write after N minutes). Optionally include a host identifier in lock metadata and ignore the PID liveness check when the recorded host differs from the local host. Ship a `.decisions/.gitignore` (or a settings entry) that excludes `*.lock`/`*.recover` so locks are never committed. Consider an `archiva` lock-clean/force subcommand for operators.

---

### F35. [MEDIUM] Read-only commands `status` and `lint` (no --fix) take the per-file write lock and fail on a read-only/permission-restricted .decisions/

`defect` · location `src/core/project.rs:399-409 (load_project_status_summaries calls ensure_dmap_current), 412-427 (lint_dlog_locked always acquires write lock + ensure_dmap_current_locked), 87-90 (status calls both)` · reporter-confidence high · verification **CONFIRMED**

**Description:** `status` refreshes each file's dmap via ensure_dmap_current (which locks) AND calls lint_project_issue_count -> lint_dlog_locked, which UNCONDITIONALLY acquires the per-file write lock (even when fix=false and even when the dmap is already current) to run lint and call ensure_dmap_current_locked. As a result, ostensibly read-only inspection commands require write access to .decisions/ and a free lock.

**Why it matters:** A user inspecting decisions on a read-only checkout, a mounted-RO snapshot, a CI artifact, or simply a directory where another writer currently holds the lock will get a hard failure from a command that conceptually only reads. It also means `status`/`lint` contend with and can be blocked by active writers, and on a read-only FS they fail completely rather than degrading to a best-effort report.

**Impact:** `status` and `lint` are unusable on read-only filesystems and can intermittently fail under write contention; surprising for read commands.

**Likelihood:** Medium — read-only inspection of decisions (CI, audits, RO mounts) is a normal use case, and the failure is deterministic there.

**Evidence (reporter):** Repro in /tmp/rofs2: with a CURRENT dmap and `chmod -R a-w .decisions`, `archiva status` returned EXIT=1 with stderr 'Failed to create lock file .../a.ts.lock: Permission denied (os error 13)'. `archiva lint` (no --fix) on the same read-only, current-dmap state returned the identical lock-create permission error, EXIT=1. By contrast `why`/`history` (which read the dlog directly without locking) succeeded read-only.

**Independent verification:** Reproduced end-to-end in /tmp/rofs2. Set up: git init; `archiva init`; created a.ts with `fn alpha`; recorded a decision via stdin JSON `{"file":"a.ts","anchor":"fn:alpha","lines":[1,3],"chose":"keep simple","because":"perf","rejected":[]}` -> "Recorded dec_001." Verified dmap is CURRENT: writable `archiva status` (exit 0) and `archiva lint` (exit 0, "No decision issues found.") both ran without modifying .decisions/a.ts.dmap (still `1-3:fn:alpha`).

Then `chmod -R a-w .decisions` (dir dr-xr-xr-x, files r--r--r--, dmap already current) and re-ran:
- `archiva status` -> stderr "Failed to create lock file /tmp/rofs2/.decisions/a.ts.lock: Permission denied (os error 13)", EXIT=1.
- `archiva lint` (no --fix) -> identical "Failed to create lock file ... Permission denied (os error 13)", EXIT=1.
- `archiva why a.ts fn:alpha` -> EXIT=0 (full output). `archiva history a.ts fn:alpha` -> EXIT=0. Both succeeded read-only, exactly as claimed (they read the dlog without locking).

Code path confirmed by reading source:
- status (project.rs:87-91) calls load_project_status_summaries AND lint_project_issue_count(.., fix=false).
- lint_project_issue_count -> lint_project_inner -> lint_dlog_locked (project.rs:412-428) UNCONDITIONALLY calls with_decision_file_lock regardless of fix flag or dmap freshness.
- with_decision_file_lock (storage.rs:184-196) -> acquire_file_lock (fs.rs:271) -> create_lock_file (fs.rs:295) opens lock with create_new(true) in the read-only dir; lock_create_error_is_contention (fs.rs:334-342) returns false because the .lock file does not pre-exist (path_exists==false), so PermissionDenied is surfaced as a hard error, not treated as contention. acquire_file_lock is also fail-fast on real errors (no retry on hard error), supporting the contention-failure point under a held lock (returns existing_lock_error after LOCK_RETRY_TIMEOUT).

**Verifier notes / severity correction:** CONFIRMED at medium. The core finding is accurate and reproducible: lint_dlog_locked (project.rs:412-428) unconditionally takes the per-file write lock even when fix=false and the dmap is already current, so the read-only inspection commands `status` and `lint` require write access to .decisions/ and a free lock, while `why`/`history` work read-only.

One correction to the cited root-cause mechanism (severity unchanged): the claim states `status` fails because "ensure_dmap_current (which locks)" runs. In the reproduced current-dmap state ensure_dmap_current does NOT lock — it early-returns at storage.rs:104 when the on-disk dmap already equals the expected render, taking the lock only when the dmap is stale. So in the current-dmap repro the status failure is caused solely by lint_project_issue_count -> lint_dlog_locked's unconditional write lock, not by ensure_dmap_current. (ensure_dmap_current WOULD independently cause the same failure if the dmap were stale, e.g. after a manual edit — a second, separate trigger.) The claim's location list, evidence, impact, and exit codes are otherwise correct.

Recommended resolution: make read-only paths lock-free. lint_dlog_locked should only acquire the write lock when fix==true (or when a dmap rebuild is actually needed); when fix==false, load the dlog and run lint rules without locking and without calling ensure_dmap_current_locked. status should compute issue counts via that lock-free path. Additionally, ensure_dmap_current's refresh-on-read for status/session-start should degrade gracefully (treat a write/lock failure as non-fatal and serve the existing dmap/dlog) so inspection commands never fail on read-only or write-contended .decisions/.

This is an operational defect (unusable on read-only filesystems / CI artifacts / read-only mounts, and can intermittently fail under concurrent writers) rather than a correctness or data-safety bug; medium is appropriate.

**Recommended resolution:** Don't acquire the write lock for read-only paths. For `lint` without --fix and for `status`, compute results from the loaded dlog in memory; only acquire the lock when actually persisting a dmap repair or a --fix mutation. When the FS is read-only or the lock cannot be taken, skip the dmap refresh and report from the authoritative dlog (the dmap is derivative and reads already tolerate a stale one) instead of failing the whole command.

---

### F36. [LOW] Cross-file move (re-anchor) is a 4-step non-atomic sequence; a crash mid-move can leave decisions duplicated across old and new paths

`architecture` · location `src/core/storage.rs:155-169 (move_dlog_and_dmap_locked)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** Under both file locks the move does: write new .dlog, write new .dmap, remove old .dlog, remove old .dmap. If the process crashes after the new .dlog is written but before the old .dlog is removed, both `.decisions/old.ts.dlog` and `.decisions/new.ts.dlog` exist with the same decisions. The function is guarded by an idempotency check (if new already exists, return it) which makes re-runs safe, but there is no reconciliation that removes a now-orphaned old dlog on the next run.

**Why it matters:** A post-tool-use re-anchor interrupted at the wrong instant leaves a duplicate decision set under the old source path. status/why over the old path would still surface the migrated decisions as if they belonged there, and the old file is an orphan that nothing cleans up.

**Impact:** Stale duplicate decision file under the old path after an interrupted rename; potential confusing `why`/`status` output for the old path until manually removed.

**Likelihood:** Low — requires a crash in a narrow window during the relatively rare move/re-anchor path.

**Evidence (reporter):** Code path src/core/storage.rs:160-167 performs write-new then remove-old as separate fs ops with no journal; the only safety is the leading `if let Some(current) = load_dlog(new_file)` early-return at line 156, which prevents clobbering but does not clean a half-completed prior move.

**Independent verification:** Read src/core/storage.rs:144-170. move_dlog_and_dmap_locked performs exactly the claimed 4-step non-atomic sequence under both locks: load new (early-return if exists, line 156-158), load old, set dlog.file=new, write_dlog (164), write_dmap (165), remove old dlog (166), remove old dmap (167). Each fs op is independently atomic (atomic_write_text -> atomic_write_bytes, src/core/fs.rs), but the sequence has no journal/temp-marker, so a crash after line 164/165 but before 166/167 leaves both paths populated.

No reconciliation exists: searched src/core/project.rs and storage.rs for orphan-dlog cleanup. The only "orphan" logic (lint.rs, project.rs:308) concerns missing source-file ANCHORS, a different concept. status/lint/history all enumerate every *.dlog via list_dlog_files (project.rs:54,357,400) with no dedup across paths, so a duplicate surfaces directly.

Reproduced the half-completed-move state in /tmp/archiva-move-test:
1. init + wrote dec_001 against src/old.ts -> .decisions/src/old.ts.{dlog,dmap}.
2. Simulated crash-after-write-new by copying to .decisions/src/new.ts.{dlog,dmap}, leaving old files (exactly the on-disk state at the crash window).
3. `status` output:
   src/new.ts  1 decisions ...
   src/old.ts  1 decisions ...
   Total: 2 decisions ... 1 issues
4. `why src/old.ts` and `why src/new.ts` BOTH print the same dec_001 (impl A / simplest / B->slower).
5. Re-running the move (post-tool-use src/new.ts) does NOT reconcile: it hits the idempotency early-return path and never removes the orphaned old dlog; old.ts.dlog remains on disk. (In my repro the source file for new.ts was absent so post-tool-use errored before reaching the no-op, but the storage early-return at line 156-158 is unconditional and provably never deletes the old path.)

The leading early-return prevents clobbering on re-run but performs no cleanup of a previously orphaned old dlog — matching the claim verbatim.

**Verifier notes / severity correction:** Claim is correct in mechanism, location (storage.rs:155-169), evidence, and impact. Severity low is appropriate and I concur: it requires a process crash within a narrow window during a cross-file move (an operation only triggered by post-tool-use re-anchoring after a file rename), causes no data loss or corruption, and is fully recoverable by manual deletion of the stale old dlog. status even surfaces it as an issue, so it is observable rather than silent. One scoping nuance: the duplication is visible in why/status/history/lint output for the OLD path until manually removed; the new path is correct. A cheap fix would be a reconciliation step (delete any old.dlog whose contents already exist under the moved-to path) or a temp/journal marker so an interrupted move can be completed/rolled back on next run. Confidence: high.

**Recommended resolution:** On the idempotent early-return (new already present), also best-effort remove any leftover old .dlog/.dmap so a re-run reconciles a half-done move. Alternatively perform the old-file removal first into a temp/journal, or document that interrupted moves require the next post-tool-use to clean up.

---

### F37. [LOW] Stale-lock staleness compares writer-supplied wall-clock timestamps; clock skew on shared storage can break locks early or never

`operational` · location `src/core/fs.rs:563-571 (lock_timestamp_is_expired), 445-468 (lock_file_modified_is_expired), 12 (STALE_LOCK_AGE_MILLIS=120000)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** Staleness is `contender_now_millis - lock_recorded_millis >= 120000`, where both values come from wall-clock SystemTime on whatever host wrote them. The fallback path uses file mtime, also wall-clock. There is no monotonic component. On a shared/network .decisions/ accessed from hosts with skewed clocks (or after an NTP step), a contender with a fast clock could break a lock held <2min, and a contender with a slow clock could refuse to break a genuinely stale lock.

**Why it matters:** The lock's safety window assumes roughly synchronized clocks. On single-host local use this is fine; on shared storage it weakens the 2-minute guarantee in both directions.

**Impact:** Premature lock breaking (two writers proceed) or delayed recovery (writes wedged longer than 2min) under clock skew on shared storage.

**Likelihood:** Low — the documented model is repo-local single-host; shared-storage multi-host use is out of the stated design but not prevented.

**Evidence (reporter):** src/core/fs.rs:563-571 subtracts two parse_utc_millis values with no monotonic clock or per-host normalization; STALE_LOCK_AGE_MILLIS is a fixed 120s. The PID-liveness veto (separate finding) is also host-local, compounding cross-host risk.

**Independent verification:** Code matches the cited locations exactly. src/core/fs.rs:563-571 lock_timestamp_is_expired computes `contender_millis - lock_millis >= STALE_LOCK_AGE_MILLIS` from two parse_utc_millis() values; fs.rs:445-468 lock_file_modified_is_expired (the no-parseable-timestamp fallback) compares contender_millis against file mtime via `modified.duration_since(UNIX_EPOCH)` — also wall-clock; fs.rs:12 STALE_LOCK_AGE_MILLIS = 2*60*1000 = 120000ms. Both the contender timestamp and the recorded lock timestamp originate from wall-clock SystemTime::now(): acquire_file_lock_now (fs.rs:265-268) calls now_utc_millis(); the production write path storage.rs:109 also uses now_utc_millis(). src/core/time.rs confirms now_utc_millis/format_utc_millis are pure SystemTime, no monotonic component. The only monotonic clock in the lock path is Instant::now() at fs.rs:275/288, used solely for the retry-loop timeout, never for staleness.

Runtime reproduction with the release binary in /tmp/lockdemo (after `archiva init` + writing dec_001/002 to create .decisions/src/foo.ts.dlog, lock path .decisions/src/foo.ts.lock):
- Baseline: planted lock pid=999999(dead) timestamp 30s ago -> contender REFUSED ("Archiva lock already exists ... retry later"), lock retained. Confirms staleness gated purely on the recorded timestamp, not real age, and that <120s holds.
- Scenario B (slow-writer skew / premature break): lock physically created NOW but stamped 3 min in the PAST, pid=999999 -> contender BROKE it and recorded dec_003. A genuinely fresh on-disk lock was destroyed because its recorded wall-clock timestamp said 180s>=120s.
- Scenario C (fast-writer skew / delayed recovery): genuinely abandoned lock (dead pid 999999) stamped 5 min in the FUTURE -> contender REFUSED and reported lock held, because contender_now - future_ts < 0, never >=120s. Verified lock_is_recoverable (fs.rs:426-442): a dead-owner lock with a parseable future timestamp returns Ok(false) at line 440, so even same-host PID-liveness does not rescue it; recovery is wedged until future_offset+120s elapses.

**Verifier notes / severity correction:** CONFIRMED as written; location, mechanism, and both failure modes (premature break under a slow/backward clock; delayed-or-never recovery under a fast/forward clock) are accurate and empirically reproduced. Severity low is appropriate and I keep it: the defect only manifests under the conjunction of (a) a shared/network .decisions/ reached from multiple hosts and (b) meaningful clock skew or an NTP step — and this tool's documented design is repo-local single-host, where SystemTime is internally consistent except for a rare backward NTP step. On a single host the only realistic trigger is a backward NTP adjustment producing a future-relative recorded timestamp (Scenario C wedge) or a backward step making an existing lock look aged (Scenario B). Blast radius when it does hit: premature break can let two writers serialize incorrectly on the same .dlog (last-writer-wins / lost decision update, not file corruption, since atomic_write_text preserves write atomicity and a secondary recovery lock exists — though that recovery lock is subject to the identical wall-clock logic and offers no skew protection); delayed recovery wedges writes for up to future_offset+120s. The claim's note that the PID-liveness veto is host-local and compounds cross-host risk is correct: pid 999999 checked against the wrong host's process table is meaningless, so liveness cannot compensate cross-host. No monotonic or per-host-normalized clock exists anywhere in the staleness decision. Reasonable remediation: derive staleness from local file mtime as the primary signal (already the fallback) rather than the writer-supplied timestamp, and/or include a host identifier so the PID-liveness veto is only trusted when lock host == contender host.

**Recommended resolution:** Document that .decisions/ must be local single-host storage, or add host-id awareness so cross-host locks fall back to mtime-only with a generous ceiling. For single-host correctness the current scheme is adequate; the fix is primarily documentation plus the absolute-age ceiling recommended in the PID-veto finding.

---

### F38. [INFO] Atomic single-file write and concurrent serialization are correct and crash-safe (verified)

`operational` · location `src/core/fs.rs:115-147 (atomic_write_bytes_impl), 295-332 (create_lock_file), 344-364 (recover_stale_lock)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Positive verification, not a defect. The atomic write does temp-create -> write_all -> sync_all -> atomic rename (MoveFileExW with WRITE_THROUGH on Windows, fs::rename elsewhere) -> best-effort parent-dir fsync, with temp cleanup on any error. write_dlog re-parses its own rendered YAML before writing, so it never persists unparseable dlog. The lock uses O_CREAT|O_EXCL, classifies Windows ERROR_ACCESS_DENIED(5) and Unix EACCES-on-existing as contention, and the stale-recovery path acquires a separate `.recover` guard lock then RE-READS and RE-VALIDATES the target lock before deleting it, closing the check-then-act TOCTOU.

**Why it matters:** These are the core crash-consistency and concurrency guarantees; confirming them is essential to the audit and they hold.

**Impact:** None — confirms intended behavior.

**Likelihood:** n/a

**Evidence (reporter):** 120 parallel writers to one file (/tmp/conc2): 106 'Recorded' with 106 unique sequential dec_ ids and a 106-line dmap exactly matching the dlog; 14 clean lock-timeout rejections; zero corruption; `find` showed no leaked .lock/.recover/.archiva-tmp files. Dead-PID+ancient-timestamp lock recovered in 9ms; dead-PID+fresh-timestamp correctly waited 1.006s then errored. The in-tree fault-injection tests (fs.rs:1253-1340) abort a child mid-write at each stage and assert the target is never truncated. `why`/`history` read the authoritative dlog directly (project.rs:39-52), and load_dmap/ensure_dmap_current self-heal a missing/stale/corrupt/oversized dmap from the dlog (storage.rs:58-129), so a bad derivative never yields wrong answers.

**Independent verification:** Read the cited code and ran the binary. CODE: (1) atomic_write_bytes_impl at src/core/fs.rs:115-147 does exactly ensure_parent_dir -> create_temp_sibling -> write_all -> sync_all -> replace_file -> best_effort_flush_parent_dir, with `if result.is_err() { drop+remove_file(temp) }` cleanup. replace_file is MoveFileExW(MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH) on Windows (fs.rs:719-757) and fs::rename elsewhere (fs.rs:759-768); best_effort_flush_parent_dir fsyncs the parent dir on unix (fs.rs:770-774). (2) write_dlog at src/core/storage.rs:131-135 renders YAML then calls parse_dlog_yaml(&rendered)? BEFORE atomic_write_text, so unparseable dlog is never persisted. (3) create_lock_file uses OpenOptions create_new(true) (O_CREAT|O_EXCL) (fs.rs:296-299); lock_create_error_is_contention classifies Windows raw_os_error()==5 and Unix PermissionDenied-on-existing-path as contention (fs.rs:334-342). (4) recover_stale_lock (fs.rs:344-363) acquires a separate `.recover` guard via acquire_stale_recovery_lock then RE-READS (read_lock_file "before stale recovery") and RE-VALIDATES (lock_file_read_is_recoverable) the target before remove_stale_lock — closing the TOCTOU. (5) why/history read the authoritative dlog directly via load_dlog (src/core/project.rs:40, 50); load_dmap/ensure_dmap_current self-heal missing/stale/corrupt/oversized dmap from the dlog (storage.rs:58-129).

RUNTIME: 120 parallel writers (xargs -P 120) to ONE file with 120 distinct anchors in /tmp/conc3 -> 115 "Recorded", 5 clean "lock already exists" rejections, 0 other errors; dlog had 115 decisions, 115 unique dec_ ids with zero duplicates (grep uniq -d empty), dmap exactly 115 lines, and `find` showed zero leaked .lock/.recover/.archiva-tmp files. (Writing 120 times to the SAME anchor instead correctly collapses to a single record dec_120, no corruption — overwrite semantics.) Corrupting the dmap to "TOTALLY BOGUS" then running `why src/app.ts` still returned correct records from the dlog; running `status` regenerated the dmap to the correct 115 lines (self-heal confirmed). Stale-lock timing: hand-planted dead-PID(999999)+2020-timestamp lock recovered in ~12ms (1 Recorded); dead-PID+fresh-timestamp lock correctly waited 1.008s then returned "lock already exists" error. A clean single write left no lock file behind (Drop releases). `cargo test --release --lib fs::` -> 26 passed/0 failed, including atomic_write_faults_before_replace_preserve_old_complete_file, atomic_write_fault_after_replace_leaves_new_complete_file, atomic_write_killed_child_never_leaves_truncated_target, and the stale-recovery-guard tests.

**Verifier notes / severity correction:** Claim is a positive verification (operational/info) and is accurate. Every mechanism described — atomic temp+sync+rename+parent-fsync with temp cleanup, write_dlog self-reparse, O_EXCL lock with platform-specific contention classification, guard-locked re-read/re-validate stale recovery, dlog-authoritative reads, and dmap self-heal — was confirmed in source and reproduced at runtime. The exact counts differ trivially from the prior auditor's run (I observed 115 recorded + 5 rejections vs their 106 + 14; recovery 12ms vs 9ms; wait 1.008s vs 1.006s) — these are nondeterministic scheduling/timing artifacts of the parallel race and the lock retry window, not behavioral discrepancies. One methodology nuance worth recording: 120 concurrent writers to the SAME anchor do not produce 120 distinct ids — same-anchor writes overwrite to a single last-writer-wins record (dec_N), so unique sequential ids are only observable with distinct anchors (which my conc3 run used). This does not weaken any correctness or crash-safety claim. No defect found; severity info is correct.

**Recommended resolution:** No action. Keep these invariants under test if the write path is refactored for the cross-file-atomicity fix.

---

## Project workflows orchestration  — score 8/10

> This subsystem wires the core decision-memory operations behind three entrypoints: the CLI (`archiva init|why|history|lint|status|hooks|write-decision`), the MCP stdio server (`write_decision|why|ghost_check`), and Claude Code hooks (`session-start|post-tool-use`). I verified by reading every relevant source file and by running the release binary in ~12 scratch projects under /tmp: init idempotency (3x no-diff), settings merge with unrelated keys (model/permissions/env/PreToolUse all preserved, order intact), malformed-settings handling, the full write→session-start→edit→post-tool-use→why/status/lint loop, git-HEAD-based reanchoring, Rust-only file-move detection, parser-incomplete safety, and the MCP stdio protocol. The core orchestration is solid and well-tested (301 lib tests pass): the settings merge is genuinely non-destructive and idempotent, atomic writes use temp-sibling+rename with cleanup, and locking guards every dlog/dmap mutation. The two findings that matter are behavioral: (1) post-tool-use reanchoring silently fails to shift `lines_hint` and falsely marks decisions STALE whenever the edited file has no committed HEAD blob (brand-new files) or accumulates multiple edits between commits — the common case during an active editing session — because the git-HEAD diff baseline is empty or stale; and (2) `init` is not transactional across its four steps, so a mid-sequence failure leaves a partial install (recoverable via idempotent re-run). Finding (1) is a faithful port of a TS-side architectural limitation, not a Rust regression, but it directly degrades the headline `why <file> <line>` feature. CLI/MCP/hook paths reach the same core functions consistently; the only cross-surface divergence is ghost_check's lint scope (single-file in Rust vs whole-project-filtered in TS) and its intended-but-notable write side effects.

*Score rationale:* The orchestration layer is well-engineered: settings merge is provably non-destructive and idempotent (verified across many runs), per-file atomic writes with temp-sibling+rename and cleanup, consistent locking around every mutation, clean CLI/MCP/hook convergence on the same core functions, and strong test coverage (301 lib tests pass, including lock-contention and corruption cases). Points off for the post-tool-use HEAD-diff reanchoring that produces false STALE and wrong line hints during the most common editing workflow (a ported architectural limitation, but it undercuts the product's headline feature), the non-transactional init, and a few error-message/side-effect rough edges. None are data-loss bugs; the worst is metadata correctness under active editing.

**Verified behaviors (checked, not assumed):**

- init idempotency: ran `archiva init` 3x in /tmp/archiva-audit/proj; .claude/settings.json and AGENTS.md md5 identical across all runs; --gitignore-decisions run twice yields a single '.decisions/' line
- settings merge non-destructive: /tmp/m with model/permissions/env/mcpServers.other/PreToolUse(Bash) — all preserved verbatim, key order intact, archiva SessionStart/PostToolUse/mcpServers.archiva appended without disturbing existing entries
- malformed settings handling: '[]','"x"','42','null','true' -> '.claude/settings.json: expected object' exit 1; '{}trailing' -> 'Unexpected trailing characters'; '{"hooks":' -> 'Unexpected end of input'; in all cases .decisions/ created but AGENTS.md not (partial state)
- full loop: write-decision -> session-start renders '[Archiva] Decision map loaded...' with compact rejected list; dmap '1-7:fn:processOrder'; status tallies '1 decisions 0 stale 0 orphan'
- git-HEAD reanchoring (happy path): committed baseline, inserted 3 lines above function -> post-tool-use shifted dmap to '4-10:fn:processOrder', 0 stale, why reflects lines 4-10
- false STALE (no HEAD blob): /tmp/newfile never-committed file + insert-above -> '1 stale', dmap stuck at '1-3' STALE; `why src/d.ts 6` after compounding edits -> 'No decision found'
- Rust-only move detection: /tmp/mv renamed old.ts->new.ts (same content), post-tool-use migrated dlog+dmap to new.ts.dlog, 0 stale/orphan, why on new.ts works
- parser-incomplete safety: unbalanced braces -> post-tool-use '0 stale, 0 orphan' (no false orphan), lint emits arc/parser error, no status written to dlog
- MCP stdio: initialize/tools/list/tools/call(why) over newline-delimited JSON-RPC all respond correctly; tools list = write_decision, why, ghost_check only (no session_start/status/lint/history/post_tool_use exposed)
- MCP ghost_check write side effect: single call persisted STALE+stale_since into dlog and dmap
- commands before init: status/session-start/lint all exit 0 with empty-state messages; lint walks source tree and flags a multiline complexity-6 function as arc/undecided, reflected as '1 issues' in status
- test suite: `cargo test --release --lib` -> 301 passed, 0 failed, 1 ignored

### F39. [HIGH] post-tool-use falsely marks decisions STALE and fails to shift line hints when no committed HEAD baseline exists

`architecture` · location `src/core/project.rs:223-344 (post_tool_use), :271-275 (old_content fallback); src/core/git.rs:153-189 (read_git_head_file)` · reporter-confidence high · verification **CONFIRMED**

**Description:** post_tool_use computes line drift by diffing the working-tree content against the file's content at git HEAD (read_git_head_file). When HEAD has no blob for the file (a file created and decided within the same session, never committed) read_git_head_file returns Err, and line 273 falls back to `old_content = new_content`. diff_lines(new, new) is empty, so apply_line_changes_to_range never shifts lines_hint. If the edit inserted lines ABOVE the anchor, the anchor's real position has moved but lines_hint stays at the old range; is_fingerprint_stale then hashes the WRONG lines, mismatches, and the decision is marked STALE. The same happens cumulatively for committed files edited multiple times between commits: HEAD stays fixed at the last commit, so the second in-session edit is diffed against a stale baseline and the shift is computed incorrectly.

**Why it matters:** post-tool-use is registered to fire on every Write/Edit/MultiEdit (settings.rs:6 matcher), i.e. continuously during active editing — precisely when files are uncommitted or commit-lagging. The result is spurious STALE flags on correct, unchanged decisions and a lines_hint that points at the wrong code, which breaks `why <file> <line>` (the line lands outside the recorded range) and pollutes `status`/`lint`/session-start with false staleness.

**Impact:** Core 'why at this line' lookup returns 'No decision found' for code that does have a decision; false STALE noise erodes trust in the decision map. No data loss (chose/because/rejected preserved), but the anchoring metadata becomes wrong.

**Likelihood:** High during normal agent editing sessions (new files, or multiple edits between commits); zero only if every edit is immediately committed before the hook runs.

**Evidence (reporter):** Reproduced in /tmp/newfile: committed scaffold, created src/fresh.ts (never committed), wrote decision lines 1-3, inserted 2 lines above so fn:fresh moved to 3-5, ran `archiva hooks post-tool-use src/fresh.ts` -> output 'Re-anchored src/fresh.ts: 1 stale, 0 orphan.' and dmap stayed '1-3:fn:fresh:STALE'. Also /tmp/compound: committed v0 at 1-3, edit1 correctly shifted to 3-5 (0 stale), edit2 (HEAD still v0) produced dmap '7-9:fn:calc:STALE' while the function was actually at 5-7; `why src/d.ts 6` -> 'No decision found for src/d.ts at line 6.'

**Independent verification:** Read the cited code and reproduced both scenarios against /home/ubuntu/archaeo/target/release/archiva.

CODE PATH (verified by reading):
- src/core/project.rs:271-275 — old_git_file defaults to the current file; old_content = read_git_head_file(...).unwrap_or_else(|_| new_content.clone()). When HEAD has no blob (uncommitted file), read_git_head_file returns Err, so old_content := new_content.
- src/core/diff.rs:46-54 — diff_lines(new, new) yields no Added/Removed changes.
- src/core/diff.rs:17-44 — apply_line_changes_to_range with empty changes applies offset 0, so lines_hint is NOT shifted.
- src/core/decision_status.rs:8-14 — is_fingerprint_stale hashes get_lines(source, lines_hint.start, lines_hint.end). With an unshifted (now-wrong) range it hashes the wrong lines, mismatches the stored fingerprint, and the decision is marked STALE (project.rs:318-322).
- src/core/git.rs:153-189 — read_git_head_file_native reads the file's blob from the HEAD commit tree; the baseline is fixed at the last commit, so a second intra-session edit is diffed against a stale baseline (project.rs:271-275 always passes HEAD content, never the prior post-tool-use snapshot).

SCENARIO 1 (uncommitted new file) — /tmp/newfile: committed scaffold (README only), created src/fresh.ts (never committed), wrote decision fn:fresh lines [1,3] -> dmap '1-3:fn:fresh'. Inserted 2 comment lines above so the function moved to lines 3-5. `git cat-file -p HEAD:src/fresh.ts` -> 'fatal: Not a valid object name HEAD:src/fresh.ts' (no HEAD blob). Ran `archiva hooks post-tool-use src/fresh.ts` -> "Re-anchored src/fresh.ts: 1 stale, 0 orphan." dmap stayed '1-3:fn:fresh:STALE'. dlog shows status: STALE, lines_hint [1,3]. `archiva why src/fresh.ts 4` (real body) -> "No decision found for src/fresh.ts at line 4." `archiva why src/fresh.ts 2` (now a comment) -> wrongly returns the decision. Exactly as claimed.

SCENARIO 2 (compound, committed file) — /tmp/compound: committed src/d.ts v0 with fn:calc at 1-3, decision [1,3]. EDIT1 inserted 2 lines above (function now 3-5), HEAD still v0 -> post-tool-use: "0 stale", dmap '3-5:fn:calc' (correct). EDIT2 inserted 2 more lines above (function now actually 5-7), HEAD STILL v0 -> post-tool-use: "1 stale", dmap '7-9:fn:calc:STALE'. `archiva why src/d.ts 6` and `... 5` (the real function lines) both -> "No decision found". Exactly as claimed (the claim's '6' lookup matches).

ADDITIONAL FINDING beyond the claim — the corruption is sticky and compounds: I then committed the function-at-5-7 state and re-ran post-tool-use. Now HEAD == worktree, so diff_lines is again empty and the already-wrong range stays put: dmap still '7-9:fn:calc:STALE', `why src/d.ts 6` still "No decision found". It does NOT self-heal on commit; once lines_hint diverges from the HEAD baseline the divergence persists and any later edit's offset is computed from the wrong starting range, so the error compounds rather than corrects.

MITIGATION (confirmed): anchor-based lookup still works — `archiva why src/d.ts fn:calc` returns the decision (with the wrong line range and STALE flag), and chose/because/rejected are preserved. No content data loss; only the anchoring metadata (lines_hint + status) is corrupted.

**Verifier notes / severity correction:** The finding is real and the code analysis, locations, and both reproductions are accurate. I am correcting severity from medium to HIGH: this is not an edge case but the tool's core loop. The post-tool-use hook is designed to fire after every agent file edit, and the intended workflow is to keep anchors fresh DURING a session, between commits — precisely the window where the HEAD baseline is wrong. For any file created+decided within a session before its first commit, line-based `why` is broken from the first edit. For committed files, only the first edit since the last commit re-anchors correctly; the second and later intra-session edits corrupt the range and falsely mark STALE. I also extend the claim's impact: the corruption does NOT self-heal after commit (HEAD==worktree => empty diff => range stays wrong) and compounds across subsequent edits because future offsets are measured from an already-diverged lines_hint against a baseline that has moved. Recommended resolution: do not use HEAD as the diff baseline for the post-tool-use re-anchor. Instead diff against the file content snapshot captured at the time of the last successful re-anchor / decision write (persist a per-file pre-edit content hash or the prior normalized source), or fall back to re-locating the anchor by extracted anchor position (the extraction.anchors map already has the true start/end — project.rs:290-299 only uses it for the moved-file fallback) rather than only shifting via diff. When read_git_head_file errors or HEAD==worktree, prefer snapping lines_hint to the freshly extracted anchor range before the fingerprint-staleness check, instead of leaving the stale range and marking STALE.

**Recommended resolution:** When read_git_head_file errors (no HEAD blob), do not diff against an empty baseline. Instead, since extract_anchors already located the anchor's CURRENT range in new_content, prefer re-anchoring directly from the live anchor position (as the code already does for the moved-file fallback at project.rs:291-298) rather than from a HEAD diff. At minimum, when the anchor still exists and its fingerprint matches the live anchor range, set lines_hint from the extraction and skip the STALE marking. This also fixes the compounding-shift case. Document that HEAD-diff reanchoring is only reliable immediately after commit.

---

### F40. [LOW] init is not transactional: a failure after the first step leaves a partial installation

`operational` · location `src/core/init.rs:37-65 (init_project)` · reporter-confidence high · verification **CONFIRMED**

**Description:** init_project executes four ordered side-effecting steps with no rollback: (1) create .decisions/, (2) parse+write .claude/settings.json, (3) merge+write AGENTS.md, (4) optionally merge+write .gitignore. If any later step fails (malformed existing settings, AGENTS.md path is a directory, permission error), the earlier steps remain committed. There is no cleanup and no all-or-nothing guarantee.

**Why it matters:** A user running `archiva init` against a repo with a pre-existing malformed .claude/settings.json gets the .decisions/ dir created but no AGENTS.md/gitignore and no settings update, with a partial state on disk. Because each step is individually idempotent, a corrected re-run converges, so impact is limited.

**Impact:** Confusing partial state after a failed init; no corruption (atomic_write_text is itself atomic per-file). Self-heals on re-run after fixing the underlying cause.

**Likelihood:** Low — requires pre-existing malformed settings or an unusual filesystem condition.

**Evidence (reporter):** /tmp/t_array (settings.json='[]'): init exits 1 with '.claude/settings.json: expected object' yet .decisions/ exists and AGENTS.md does not. /tmp/perm (AGENTS.md is a directory): init exits 1 with 'Is a directory (os error 21)' after .claude/settings.json was already written, leaving .decisions/ + settings.json present but AGENTS.md/gitignore absent.

**Independent verification:** Read src/core/init.rs:37-65 — init_project executes exactly the four ordered side-effecting steps described, with no rollback/cleanup and no all-or-nothing guard.

Reproduced both claimed scenarios against /home/ubuntu/archaeo/target/release/archiva:

Scenario A (settings.json='[]'): in /tmp/t_array, `archiva init` printed ".claude/settings.json: expected object" and exited 1. Afterward `.decisions/` existed (created at init.rs:39), but AGENTS.md and .gitignore did NOT exist. Failure occurred at step 2 (merge_claude_settings_json, init.rs:44), so steps 3-4 never ran while step 1 was already committed. Matches claim exactly.

Scenario B (AGENTS.md is a directory): in /tmp/perm (mkdir AGENTS.md), `archiva init` printed "Failed to read file /tmp/perm/AGENTS.md: Is a directory (os error 21)" and exited 1. Afterward `.decisions/` AND `.claude/settings.json` (538 bytes) both existed, but `.gitignore` did NOT. Failure at step 3 (read_text_if_exists on AGENTS.md, init.rs:48) after steps 1-2 committed. Matches claim exactly.

Impact claims verified:
- No corruption: atomic_write_text -> atomic_write_bytes_impl (src/core/fs.rs:107-137) writes to a temp sibling, sync_all, then replace_file — per-file atomic. Confirmed.
- Self-heals on re-run: after `printf '{}' > settings.json` in t_array, re-running init exited 0 and created AGENTS.md. After `rmdir AGENTS.md` in perm, re-running init exited 0 and created AGENTS.md. Both confirmed idempotent recovery.

Note: the behavior is explicitly captured by the existing unit test init_project_surfaces_invalid_existing_settings_after_creating_decisions_dir (init.rs:182-199), which asserts .decisions/ exists while AGENTS.md/.gitignore do not after a settings failure — i.e. the partial-state-on-failure is a known, accepted property, not an oversight.

**Verifier notes / severity correction:** CONFIRMED with no corrections. Title, category (operational), severity (low), location (src/core/init.rs:37-65), description, evidence, and impact are all accurate. Both reproductions match the auditor's reported output byte-for-byte in substance (exit 1, the exact error strings, the exact surviving/absent files). Severity low is correct and not understated: there is no data corruption (per-file atomic writes), the failure is loud (non-zero exit + clear message), and a re-run after fixing the root cause fully self-heals because every step is idempotent (create_dir_all, settings merge dedups, AGENTS marker check, gitignore line check). The only cost is a transiently confusing partial layout (.decisions/ and possibly .claude/settings.json present without AGENTS.md/.gitignore). Recommended resolution if hardening is desired: stage the three text writes and commit them only after all merges parse successfully, or track created paths and best-effort clean up the freshly-created .decisions/ dir on error — but this is an optional polish, appropriately ranked low for release readiness.

**Recommended resolution:** Either (a) validate the existing settings.json (parse-only) BEFORE creating .decisions/ so a bad-settings init is a clean no-op, or (b) document that init is idempotent and safe to re-run after fixing the reported error. Option (a) is cheap and removes the most likely partial-state case.

---

### F41. [LOW] JSON parse errors from settings/dlog lack the file-context prefix that schema errors carry

`techdebt` · location `src/core/settings.rs:8-15 (merge_claude_settings_json); src/core/error.rs:100-103 (user_message); src/core/json.rs:135,186` · reporter-confidence high · verification **CONFIRMED**

**Description:** merge_claude_settings_value emits schema errors prefixed with the field name ('.claude/settings.json: expected object'), but a raw JSON syntax error in the same file surfaces as the bare parser message ('Unexpected end of input', 'Unexpected trailing characters') with no indication it came from .claude/settings.json. The two error classes for the same file present inconsistently to the user.

**Why it matters:** When init fails on a syntactically broken settings.json, the message gives no hint which file is at fault, making the failure harder to diagnose than the structurally-invalid case.

**Impact:** Reduced diagnosability of init failures; purely a message-quality issue.

**Likelihood:** Low-medium — occurs whenever an existing settings.json has a syntax error rather than a wrong top-level type.

**Evidence (reporter):** /tmp/t_broken (settings.json='{"hooks":') -> 'Unexpected end of input'; /tmp/t_trailing ('{}trailing') -> 'Unexpected trailing characters'; compare /tmp/t_array ('[]') -> '.claude/settings.json: expected object'.

**Independent verification:** Reproduced end-to-end via `archiva init` in three scratch dirs under /tmp:

- /tmp/t_broken/.claude/settings.json = `{"hooks":` -> stderr `Unexpected end of input`, exit=1
- /tmp/t_trailing/.claude/settings.json = `{}trailing` -> stderr `Unexpected trailing characters`, exit=1
- /tmp/t_array/.claude/settings.json = `[]` -> stderr `.claude/settings.json: expected object`, exit=1

Code path confirms the asymmetry:
- src/core/init.rs:44 calls merge_claude_settings_json(read_text_if_exists(settings_path)).
- src/core/settings.rs:10 calls json::parse(input)? on the file contents. A syntax error returns a JsonError.
- src/core/error.rs:128-136 `From<JsonError>` builds `ArchivaError::Json { message: error.message() }` with NO field/path. src/core/error.rs:100 `Self::Json { message, .. } => message.clone()` returns the bare parser string.
- The raw parser strings come from src/core/json.rs:186 ("Unexpected end of input") and json.rs:135 ("Unexpected trailing characters").
- By contrast, the structural rejection at settings.rs:18-22 calls `ArchivaError::schema(".claude/settings.json", "expected object")`, and error.rs:103 formats Schema as `"{field}: {message}"`, yielding the file-prefixed message.

So two error classes for the same file present inconsistently, exactly as claimed. Cited file:line references (settings.rs:8-15, error.rs:100-103, json.rs:135/186) all check out.

**Verifier notes / severity correction:** Accurate and correctly scoped as low/techdebt — purely a message-quality/diagnosability issue, not a correctness or security defect (init still fails closed with exit=1, no bad file is written; atomic_write happens only after a successful merge). Two minor scope corrections: (1) The title says "settings/dlog", but .dlog files are YAML, not JSON, so the JSON parse path does not apply to dlog. The same diagnosability gap does exist for YAML (error.rs:101 `Self::Yaml { message, .. } => message.clone()` also returns a bare message with no file context), so the underlying pattern generalizes, but the specific JSON evidence is settings.json-only. (2) The JSON error also carries a line/column (computed at error.rs:131-133) that user_message() discards entirely, so even the offset hint is dropped — worth fixing alongside the file prefix. Recommended fix: thread a file/source label into the JSON (and YAML) error at the call sites (e.g. add a context wrapper or a `with_context`/path field on the Json/Yaml variants) so init parse failures read like `.claude/settings.json: Unexpected end of input`, matching the schema-error format.

**Recommended resolution:** Wrap the json::parse call in merge_claude_settings_json to map JsonError into a schema/context error carrying the '.claude/settings.json' field, mirroring the existing prefixed schema errors.

---

### F42. [LOW] Read-oriented surfaces (session-start, status, MCP ghost_check) silently mutate dlog/dmap on disk

`tradeoff` · location `src/core/project.rs:69-85 (session_start -> ensure_dmap_current), :399-410 (status summaries), :97-112 (lint_file used by MCP ghost_check); src/mcp.rs:124-131` · reporter-confidence high · verification **CONFIRMED**

**Description:** session_start and status call ensure_dmap_current, which takes a file lock and rewrites the .dmap if it differs from the dlog-derived expectation. MCP ghost_check routes to lint_file(fix=false), which still calls mark_stale_now and persists status/stale_since into the dlog and dmap. So operations an agent reasonably treats as read-only inspection can write to .decisions/ and acquire locks.

**Why it matters:** An MCP ghost_check or a status call can change tracked files (dmap/dlog), producing unexpected git diffs and acquiring locks that can contend with a concurrent post-tool-use hook. The STALE persistence on ghost_check is arguably intended (it mirrors lint), but the write-on-inspect behavior is non-obvious and differs in scope from the TS server.

**Impact:** Unexpected working-tree changes under .decisions/ from inspection commands; potential lock contention; cross-surface scope difference (Rust ghost_check lints one file, TS lints the whole project and filters).

**Likelihood:** Medium for ghost_check writes (any stale file); low for session-start/status (only when dmap is already out of sync).

**Evidence (reporter):** /tmp/ghostw: after editing src/g.ts to change the body, a single MCP ghost_check call wrote 'status: STALE' + 'stale_since' into src/g.ts.dlog and 'STALE' into the dmap (dlog had no status before the call). TS reference (dist/src/mcp/server.js:95) computes ghost_check via lintProject(projectRoot) filtered to the file, vs Rust project.rs:127 lint_file(single file).

**Independent verification:** Reproduced the core claim end-to-end against /home/ubuntu/archaeo/target/release/archiva.

REPRO (MCP ghost_check mutates dlog+dmap):
1. /tmp/ghostw: git init, archiva init, wrote g.ts (export function g), recorded a decision against anchor fn:g via `write-decision --json`. Initial .decisions/g.ts.dlog had NO status/stale_since field; dmap = "1-3:fn:g". md5(dlog)=42a93b00..., md5(dmap)=3fa36c2c...
2. Edited g.ts body (return x+999) to diverge the fingerprint.
3. Drove ONE MCP ghost_check call over stdio (initialize + tools/call name=ghost_check file=g.ts). Server returned only an issue text ("arc/stale fn:g ... fingerprint differs"), exit 0.
4. After the single call: md5(dlog)=e1c7d50d..., md5(dmap)=1ad8f7e8... — both changed. dlog gained `status: STALE` and `stale_since: '2026-06-30T20:10:10.707Z'`; dmap became "1-3:fn:g:STALE". diff confirms +2 lines added by the read-only inspection.

REPRO (status rewrites dmap): /tmp/ghs, recorded a decision, overwrote dmap with "BOGUS"; `archiva status` (returns Ok) rewrote it back to canonical "1-3:fn:h" (md5 3543b5...->d9732f8...).

REPRO (session-start rewrites dmap): same dir, overwrote dmap with "BOGUS2"; `archiva hooks session-start` rewrote it to "1-3:fn:h" (md5 ffebf3df...->d9732f8...).

CODE TRACE (matches cited lines):
- project.rs:69-85 session_start -> ensure_dmap_current(...,"session-start") at :79.
- project.rs:399-410 load_project_status_summaries -> ensure_dmap_current(...,"status") at :405 (called by status() :87-91).
- mcp.rs:124-133 ghost_check -> project::lint_file(...,fix=false) at :127.
- project.rs:97-112 lint_file -> lint_dlog_locked (:412) which acquires with_decision_file_lock (:420) and calls lint_dlog (:459). lint_dlog calls mark_stale_now (:521) and write_dlog/write_dmap (:545-546) when `changed` is true, INDEPENDENT of the `fix` flag. So fix=false still persists STALE status. Lock is acquired on every lint_file/ghost_check.
- storage.rs ensure_dmap_current (:100) takes a lock and rewrites dmap when content != render_dmap_from_dlog(dlog).

TS-reference scope sub-claim (verified, with a correction): dist/src/mcp/server.js:95 computes ghost_check as `lintProject(projectRoot).filter(issue.file===input.file)` — whole-project lint filtered to one file, whereas Rust project.rs:127 lints only the single file. So the cross-surface scope difference is real. NOTE: dist/src/lint/rules.js:59,78-79 shows the TS lint ALSO calls markStale + writeDlog + writeDmap, i.e. TS persists on lint too. The Rust mutation-on-read behavior is therefore faithful to the TS reference, and Rust's single-file scope makes its write side-effect NARROWER than TS's whole-project lint, not broader.

**Verifier notes / severity correction:** CONFIRMED and correctly scoped/severitied (tradeoff/low). All four cited locations are accurate. The mechanism is exactly as described: read-oriented surfaces (MCP ghost_check, status, hooks session-start) acquire a per-file lock and write to .decisions/ — ghost_check persists `status: STALE`/`stale_since` into the dlog and STALE into the dmap; status/session-start rewrite a divergent dmap via ensure_dmap_current.

Two corrections to the prior auditor's framing (neither changes the low severity):
1. This is NOT a Rust-introduced defect. The TS reference behaves the same way (dist/src/lint/rules.js persists markStale via writeDlog/writeDmap on lint). The behavior is faithful parity, which supports keeping it at low/tradeoff rather than escalating.
2. The cross-surface scope difference is real but its impact is the opposite of "broader": Rust ghost_check lints ONLY the requested file, so it can only mutate that one file's dlog/dmap. TS ghost_check runs lintProject over the WHOLE project (persisting stale status to every divergent file) and then filters the returned issues to the requested file — a wider write side-effect. The user-visible issue output is equivalent; the persistence blast radius differs, with Rust being the narrower (safer) of the two.

Practical impact remains low: writes are confined to .decisions/ (a derived/managed store, not user source), are idempotent (re-running ghost_check on an already-STALE decision produces no further dlog churn since `was_already_stale` short-circuits the mark), and are lock-protected. The genuine residual concerns are (a) surprising working-tree churn under .decisions/ from commands an agent treats as read-only, which can produce unexpected git diffs, and (b) lock acquisition on inspection paths enabling contention/stale-lock interactions. Recommended resolution: either document that inspection commands are allowed to reconcile the derived dmap and persist freshly-detected stale status, or add a read-only mode for MCP ghost_check / status / session-start that computes issues without persisting (compute stale in-memory, skip write_dlog/write_dmap and the ensure_dmap_current rewrite).

**Recommended resolution:** Document that lint/ghost_check intentionally persist staleness, and ensure_dmap_current's self-heal is by design. If read-only semantics are desired for ghost_check/status, add a non-persisting code path that computes staleness without writing. Reconcile the single-file vs whole-project lint scope between Rust and TS to keep cross-surface parity.

---

## Serialization: JSON / YAML / dlog / dmap parsers  — score 6/10

> These are hand-written, std-only replacements for serde/js-yaml. The JSON parser is solid: it implements the spec correctly (escapes, surrogate pairs, control-char rejection, leading-zero rejection, depth limit at 512, byte limit) and its number stringifier faithfully mirrors JavaScript JSON.stringify semantics (1e21/1e-7 boundaries, exponent normalization, -0→0, Infinity→null). The dmap reader/writer and the OrderedMap are simple and mostly correct. The YAML layer is where the system breaks down: it contains a reachable panic (UTF-8 mid-codepoint byte slice in block-scalar parsing) that aborts the process on any crafted/hand-edited/merge-corrupted .dlog, and it is not round-trip faithful — the renderer silently loses leading/trailing whitespace, tabs, and runs of internal spaces (folded-block path), and write_dlog re-parses only to check validity, never comparing values, so the loss passes silently. The .dmap status-suffix trick is genuinely ambiguous for anchors whose final segment equals a status keyword (fn:STALE renders to a line that re-parses as anchor "fn" + status STALE). dlog is authoritative and parse_dmap currently has only test callers, which limits live blast radius of the dmap ambiguity, but the on-disk .dmap that agents are instructed to read is wrong. Newline-injection field forging is correctly contained by block-scalar indentation.

*Score rationale:* The JSON parser and number stringifier are correct and faithfully match the JavaScript contract, with good limit enforcement and a clean test suite — high quality. The dmap and OrderedMap are simple and mostly correct. The YAML layer drags the score down: a reachable process-aborting panic on crafted input (no panic-safety on a byte slice), and the renderer/parser pair is not round-trip faithful (silent loss of leading/trailing/internal whitespace and tabs) while the write path validates only parseability, never value equality. These are real correctness/availability defects in a tool whose entire value proposition is faithful, durable decision memory. The dmap status-suffix ambiguity and silent unknown-field dropping are lower-severity but show the round-trip contract was not systematically enforced. Fixable with bounded changes (char-boundary-safe slicing, whitespace-aware quoting, and a round-trip equality assertion in write_dlog), but as shipped the serialization layer has a high-severity DoS and medium-severity data-loss bugs.

**Verified behaviors (checked, not assumed):**

- Built binary at /home/ubuntu/archaeo/target/release/archiva runs init/write-decision/why/status/lint/hooks/mcp as described.
- CONFIRMED PANIC: crafted .dlog with a literal block whose continuation line has a multibyte char straddling the first line's indent byte offset -> panic at src/core/yaml.rs:311 'not a char boundary', RC=101. Reproduced via why, status, lint, hooks session-start, and the MCP stdio server (server died mid-session).
- CONFIRMED data loss: write-decision with chose=' leading' / 'trailing ' / '\ttab-start' reads back with the whitespace stripped; root cause is needs_single_quotes not quoting whitespace + parser trim_start/trim_end.
- CONFIRMED data loss: >100-char because with double spaces collapses to single spaces on readback (folded-block wrap_words split_whitespace); literal-block continuation line less-indented than first line loses leading chars (because='    deep first\nshallow second' -> 'deep first\nlow second').
- CONFIRMED dmap round-trip break via standalone harness over dmap.rs: anchor 'fn:STALE' (no status) renders '1-3:fn:STALE' and re-parses to anchor 'fn' + status STALE; same for 'fn:UNDECIDED'. write-decision accepts fn:STALE for a function named STALE; .dmap on disk = '1-3:fn:STALE'.
- CONFIRMED unknown-field dropping: hand-written dlog with custom_top_level and custom_field lost both after `hooks post-tool-use` rewrite.
- VERIFIED CORRECT: JSON number stringify matches JS (1e21->1e+21, 1e-7->1e-7, 1e-6->0.000001, 0.0000001->1e-7, -0->0, 1e309->null, 9007199254740993->...992); depth limit (512) and byte limit enforced gracefully; surrogate pairs and control-char rejection per tests.
- VERIFIED CORRECT: newline injection in chose does not forge sibling YAML fields — payload is contained inside the indented block scalar; real fingerprint/because survive.
- VERIFIED: write-decision gates anchors via assert_anchor_exists, so arbitrary newline/colon-space/# anchors cannot be injected through the normal write path; C++/Rust extraction sanitizes anchors to safe forms.
- VERIFIED: YAML deep flow/block nesting hits the configured depth limit (graceful error) rather than overflowing the stack; multibyte UTF-8 in flow keys/values does not panic.
- VERIFIED: load_dmap/parse_dmap have no non-test production callers; dlog is authoritative and the dmap is a write-only derivative regenerated from dlog, which bounds the live impact of the dmap ambiguity.

### F43. [HIGH] Reachable panic: UTF-8 mid-codepoint byte slice in YAML block-scalar parsing aborts the process

`defect` · location `src/core/yaml.rs:310-314 (raw.text[content_indent..])` · reporter-confidence high · verification **CONFIRMED**

**Description:** parse_block_scalar derives content_indent from the leading-space byte count of the FIRST content line, then byte-slices every subsequent line at that offset: `raw.text[content_indent..]`. If a later line has fewer leading spaces followed by a multibyte UTF-8 character, content_indent lands inside that character and the slice panics ('byte index N is not a char boundary'). count_indent only counts spaces, so it does not protect this slice. There is no char_boundary check.

**Why it matters:** .dlog files are stored in-repo, are meant to be human-readable, hand-editable, and git-mergeable. A single malformed literal/folded block (`|-` or `>-`) crashes the binary instead of producing a clean error. The MCP server is long-running, so one bad decision file terminates the whole agent session.

**Impact:** Denial of service / availability. Confirmed to abort why, status, lint, hooks session-start, and the MCP server (panic terminates the stdio server mid-session).

**Likelihood:** Medium — requires a continuation line shorter than the first line's indent that begins with a multibyte char; arises from manual edits, merge artifacts, or any non-Rust-generated dlog. Not produced by the tool's own writer.

**Evidence (reporter):** Crafted /tmp/inj7/.decisions/src/a.rs.dlog with a literal block: first line '          firstlineindent10' (10 spaces), second line '         é trailing' (9 spaces + é). Running `archiva why src/a.rs fn:foo` -> `thread 'main' panicked at src/core/yaml.rs:311:25: start byte index 10 is not a char boundary; it is inside 'é' (bytes 9..11 of string)`, RC=101. Reproduced identically via MCP (`tools/call` why) — server printed the initialize result then panicked and died — and via `status`, `lint`, and `hooks session-start`.

**Independent verification:** READ src/core/yaml.rs:309-314 — parse_block_scalar fixes content_indent from the first content line's space count (count_indent at 1168-1178 counts only ' ' bytes) and slices every later line `raw.text[content_indent..]` guarded ONLY by `raw.text.len() >= content_indent` (byte length), with no is_char_boundary check.

RAN /home/ubuntu/archaeo/target/release/archiva. Setup: init'd /tmp/inj7 (git repo + src/a.rs), wrote a valid decision via stdin JSON to learn the dlog schema, then overwrote .decisions/src/a.rs.dlog with a `chose: |` literal block: line1 = 10 spaces + "firstlineindent10" (sets block_indent=10), line2 = 9 spaces + "é trailing". Verified bytes with xxd: line2 = 20*9 then `c3 a9` (é) at byte offsets 9-10, so content_indent=10 lands inside é.

Results:
- `archiva why src/a.rs fn:foo` -> "thread 'main' panicked at src/core/yaml.rs:311:25: start byte index 10 is not a char boundary; it is inside 'é' (bytes 9..11 of string)", exit code 101.
- Same panic at src/core/yaml.rs:311:25 from `status`, `lint`, `hooks session-start`, and `history src/a.rs fn:foo`.
- MCP over stdio (`archiva mcp`, newline-delimited JSON-RPC): server printed the initialize result `{...serverInfo...0.2.0}`, then on `tools/call` name=why args={file,anchor} panicked to stderr at src/core/yaml.rs:311:25 and exited 101, terminating the session mid-stream.

Cited location (src/core/yaml.rs:310-314), panic message, panic site (311:25), and RC=101 all match the claim exactly.

**Verifier notes / severity correction:** Claim CONFIRMED with no material change to scope or severity. The .dlog file is the authoritative on-disk format and is shared/checked-into the repo, so crafted or replicated content reaches this slice; the panic is a reachable, deterministic process abort (availability/DoS) but not data corruption or RCE, so high (not critical) is correct. One minor correction to the reproduction recipe: the MCP `why` tool takes arguments {file, anchor}, not {path, anchor} (path yields a clean 'file: missing required field' error and never reaches the parser); with the correct `file` key the MCP panic reproduces exactly as claimed. Recommended fix: clamp/advance content_indent to the next char boundary (e.g. via str::is_char_boundary / floor_char_boundary, or iterate chars to strip indent) instead of raw byte slicing in parse_block_scalar; consider a single catch_unwind/Result boundary around per-file dlog parsing so one malformed file degrades gracefully rather than aborting the whole process and the long-lived MCP server.

**Recommended resolution:** Replace the byte slice with a char-boundary-safe strip of up to content_indent leading spaces (e.g. iterate chars, or use char_indices / str::get with a boundary check and fall back to taking the whole trimmed remainder). Add a fuzz/property test feeding arbitrary UTF-8 dlogs to parse_yaml and asserting it never panics.

---

### F44. [MEDIUM] write_dlog/render_yaml are not round-trip faithful: leading/trailing whitespace and tabs are silently dropped

`defect` · location `src/core/yaml.rs:963-977 (needs_single_quotes) and yaml.rs:388 (parse_scalar_with_options trim_end) / yaml.rs:280,1110-1114 (mapping value trim_start)` · reporter-confidence high · verification **CONFIRMED**

**Description:** needs_single_quotes does not treat leading/trailing whitespace or leading tabs as requiring quoting, so values like ' leading', 'trailing ', and '\ttab-start' are emitted as bare plain scalars. On read, parse_scalar_with_options does trim_end() and split_mapping_entry/parse_mapping_value do trim_start(), so the whitespace is lost. write_dlog (storage.rs:131-135) renders then re-parses ONLY to confirm it parses — it never compares the re-parsed value to the original — so the loss is silent.

**Why it matters:** chose/because/approach/reason are free-text fields recording engineering rationale. Silent mutation of stored content undermines the tool's core promise (faithful decision memory) and is invisible to the user.

**Impact:** Data loss on first write. Affects every string field uniformly.

**Likelihood:** Medium — leading/trailing spaces and tabs are common in pasted snippets, aligned text, and indented rationale.

**Evidence (reporter):** In /tmp/typ: write chose=' leading' -> readback 'leading'; chose='trailing ' -> 'trailing'; chose='\ttab-start' -> 'tab-start' (preserved=False in all three). dlog on disk shows e.g. `    chose: trailing ` (bare, unquoted) confirming the parser, not the writer, drops it.

**Independent verification:** Code matches the claim exactly:
- yaml.rs:963-977 needs_single_quotes() never flags leading/trailing space or leading tab; none of its conditions (starts_with([...]) list, ": ", " #", bool/null/number/timestamp) cover edge whitespace, so such values are emitted as bare plain scalars.
- yaml.rs:388 parse_scalar_with_options does .trim_end(); yaml.rs:280 parse_mapping_value does value_part.trim_start(); yaml.rs:1113 split_mapping_entry returns content[index+1..].trim_start(). So on read, both leading and trailing whitespace are stripped.
- storage.rs:131-135 write_dlog: render_dlog_yaml -> parse_dlog_yaml(&rendered)? only checks it PARSES; it never compares the re-parsed value to the original. Loss is silent.

Empirical reproduction in /tmp/typ (git-init'd, archiva init):
1) chose=" leading": dlog on disk = `    chose:  leading` (bare, two spaces). `archiva why app.rs fn:target` -> `Chose: leading` (leading space dropped).
2) chose="trailing ": dlog on disk = `    chose: trailing ` (bare, trailing space present in file, cat -A shows trailing `# Archiva v2 — Release-Readiness Audit

**Auditor role:** Independent Principal Software Architect / Release Auditor
**Subject:** Archiva v2 — std-only Rust re-engineering of a TypeScript "decision memory for AI coding agents" tool
**Date:** 2026-07-01
**Branch audited:** `codex/archiva-v2-rust-validation` (HEAD `33f160e`, version 0.2.0)

**Method:** 115 independent agents across 17 subsystem/dimension reviews; every substantive finding adversarially re-verified by a second agent that reproduced it against the compiled release binary or read the cited code. Documentation and the team's own `docs/archiva-v2-review-status.md` were treated as **unverified claims**, not evidence.

**Independently verified baseline (by the auditor before fan-out):**

- `cargo build --release` — clean.
- `cargo clippy --all-targets --locked -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `cargo test` — 301 lib tests pass (1 ignored) + 9 + 1 + 3 integration tests pass.
- Binary functional: `archiva --version` → `0.2.0`; `archiva status` on the repo → 537 decisions, 0 stale, 0 orphan, 0 issues across 56 `.dlog` files.

**Verification outcome across the panel:** 96 findings `CONFIRMED`, 1 `PLAUSIBLE`, **0 `REFUTED`**. Severity distribution (corrected, non-refuted): **17 high / 27 medium / 44 low / 9 info**, plus 10 audit-coverage gaps from a completeness critic.

---

## 1. Executive Summary

Archiva v2 is a genuinely impressive piece of engineering: a zero-dependency, std-only Rust implementation of a multi-language anchor extractor, a from-scratch git object reader (SHA-1 **and** SHA-256, packs, deltas, alternates), hand-written JSON/YAML parsers, a CLI, and a stdio MCP server — all clean-compiling, clippy-strict, well-formatted, and backed by 300+ tests and an elaborate differential/stress/scale/corpus validation harness. The code quality at the unit level is high and the discipline is real.

**But it is not ready for a stable 1.0 public release, and it is not yet the reference implementation.** The audit confirmed a coherent cluster of release-blocking problems that the existing test strategy structurally cannot see:

1. **A class of trivially-reachable process aborts (panics / stack overflows) triggered by ordinary committed, team-shared data.** Three distinct crashes were reproduced (a lone `'` in a `.dlog` → `yaml.rs:700`; a mid-codepoint UTF-8 slice → `yaml.rs:311`; deeply-nested source → unbounded recursion in the anchor extractor), and the completeness critic found a **fourth** (empty block scalar) in minutes. Because `.decisions/` is git-tracked by default and shared across a team, a single malformed byte in one file aborts `status`, `lint`, the per-session `session-start` hook, and — most seriously — **kills the long-lived MCP server mid-session**, dropping all in-flight agent context. This is a class, not a list.

2. **The product's headline automatic workflow is broken end-to-end.** The auto-wired `PostToolUse` re-anchor hook is a confirmed no-op under real Claude Code: `init` wires it with no argument relying on `ARCHIVA_FILE`, but Claude Code delivers the edited path as JSON on stdin, which `post-tool-use` never reads. It errors on every edit and silently never re-anchors. Compounding this, even when invoked correctly, re-anchoring is **non-idempotent** and **falsely marks correct decisions STALE** whenever there is no committed HEAD baseline (new files, or multiple edits between commits) — and the corruption compounds and does not self-heal.

3. **Performance cliffs contradict the "scales to large repos" claim, and the scale harness is blind to both of them.** Anchor extraction is O(n²) per file (a 1.4 MB file ≈ 55–66 s for one file); the hot-file write path is O(n²) cumulative (1,200 decisions in one file ≈ 86 s). The scale-smoke harness uses tiny one-function files and skips any file > 256 KiB, so neither bottleneck is ever exercised.

4. **Operational diagnosability is essentially zero** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery (dmap repair, stale-lock takeover) — for a tool that runs unattended as an agent hook.

None of the high-severity findings are memory-unsafety or RCE — Rust aborts cleanly — so the worst case is availability/DoS and silent metadata corruption, not exploitation. The defects are concentrated, well-understood, and individually fixable without architectural change. With a focused 4–8 week remediation pass (panic-safety hardening, hook stdin contract, idempotent re-anchoring, the O(n²) fixes, and a logging channel), this can become an excellent 1.0.

**Verdict: Do not ship as-is. Strong foundation; specific, fixable blockers.**

---

## 2. System Overview

Archiva stores *why code exists* beside the code. Per source file it maintains `.decisions/<path>.dlog` (authoritative YAML, schema:1) and `.decisions/<path>.dmap` (compact derivative index). Decisions are anchored to AST identities (`fn:foo`, `struct:Bar`, `block:if_x`) rather than line numbers, carry a fingerprint for drift detection, and form supersession chains. The same core operations are reachable three ways — CLI (`init`/`why`/`history`/`lint`/`status`/`hooks`/`write-decision`/`mcp`), a stdio JSON-RPC MCP server (`why`/`write_decision`/`ghost_check`), and Claude Code hooks (`session-start`/`post-tool-use`). Distribution is via an npm wrapper that selects a platform-specific native binary; the runtime is a single native binary with **no dependencies** (`Cargo.toml` `[dependencies]` is empty).

The architecture is sound and the product thesis is coherent. The problems are in robustness, the hook integration contract, performance at scale, and observability — not in the concept or the module decomposition.

**Module sizes (Rust, src/):** `anchor.rs` 12,341 (incl. ~5,090 test lines) · `git.rs` 4,329 · `project.rs` 2,261 · `fs.rs` 1,487 · `yaml.rs` 1,465 · `mcp.rs` 1,174 · `cli.rs` 1,030 · `decision.rs` 963 · `storage.rs` 959 · `json.rs` 722 · `diff.rs` 657 · `property_tests.rs` 540 · `dlog.rs` 507 · `paths.rs` 487. Total ~31,464 lines.

---

## 3. Architectural Assessment — **8/10**

Module boundaries are clean and cohesive: `cli`/`mcp` entrypoints → `core::project` orchestration → typed core modules (`decision`, `storage`, `dlog`/`dmap`, `anchor`, `git`, `paths`, `fs`). Coupling is low and the data-flow ownership (`.dlog` authoritative, `.dmap` rebuildable, request-scoped git reader) is well-reasoned.

The central architectural tension is the **zero-dependency, reimplement-everything-by-hand** stance: ~2.6k lines of git plumbing including a from-scratch DEFLATE inflater (`git.rs`), hand-written JSON/YAML parsers, and a multi-thousand-line multi-language anchor tokenizer. This is defensible *as a product tradeoff* (trivial supply chain, tiny binary, no transitive CVEs) but it concentrates the entire bug surface in hand-rolled parsers that have **not been fuzzed** — and that is precisely where every confirmed panic lives. The std-only purity is the root cause of the dominant risk class.

Two concrete architectural weaknesses (both verified):

- **No schema migration story.** `DLOG_SCHEMA_VERSION` is a hardcoded `1` and the parser hard-rejects anything else; a single forward-version file aborts every whole-repo command. There is no migrate-on-read and no skip-with-warning.
- **The ground-truth anchor range is computed and then discarded** in the normal re-anchor path (`project.rs:290-304`) — the parser already knows the anchor's exact current position, but the code trusts a fragile HEAD-diff shift instead. This is the root cause of the idempotency/STALE corruption.

Top simplification opportunity: prefer the extractor's live anchor position over diff-shifting; this single change fixes two high-severity findings at once.

---

## 4. Workflow Assessment — **5/10**

The intended loop is *read map → ask why → edit → write decision → lint drift*. Traced end-to-end:

- **`init` → first decision → `why`**: works cleanly; idempotent on re-run.
- **`write-decision`**: works, but is **non-atomic across `.dlog`/`.dmap`** (a torn write durably commits the decision while reporting failure, exit 1) and the natural retry is **non-idempotent** (overwrites the just-committed record with a new id, losing the original reasoning with no history entry).
- **Auto re-anchor on edit (the core promise)**: **broken under real Claude Code** (hook ignores the stdin payload) and **corrupts line attribution** even when invoked correctly (false STALE + compounding line drift with no committed baseline).
- **Re-deciding an anchor**: silently destroys the prior decision and its entire history unless `supersedes:<id>` is passed; superseding *into* an anchor occupied by a different live decision silently deletes that unrelated decision.

The "happy path" demos work; the realistic agent workflow (uncommitted files, multiple edits per commit, re-decisions) has multiple silent-data-integrity failures.

---

## 5. Feature Completeness Assessment — **7/10**

The advertised command set is fully present and the CLI surface is consistent and complete. Gaps are in fidelity rather than coverage: MCP `why` cannot do line-based lookup (the `line` field is silently dropped and it returns a *confidently wrong* whole-file result rather than "not found"); no `--fix` audit trail; no migration tooling. Feature breadth is appropriate and intentionally narrow (the README's positioning vs. broad memory tools is honest and well-argued).

---

## 6. Behavioral Consistency Assessment — **6/10**

CLI ↔ MCP ↔ hooks largely agree on validation and path normalization (verified: `.//src/a.ts` and `src\a.ts` normalize to one identity across entrypoints). Confirmed divergences:

- **MCP `why` ignores `line`** and returns the wrong decision; CLI `why <file> <line>` is correct.
- **MCP tool errors are returned as JSON-RPC protocol errors (`-32000`)** instead of the MCP convention `result.isError:true` — a spec inconsistency for tool *execution* failures.
- **`lint` exit code conflates** "found issues" with "command failed"; `status` returns 0 even with outstanding issues. No exit-code taxonomy.

---

## 7. Data Model Assessment — **7/10**

The decision record (chose/because/rejected/anchor/fingerprint/lines_hint/history/supersession) is well-designed and matches the spec and the YAML schema. Two material risks: **unknown/forward-compat fields are silently dropped on every rewrite** (data loss for any external or future-version annotation), and the **no-supersede overwrite** path discards history. The model is good; its *evolution and preservation guarantees* are weak.

---

## 8. Storage & Persistence Assessment — **7/10**

Individual writes are atomic (temp + rename + fsync; Unix parent-dir fsync). Verified strengths: corrupt `.dmap` self-heals from `.dlog`; stale-lock recovery works. Verified weaknesses:

- **No cross-file atomicity** between `.dlog` and `.dmap` (torn-write false-failure).
- **PID-liveness lock veto can wedge all writes indefinitely** (PID reuse defeats the staleness check; no force-unlock).
- **Read-only `status`/`lint` acquire the per-file write lock** and fail on a read-only `.decisions/`.
- Parent-dir durability fsync is a **no-op on Windows** (crash-consistency claim is weaker there and untested in crash-injection form).

---

## 9. CLI Assessment — **8/10**

The strongest subsystem. Dispatch, help text, exit-code routing (0/1), `--` escaping, and unknown-flag/command handling are correct and extensively tested. Rough edges: `write-decision` **reads stdin before validating its own args** (a malformed call with an open stdin hangs), the `lint` exit-code conflation, inconsistent `error:`-prefixing between argument vs. semantic errors, and `why` accepting line `0`.

---

## 10. Protocol & Integration Assessment — **6/10**

JSON-RPC 2.0 framing, id handling, `notifications/*` swallowing, and method dispatch are correct and tested. **The integration contract with Claude Code is broken** (PostToolUse stdin payload ignored — the single most important integration defect). MCP tool-error encoding deviates from the MCP convention. And the long-lived server has **no panic isolation** — one crafted `tools/call` aborts the whole process.

---

## 11. Performance Assessment — **6/10**

Measured, not assumed:

- **Anchor extraction O(n²) per file** (`is_top_level`/`rust_depths_before` re-scan the token prefix per declaration): TS 10k/20k/40k lines = 0.5/2.3/9.9 s; a 1.4 MB file ≈ 55–66 s; a single huge Rust fn ≈ 47 s vs ~3 s for the TS equivalent. Memory stays modest (~70 MB) — purely CPU.
- **Hot-file writes O(n²) cumulative** (full render + redundant re-parse + full rewrite + fsync per write): #1,200 in one file ≈ 86 s cumulative; 1,500 timed out at 120 s. The `storage.rs:133` self re-parse of freshly-rendered YAML is unconditional wasted work.
- **`status` parses every `.dlog` twice** per invocation and sweeps the whole source tree.
- The `.dmap` index **is never read by any command** — every read re-parses the verbose `.dlog`, so the derivative index pays maintenance cost for zero read benefit.

Startup is excellent (~0.7 ms). For small/medium repos performance is fine; the cliffs are real and reachable.

---

## 12. Scalability Assessment — **5/10**

The "scales to 100k files / 1M decisions / Linux-kernel-and-LLVM corpora" claim is **not substantiated by the harness**, which uses tiny one-function files, skips files > 256 KiB, and defaults to 1 decision/file — exactly avoiding both quadratic paths. The blast radius is also wider than the per-command framing: because whole-repo commands re-extract anchors from *every* source file (decided or not), one large generated/minified/vendored file degrades `lint`/`status` repo-wide. Linear-in-file-count scanning is otherwise reasonable.

---

## 13. Reliability Assessment — **5/10**

Dominated by the panic class: `status`, `lint`, `session-start`, and the MCP server all abort (SIGABRT / exit 101 or 134) on malformed-but-committed input, and **a single corrupt `.dlog` aborts the entire repo-wide command** rather than being skipped and reported — taking down visibility for all healthy files. Recovery paths that exist (dmap repair, lock recovery) are correct but silent. Single-file commands are correctly scoped and limit some blast radius.

---

## 14. Failure Recovery Assessment — **6/10**

Good: atomic single-file writes, self-healing dmap, stale-lock recovery. Weak: no cross-file transaction; `lint --fix` is non-atomic across files and leaves a partially-fixed repo with no record of what changed on mid-run failure; no panic boundary so corruption aborts rather than degrades; no migration/forward-compat recovery. The recovery primitives are sound but not composed into command-level resilience.

---

## 15. Security Assessment — **6/10**

Threat model is correct (local CLI; parses untrusted repo/agent-supplied `.dlog`/source; no authn expected). No RCE, no memory unsafety, no command execution, no network. Confirmed issues:

- **DoS via panics** on committed/shared input (the dominant issue) — including killing the shared MCP server.
- **Write-side symlink escape**: the read path canonicalizes and rejects escapes, but the *write* path does not — a checked-in symlink under `.decisions/` writes `.dlog`/`.dmap` outside the repo and can clobber a same-named `.dmap` in the target. A clean bypass of the project's own advertised symlink control (asymmetric: `.dlog` clobber is gated by the parse step, `.dmap` is not).

Read-path path-validation hardening is genuinely strong (traversal, drive/UNC/device, reserved Windows names, trailing dot/space all rejected; verified). The git zlib path bounds delta depth (though the cap of 32 is *below* git's default 50 — a correctness bug, not a security one).

---

## 16. Testing Assessment — **6/10**

Volume is high and the differential-against-TS strategy is the right idea. But the confidence is narrower than the count implies:

- **The differential oracle covers only TS/JS.** The Rust and C/C++ extractors — the largest *novel* code the v2 effort added — have **no independent oracle**; correctness rests on same-team unit tests. A wrong range in a `.rs` file is silently accepted (verified: a decision with a deliberately wrong line range passes `lint`).
- **No fuzzing of the hand-written parsers** — the exact gap that let four panics survive into a release candidate.
- **No concurrency/crash-injection tests** — atomicity and locking verdicts rest on code reading.
- **The scale harness exercises a best case** (tiny files, 256 KiB skip, 1 decision/file).
- **The strongest suites (property soak, full scale, corpus matrix) run only weekly/on-dispatch, not on PRs.**
- **Cross-platform (Windows/macOS/arm/musl) is CI-only and unverified here**; the Windows crash-consistency path is a different (weaker) code path that is not crash-tested.

---

## 17. Documentation Assessment — **8/10**

README, spec, and architecture docs are clear, honest, and unusually well-written (the competitive-positioning table is fair; `docs/archiva-v2-review-status.md` candidly lists remaining gaps). Two doc-vs-reality drifts matter: the quick-start's auto-wired hook doesn't work as documented, and the "scales to large repos" claim is not borne out. Otherwise documentation is a strength.

---

## 18. Developer Experience Assessment — **7/10**

Onboarding is smooth and the CLI is pleasant and consistent. DX is undercut by: the broken auto-hook surfacing a recurring error on every edit, the absence of any diagnostic/verbose mode, confusing error messages with no file path on corrupt-store scans (`file: missing required field` reads as a sentence, not a schema field), and the silent data-loss footguns (re-decide without supersede). The bones are good; the failure-mode UX needs work.

---

## 19. Operational Readiness Assessment — **4/10**

The single biggest operational gap is **observability: there is none** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery. For an unattended agent hook, any non-crash misbehavior currently requires recompiling with instrumentation to investigate. Combined with the panic class and the no-path corrupt-file errors, field diagnosability is poor.

---

## 20. Production Readiness Assessment — **5/10**

Not production-ready for general/public use today. It is usable in a controlled, single-user, small-repo, all-committed-files setting where the panic triggers and perf cliffs are unlikely. It is not ready for team use over a shared `.decisions/` tree (the panic propagation vector) or for large repos (the perf cliffs) or for the documented Claude Code auto-workflow (the hook contract).

---

## 21. Open Source Readiness Assessment — **7/10**

Strong: MIT license, clean repo, clippy-strict, formatted, CONTRIBUTING-grade docs, a real CI/validation/publish pipeline, and a thoughtful architecture doc with explicit extension points. Adoption-readiness is gated almost entirely by the production-readiness blockers above plus the missing panic-safety and fuzz gates that an open-source contributor base would expect before trusting it with their decision history. Packaging has one sharp edge: `import.meta.dirname` in the install tooling hard-requires Node ≥ 20.11 while npm doesn't enforce `engines`, so older-Node users get a cryptic postinstall crash.

---

## 22. Prioritized Findings

**Severity distribution (adversarially verified):** 0 critical *as labeled* / 17 high / 27 medium / 44 low / 9 info. 96 CONFIRMED, 1 PLAUSIBLE, **0 REFUTED**. The completeness critic argues — and I concur — that the *consolidated* "panic on committed/shared input" theme meets a **critical / release-blocking** bar even though no single line-item was labeled critical.

### Release blockers (must fix before 1.0)

| # | Finding | Location | Why it blocks |
|---|---|---|---|
| B1 | **Panic/abort class on committed input** (lone `'` → `yaml.rs:700`; mid-codepoint slice → `yaml.rs:310`; empty block scalar; deep-nesting recursion → `anchor.rs:733`) | `yaml.rs`, `anchor.rs` | One malformed byte in a shared `.dlog`/source aborts `status`/`lint`/`session-start` and **kills the MCP server**. It's a class — must be closed by fuzzing + depth bounds + a panic boundary, not point patches. |
| B2 | **PostToolUse hook is a no-op under Claude Code** (ignores stdin `file_path`) | `settings.rs:5`, `main.rs:46-51`, `cli.rs:234-258` | The core advertised automation never runs in the documented environment. |
| B3 | **Re-anchor falsely marks STALE + non-idempotent line drift** (no HEAD baseline / multiple edits per commit; ground-truth range discarded) | `project.rs:271-304`, `diff.rs:17-44` | Silent corruption of the authoritative store under normal agent activity; breaks line-based `why`. |
| B4 | **O(n²) anchor extraction** (per-file prefix re-scan) | `anchor.rs:6678`, `1729/1760` | Tens of seconds–minutes on a single large file; hooks read as hangs. |
| B5 | **One corrupt `.dlog` aborts the whole repo command** + **errors carry no file path** | `project.rs:76-82/402-408/133-149`, `error.rs:88-110` | Localized corruption blinds the whole repo; operators can't locate the bad file. |

### High (fix before or immediately after 1.0)

B6 non-atomic `.dlog`/`.dmap` write (false-failure + retry id-churn, `storage.rs:256-258`) · B7 write-side symlink escape (`paths.rs`/`storage.rs`) · B8 O(n²) hot-file writes + redundant re-parse (`storage.rs:131-133`) · B9 no logging/diagnostics anywhere · B10 Rust/C/C++ extractors have no differential oracle · B11 silent data loss on re-decide without `supersedes` · B12 MCP `why` returns confidently-wrong result for a `line` query.

### Medium (notable)

PID-liveness lock wedge · read-only `status`/`lint` take write locks · MCP tool errors as `-32000` not `isError` · `lint`/`status` exit-code taxonomy · unknown-field drop on rewrite · no schema migration · anchored-gitignore patterns never match descendants · C++ `enum class` phantom anchor · git delta-depth cap 32 < 50 · `.dmap` never read · scale harness blind to both quadratics · `lint --fix` non-atomic · Node-version postinstall crash · case-insensitive-FS identity collisions · heavy validation runs only weekly.

---

## 23. Recommended Roadmap

**Milestone 1 — Panic-safety & robustness (release-blocking, ~2 wks)**

1. Fuzz the YAML/JSON parsers (`cargo-fuzz` or in-repo property soak over arbitrary bytes); fix every panic; add a depth/recursion bound to the anchor extractor and block-scalar paths.
2. Wrap MCP per-request handling in `catch_unwind` → return `isError`; never let one request abort the server.
3. Make whole-repo commands skip-and-report corrupt files (continue, name the file) instead of aborting; attach the file path to all parse/schema/IO errors.
4. **Make "no panic on any `.dlog`/source input" and "MCP server survives a malformed request" hard PR-CI gates.**

**Milestone 2 — Core workflow correctness (release-blocking, ~1–2 wks)**

5. Parse the Claude Code hook stdin JSON (`tool_input.file_path`) in `post-tool-use`; add an integration test feeding the real payload shape.
6. Re-anchor from the extractor's ground-truth position whenever the anchor still resolves; reserve diff-shift for the orphan/incomplete case. Add a regression test asserting two consecutive `post-tool-use` runs leave `lines_hint`/STALE unchanged.
7. Detect re-decide-without-supersede and supersede-into-occupied-anchor; refuse or auto-chain into history.
8. Make `.dlog`+`.dmap` a recoverable transaction (treat a `.dmap` write failure as success-with-warning, since reads self-heal).

**Milestone 3 — Scale & observability (~1–2 wks)**

9. Eliminate the O(n²) extraction (single-pass depth tracking) and the O(n²) write path (drop the `storage.rs:133` re-parse in release; consider per-file decision bounds); add a large-single-file and a dense-single-file perf regression to the PR gate.
10. Add an env-gated stderr diagnostic channel (files scanned/skipped, lock acquire/recover, dmap repair, git-HEAD fallback).
11. Either make `status`/`session-start` actually read `.dmap`, or drop it.

**Milestone 4 — 1.0 hardening (~1–2 wks)**

12. Independent oracle for Rust/C/C++ extraction (cross-check ranges vs tree-sitter or rustc/clang spans) over a real corpus.
13. Define a schema versioning/migration policy; preserve unknown fields on rewrite.
14. Canonicalize the write path (close the symlink escape); add concurrency + crash-injection tests; promote heavy validation into the release gate; fix the lock-wedge backstop, exit-code taxonomy, gitignore anchoring, git delta-depth cap, and Node-version preflight.
15. Run the multi-process lock and differential suites on Windows/macOS CI (not just `cargo test`).

---

## Subsystem Scorecard

| Subsystem / Dimension | Score |
|---|---|
| CLI surface & dispatch | 8/10 |
| MCP stdio JSON-RPC server | 7/10 |
| Anchor extraction engine | 7/10 |
| Native Git object reader | 8/10 |
| Decision logic (validation/supersession/history) | 7/10 |
| Storage / locking / atomicity / recovery | 7/10 |
| Project workflow orchestration | 8/10 |
| Serialization (JSON/YAML/dlog/dmap) | 6/10 |
| Path validation & portability | 7/10 |
| Diff / reanchor / line-shifting | 7/10 |
| Security (cross-cutting) | 6/10 |
| Performance & scalability | 6/10 |
| Reliability / recovery / observability | 7/10 |
| Testing strategy & confidence | 7/10 |
| Release engineering & packaging | 8/10 |
| API/CLI/behavioral consistency & DX | 8/10 |
| Overall architecture & maintainability | 8/10 |

*(Per-subsystem scores reflect each module's intrinsic quality. The system-level scores below are lower because the dominant failures are cross-cutting — they emerge from interactions, e.g. parser-panic × shared-committed-store × long-lived-server.)*

### Overall scores

- **Engineering quality (unit level): 8/10** — clean, disciplined, well-tested-in-volume, idiomatic, zero-warning.
- **Production readiness: 5/10** — concentrated, reproducible blockers; safe only in a controlled single-user setting today.
- **Validation confidence: 6/10** — broad and partly differential, but blind to the exact failure classes that bite (no fuzz, no scale realism, no Rust/C oracle, no concurrency/crash injection, no real-hook integration); heavy suites are off the PR path.

---

## Final Questions — Answered Explicitly

**Does the implementation fully achieve its stated goals?**
No. It achieves the *static* goal (a fast, repo-native, zero-dependency decision store with a working CLI and MCP surface) but not the *dynamic* goal: the automatic agent workflow it is built around (auto re-anchor on edit) is broken end-to-end under real Claude Code and corrupts line attribution even when invoked correctly, and the "scales to large repos" claim is not substantiated.

**Is the architecture appropriate for long-term evolution?**
Mostly yes. Module boundaries, ownership, and extension points are sound. The two evolution gaps — no schema migration path and the zero-dependency stance concentrating un-fuzzed parser risk — are addressable without restructuring.

**Is the implementation internally consistent?**
Largely, with confirmed exceptions: CLI vs MCP `why` line semantics, MCP error encoding vs spec, and exit-code conventions. These are localized.

**Would you approve this for a stable public release?**
No. The panic class on committed/shared input, the broken auto-hook, the re-anchor corruption, and the absence of observability are release-blocking for a stable 1.0.

**Would you approve it as the reference implementation?**
Not yet. A reference implementation must be panic-safe against its own committed data format and must have the core workflow proven end-to-end. Once Milestones 1–2 land and panic-safety + real-hook integration are CI gates, it is a credible reference candidate.

**Highest-priority improvements before release?**
(1) Fuzz and bound the parsers + add a panic boundary + skip-and-report corrupt files; (2) fix the PostToolUse stdin contract; (3) make re-anchoring idempotent from ground-truth positions; (4) fix the O(n²) extraction/write paths; (5) add a diagnostic logging channel. In that order.

**What work remains before 1.0?**
Milestones 1–4 above. Realistically 4–8 focused weeks. The critical path is panic-safety + workflow correctness (M1–M2).

**What risks remain after release (assuming blockers fixed)?**
Cross-platform behavior (Windows/macOS/arm/musl) remains CI-only and the Windows crash-consistency path is weaker; the Rust/C/C++ extractors stay oracle-light until an independent ground truth exists; the hand-rolled git/DEFLATE code is a maintenance hotspot that needs an ongoing differential-against-`git` fuzz harness; and concurrency/crash-consistency guarantees rest on reasoning until fault-injection tests exist.

**How does engineering quality compare with mature, well-regarded OSS in the same domain?**
At the *craft* level (code cleanliness, lint discipline, documentation, the differential-testing instinct) it compares favorably with well-run early-stage OSS and exceeds many. At the *production-hardening* level it is behind mature tools like ripgrep, gitoxide/`gix`, or tree-sitter, which earned trust through extensive fuzzing, fault injection, and battle-tested robustness against adversarial input — exactly the layer Archiva has not yet built. It has the bones of a top-tier project and an unusually honest self-assessment; it has not yet done the hardening that separates a strong prototype from a definitive reference.

---
---

# APPENDIX — Full Verified Findings

Every finding below survived independent adversarial verification (reproduction against the release binary or direct code read). `verdict` is the second agent's call; `severity` is the corrected severity. Findings are grouped by subsystem/dimension, then sorted high → info.

). Readback -> `Chose: trailing` (trailing space dropped).
3) chose="\ttab-start" (sent as JSON \t escape; a raw tab byte is correctly rejected by the JSON parser as "Unescaped control character"): dlog on disk = `    chose: ^Itab-start` (tab preserved bare). Readback -> `Chose: tab-start` (tab dropped).
All three confirm the writer emits whitespace-bearing values bare and the parser (trim_start/trim_end) silently drops the whitespace on read, exactly as claimed.

**Verifier notes / severity correction:** Fully confirmed at the cited file:lines with matching empirical repro. The claim's mechanism is precise: round-trip data loss on first write, the writer renders whitespace bare and the parser strips it, and write_dlog's validate-only re-parse cannot catch it. Severity medium is appropriate and not inflated: it is genuine silent data loss affecting every string field uniformly, but real-world likelihood of significance is LOW because the affected fields (chose/because/anchor/reason free text) rarely carry semantically meaningful leading/trailing whitespace or tabs, and the JSON layer already rejects raw control chars so a tab only enters via an explicit \t escape. One small correction to the claimed evidence wording: for the tab case the writer does NOT drop it ("chose: <tab>tab-start" is preserved bare on disk); it is solely the parser's trim_start that drops it on read — consistent with the claim's own assertion that "the parser, not the writer, drops it." Recommended fix: extend needs_single_quotes() to return true when value != value.trim() (i.e. has leading/trailing ASCII whitespace, including tabs), and add a value-equality assertion in write_dlog's re-parse round-trip rather than a parse-only check.

**Recommended resolution:** Quote (single- or double-quoted) any scalar with leading/trailing whitespace or control/tab characters in needs_single_quotes; and/or make write_dlog assert round-trip equality (parse(render(x)) == x) and error instead of silently writing lossy output.

---

### F45. [MEDIUM] Folded-block (>-) rendering collapses internal whitespace runs and re-indents, mutating stored text

`defect` · location `src/core/yaml.rs:934-961 (render_folded_block / wrap_words) and yaml.rs:1046-1069 (fold_block_scalar)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Any string >100 chars (without newlines) is rendered as a YAML folded block via wrap_words, which uses split_whitespace() — collapsing every run of spaces/tabs to a single space and dropping line-leading whitespace. On read, fold_block_scalar joins lines with single spaces. The result: multi-space runs and intentional alignment in long strings are permanently lost. Separately, the literal-block (|-) reader keys block indent off the first line and byte-slices later lines, so a continuation line with less indentation than the first loses leading characters (4 chars eaten in the test below).

**Why it matters:** Long because/reason text containing code, ASCII tables, or aligned lists is silently reformatted. Combined with the no-equality-check write path, the corruption is invisible.

**Impact:** Data loss / mutation for long or formatted text fields.

**Likelihood:** Medium — because fields commonly exceed 100 chars; the multi-space subset is narrower but real for snippets/tables.

**Evidence (reporter):** Folded: /tmp/inj3 wrote a >100-char because with double spaces between every word; readback collapsed all to single spaces (`cat -A` confirms single spaces). Literal: /tmp/inj5 wrote because='    deep first\nshallow second' -> readback 'deep first\nlow second' (4 chars silently removed from line 2). Single-spaced prose >100 chars round-trips correctly (verified), scoping the damage to non-standard spacing.

**Independent verification:** Read src/core/yaml.rs and reproduced all three sub-claims against /home/ubuntu/archaeo/target/release/archiva.

CODE PATHS:
- render_scalar (yaml.rs:908-922): String with '\n' -> render_literal_block (|-); String len>100 -> render_folded_block (>-). So any >100-char single-line `because`/`chose` is forced into a folded block.
- render_folded_block (yaml.rs:934-942) calls wrap_words (944-961), which iterates `value.split_whitespace()` — this collapses every run of spaces/tabs to a single space and drops line-leading whitespace, then rejoins words with a single ' '. The mutation happens at WRITE time into the authoritative .dlog, not just on read.
- Read side fold_block_scalar (yaml.rs:1046-1069) joins non-blank lines with single ' ' (line.trim_end()).
- Literal reader parse_block_scalar (yaml.rs:291-325): block_indent is set from the FIRST non-empty line's indent (get_or_insert at :309), then later lines are byte-sliced raw.text[content_indent..] (:310-314). A continuation line indented LESS than the first silently loses its leading bytes.

REPRODUCTION 1 (folded, double spaces) — /tmp/injtest, because = 'word00  word01  ...word19' (158 chars, double spaces). Authoritative .dlog stores `because: >-` with single spaces only; readback `archiva why app.ts fn:run` (cat -A) shows single spaces. All 19 double-space runs collapsed. Mutation is persisted in the .dlog itself.

REPRODUCTION 2 (literal, under-indented continuation) — /tmp/lit, because = '    deep first\nshallow second'. .dlog renders |- with line1 indented 4 extra spaces ("    deep first" under a 6-space base => 10 leading) and line2 at base indent (6). On read, content_indent is keyed off line1 (10), so line2 ("      shallow second", 6 spaces) is sliced [10..] => "low second". Readback shows: "Because: deep first\nlow second" — 4 chars ("shal") silently removed.

REPRODUCTION 3 (control) — /tmp/ctrl, single-spaced 139-char prose: ROUNDTRIP OK: True. Confirms damage is scoped to non-standard spacing / under-indented continuations, exactly as claimed.

**Verifier notes / severity correction:** Claim is accurate in mechanism, location, and scope. Two clarifications that strengthen (not weaken) it:

1) The folded-block damage occurs at WRITE time into the authoritative .dlog (render_folded_block/wrap_words), so it is true data mutation in the source of truth, not merely a read-rendering artifact. Re-reads are stable but already lossy.

2) The literal-block bug is more precisely an indent-detection flaw: content_indent is fixed from the first non-empty line, so ANY continuation line with smaller indentation than the first has that many leading bytes truncated (4 here). It also makes the byte-slice raw.text[content_indent..] a potential UTF-8 char-boundary panic risk if content_indent lands mid-multibyte-char on a short line, though I did not trigger a panic in these tests.

Severity medium is appropriate and correctly scoped: it is silent mutation/loss in the authoritative store, but only for fields containing multi-space runs, intentional alignment, or under-indented multi-line continuations. The affected fields (because/chose) are human/agent prose where exact internal whitespace is rarely load-bearing, and standard single-spaced text round-trips correctly. Not critical (no corruption of IDs/anchors/structure, no crash observed), but a genuine correctness defect in a tool whose entire value proposition is faithfully preserving decision rationale. Confidence: high.

**Recommended resolution:** For folded blocks, only fold strings that are safe to fold (no significant internal whitespace), otherwise use a literal block or quoting that preserves bytes. For the literal-block reader, strip leading whitespace per-line safely rather than byte-slicing at the first line's indent. Add round-trip equality assertion in write_dlog as defense in depth.

---

### F46. [LOW] .dmap status-suffix encoding is ambiguous for anchors ending in a status keyword (round-trip break)

`defect` · location `src/core/dmap.rs:122-152 (parse_line last-colon heuristic) and dmap.rs:98-120 (render_dmap)` · reporter-confidence high · verification **CONFIRMED**

**Description:** render_dmap emits `start-end:anchor[:STATUS]`. parse_line splits the range at the FIRST colon, then treats the substring after the LAST colon of the remainder as a status iff it parses as UNDECIDED/STALE/ORPHAN. An anchor whose final segment is exactly STALE/ORPHAN/UNDECIDED with no status renders to a line that re-parses as a different anchor plus a phantom status. assert_anchor_exists permits such anchors (a function literally named STALE), so they are producible.

**Why it matters:** The .dmap is described as the compact index that agents are instructed to read at session start. An ambiguous line misrepresents both the anchor identity and its decision status to a human or agent reading the file.

**Impact:** On-disk index is wrong/ambiguous; a consumer reading .dmap sees anchor 'fn' with status STALE when the truth is anchor 'fn:STALE' with no status. Live data-corruption blast radius is limited because dlog is authoritative and parse_dmap is reached in production only via load_dmap, which currently has no non-test callers.

**Likelihood:** Low — requires a code symbol named exactly STALE/ORPHAN/UNDECIDED.

**Evidence (reporter):** /tmp/inj2: `archiva write-decision` for a Rust `pub fn STALE()` with anchor fn:STALE succeeded; .dmap on disk = `1-3:fn:STALE`. Standalone harness over dmap.rs: render(anchor='fn:STALE',status=None)='1-3:fn:STALE' -> parse -> anchor='fn', status=Some(Stale), roundtrip_ok=false; same for fn:UNDECIDED. Control anchor 'block:ORPHAN' WITH status STALE round-trips fine.

**Independent verification:** Read src/core/dmap.rs:98-152 directly: render_dmap appends ":STATUS" only when status.is_some() (L113-116); parse_line splits range at FIRST colon (L123), then treats substring after LAST colon of remainder as status iff DecisionStatus::parse succeeds (L128-138). This means render(anchor="fn:STALE", status=None) -> "1-3:fn:STALE" -> parse -> anchor="fn", status=Some(Stale).

Reproduced the round-trip break with a real unit test appended to dmap.rs and run via `cargo test --lib`: FAILED for fn:STALE/fn:UNDECIDED/fn:ORPHAN. Output: name="fn:STALE" rendered="1-3:fn:STALE\n" parsed_anchor="fn" parsed_status=Some(Stale); assertion left=DmapEntry{anchor:"fn",status:Some(Stale)} != right=DmapEntry{anchor:"fn:STALE",status:None}. (Test reverted afterward via git checkout.)

Reproduced producibility with the release binary in /tmp scratch project: `archiva write-decision --json '{"file":"lib.rs","anchor":"fn:STALE","lines":[1,1],"chose":"keep this","because":"test","rejected":[]}'` against source `pub fn STALE() {}` -> "Recorded dec_001." On-disk .decisions/lib.rs.dmap = "1-1:fn:STALE"; .dlog shows anchor `fn:STALE` with no status. assert_anchor_exists (anchor.rs:347-352) permits the anchor.

Blast-radius verified: `grep -rn load_dmap src/` shows it referenced ONLY inside src/core/storage.rs (definition + tests) — no production callers. parse_dmap is reached in non-test code only via load_dmap. Write path (render_dmap_from_dlog/write_dmap/ensure_dmap_current) IS live via src/core/project.rs (L79,329,405,425,546), so the ambiguous string is genuinely written to disk in production, but nothing reads it back through parse_dmap. Additional nuance: load_dmap's self-heal (storage.rs:82) compares the on-disk string to render_dmap_from_dlog, which renders the SAME ambiguous "1-1:fn:STALE", so even if load_dmap were wired in, content==expected, repair would not trigger, and it would silently return the mis-parsed entry.

**Verifier notes / severity correction:** Claim is accurate in mechanism, producibility, location, and impact scoping; no corrections needed. Severity low is correct: the on-disk .dmap is genuinely ambiguous/wrong for anchors whose final colon-segment is exactly STALE/UNDECIDED/ORPHAN (a real round-trip/encoding defect), but there is no production read path through parse_dmap today (load_dmap has zero non-test callers), and dlog is authoritative. Recommended resolution: make .dmap encoding unambiguous, e.g. always emit a status field (use a NONE/active sentinel) or escape/delimit the optional status so it cannot collide with a same-named anchor segment; alternatively have parse_line reconstruct against the dlog anchor set rather than a last-colon heuristic. A defensive note: the self-heal string-compare in load_dmap (storage.rs:82) would not detect this class of corruption because render produces the identical ambiguous string, so a parse-vs-derive comparison would be more robust than the current string comparison if load_dmap is ever exposed to a read path.

**Recommended resolution:** Make the dmap encoding unambiguous: either escape/percent-encode the anchor, use a delimiter that cannot appear in anchors, or write the status with a sentinel that cannot collide (e.g. a leading marker). At minimum add a render->parse->render property test over anchors ending in status keywords.

---

### F47. [LOW] Unknown schema-1 fields are silently dropped on every dlog rewrite

`tradeoff` · location `src/core/dlog.rs:62-127 (parse_decision_record reads only known keys) and storage.rs:131-135 (write_dlog re-renders from the typed model)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The dlog parser reads only the known schema-1 fields into a typed struct; the renderer emits only those fields. Any additional top-level or per-decision keys present on disk are dropped the next time the file is rewritten (e.g. post-tool-use re-anchoring, supersede, status updates).

**Why it matters:** This is documented as intentional schema-1 strictness, but it means there is no forward compatibility: a newer producer's extra fields, or user annotations, vanish without warning after any Archiva write.

**Impact:** Loss of any out-of-schema data after a rewrite. Acceptable by design but worth explicit acknowledgement as an operational risk for mixed-version or annotated repos.

**Likelihood:** High when extra fields exist; otherwise N/A.

**Evidence (reporter):** /tmp/unk: hand-wrote a dlog with custom_top_level and per-decision custom_field, ran `hooks post-tool-use src/a.rs`; both custom fields were gone from the rewritten file (and status/stale_since were added).

**Independent verification:** Code matches the claim exactly. src/core/dlog.rs:29-44 defines DecisionRecord with only typed schema-1 fields and no catch-all/extra map; DlogFile (47-51) holds only file/schema/decisions. parse_decision_record (88-127) reads only known keys; decision_to_yaml (214-253) and dlog_to_yaml (198-212) emit only those known keys. grep for extra/unknown/passthrough/HashMap in dlog.rs shows no preservation field (only test fixtures). storage.rs:131-135 write_dlog re-renders purely from the typed model (render_dlog_yaml -> reparse roundtrip -> atomic_write).

Empirical reproduction in /tmp/unkaudit:
1. git init, wrote src/a.rs, ran `archiva init`, then wrote decision via stdin JSON {"file":"src/a.rs","anchor":"fn:main","lines":[1,4],"chose":"option A","because":"reason A","rejected":[]} -> "Recorded dec_001."
2. Hand-edited .decisions/src/a.rs.dlog to add top-level `custom_top_level: keep_me_please` and per-decision `custom_field: annotate_me`. The file still parsed fine (no error).
3. Modified src/a.rs and ran `archiva hooks post-tool-use src/a.rs` -> "Re-anchored src/a.rs: 1 stale, 0 orphan."
4. Re-read the dlog: BOTH custom_top_level and custom_field were GONE, and `status: STALE` + `stale_since: '...'` were added — exactly the claimed behavior.

**Verifier notes / severity correction:** Claim is accurate in description, location, evidence, and severity. category=tradeoff / severity=low is correct: the parser tolerates unknown keys on read (does not reject mixed-version files) but the typed model cannot retain them, so any rewrite path (post-tool-use re-anchor, supersede, status/stale updates, file move) silently discards out-of-schema top-level and per-decision keys. This is acceptable-by-design lossy normalization, not a correctness defect — recommend documenting it as an operational risk for mixed-version or externally-annotated .decisions repos, and optionally adding an `extra: OrderedMap<String, YamlValue>` passthrough on DecisionRecord/DlogFile if forward-compat preservation is ever desired. No scope/severity correction needed.

**Recommended resolution:** If strictness is intended, document it and consider rejecting (or warning on) unknown fields at parse time so silent loss is at least surfaced. If forward-compat matters, preserve unknown fields through a passthrough map.

---

### F48. [LOW] YAML subset does not support anchors/aliases; js-yaml-produced anchored values are misread as literals

`techdebt` · location `src/core/yaml.rs:392-432 (parse_scalar_value has no &/* handling)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** The parser treats `&anchor value` and `*alias` as ordinary plain scalars rather than YAML anchor definitions/references. js-yaml (the TS reference implementation) supports these, so a .dlog written by the original TypeScript tool using anchors/aliases would be parsed incorrectly by the Rust reader.

**Why it matters:** The project's stated goal is parity with the TS/js-yaml implementation for reading existing decision logs. Anchored YAML silently yields wrong field values rather than an error.

**Impact:** Silent misread of cross-implementation dlogs that use YAML anchors. The Rust writer never emits anchors, so self-produced files are unaffected.

**Likelihood:** Low — js-yaml dump does not emit anchors for these simple structures by default.

**Evidence (reporter):** /tmp/yutf crafted dlog with `id: &x dec_001` and `chose: *x`; `archiva why` returned id='&x dec_001' and Chose='*x' (literal), RC=0, no error.

**Independent verification:** Code read — /home/ubuntu/archaeo/src/core/yaml.rs:392-432 (parse_scalar_value): the scalar resolution chain handles [], [..], {}, {..}, null/~, true/false, single-quoted, double-quoted, i64, then falls through to YamlValue::String. There is NO branch for a leading '&' (anchor definition) or '*' (alias reference). Grep of the whole file for "anchor"/"alias"/'&'/'*' handling found only: line 966 (needs_single_quotes lists '&' and '*' among chars that force quoting on WRITE) and a test fixture string "empty alias" at lines 1299/1318/1327 (unrelated — it's literal test content, not anchor logic). No anchor map is ever built or consulted in the parser.

Runtime reproduction — scratch dir /tmp/yutf (git init + archiva init, real src/db.rs, dec_001 written). I overwrote .decisions/src/db.rs.dlog so id was "&x dec_001" and chose was "*x", then ran the release binary:
  $ /home/ubuntu/archaeo/target/release/archiva why src/db.rs
  fn:main &x dec_001 (lines 1-3)
  Chose: *x
  Because: ACID compliance
  Recorded: 2026-06-30T20:28:45.024Z
  RC=0
The anchor token "&x" was retained verbatim in the id and the alias "*x" was printed literally instead of resolving to "dec_001". No warning, no error, exit 0 — exactly the claimed silent misread.

Self-produced-safe claim verified — wrote a decision with chose='&weird *value'. The writer emitted `chose: '&weird *value'` (single-quoted because needs_single_quotes flags leading '&'/'*' at yaml.rs:965-966) and `archiva why` round-tripped it back to `&weird *value` correctly. So Rust-written .dlog files never contain unquoted anchors/aliases and are unaffected.

**Verifier notes / severity correction:** Claim is accurate in mechanism, location (src/core/yaml.rs:392-432), and reproduction. Severity low / category techdebt is correct; I'd lean toward info. The real-world cross-implementation exposure is narrower than a bare "anchors aren't supported" framing suggests: js-yaml's dump() only emits &anchor/*alias for repeated OBJECT/ARRAY references (and only when noRefs=false), not for duplicate scalar strings. The decision schema (.dlog) is overwhelmingly flat scalars plus arrays of primitives, which rarely yields shared non-scalar references, so a js-yaml-produced .dlog containing anchors is an edge case rather than a routine occurrence. Combined with the fact that the Rust writer never emits anchors (self-produced files always safe), the practical blast radius is minimal — it would only bite when ingesting a hand-edited or atypically-serialized external .dlog. Recommended resolution as stated is reasonable: either (a) detect a leading '&'/'*' in a plain scalar and return a YamlError("anchors/aliases unsupported") so the misread is loud rather than silent, or (b) implement minimal anchor/alias resolution. Given the low likelihood, option (a) — failing loudly — is the lower-cost correct fix.

**Recommended resolution:** Either document anchors/aliases as explicitly unsupported and reject `&`/`*`-leading scalars with a clear error, or implement minimal anchor resolution. Rejecting is safer than silently misreading.

---

## Path validation and portability  — score 7/10

> The project-relative path validator (paths.rs) is solid on the classic traversal surface: I verified by running the release binary that `..` segments, internal `./`, absolute paths, drive prefixes (`C:/`), UNC/device prefixes (`//`, `\\`, `\\?\`), NUL bytes, empty segments, Windows-invalid chars, trailing dot/space, and reserved device names are all rejected, and that `./`, `.//`, and `\` normalize to a single forward-slash identity. The READ path additionally canonicalizes and rejects symlink escapes (EscapesProjectRoot). However the WRITE path to `.decisions/` is asymmetric: dlog/dmap paths are built with a plain `project_root.join(".decisions").join(..)` and never canonicalized, so a symlinked subdirectory under `.decisions/` redirects writes outside the project root — I reproduced files landing in `/tmp/arch-evil`. The gitignore matcher (gitignore.rs) is a faithful 1:1 port of the TS reference but diverges sharply from real git semantics: negation (`!`) is parsed then ignored, anchored patterns never match descendants (a dead `dir_only` branch makes `/src/` and `/src/generated/` match nothing), and slash-bearing unanchored patterns require a full-path match. Because this matcher only gates the `lint` complexity scan, the net effect is files being silently hidden from (or wrongly exposed to) the scan. Separately, Windows-name hardening is applied unconditionally on all platforms, making legitimate Unix files like `src/aux.ts` permanently unaddressable, and there is no case-folding or Unicode normalization, so identity can split or collide on case-insensitive/normalizing filesystems.

*Score rationale:* The core traversal/absolute/drive/UNC/NUL/empty-segment/reserved-name validation is correct, thorough, and well-tested, and the READ path's symlink canonicalization is a genuine strength. Points are deducted for: (1) an asymmetric WRITE path that fails to canonicalize .decisions/ and lets a symlinked subdir escape the project root — the most serious gap; (2) a gitignore matcher that, while a faithful port of the TS reference, diverges materially from real git semantics (negation ignored, anchored patterns match nothing, directory ignores ineffective), silently hiding/exposing files in the lint scan; and (3) portability tradeoffs (Windows checks on all platforms, no case/Unicode folding) that are defensible but lossy and undocumented at the point of failure.

**Verified behaviors (checked, not assumed):**

- Ran the release binary: traversal (../, ../../etc/passwd, src/../../../etc/x), absolute (/etc/passwd), drive (C:/, C:\), UNC/device (//, \\), dot segment (src/./a.ts), empty segment (src//a.ts, src/), Windows-invalid chars (a:b, a?, a*), trailing dot/space, and reserved names (CON, NUL, COM1, LPT9) are all rejected with specific PathErrorKind messages.
- Confirmed ./, .//, \ normalize to a single forward-slash identity and the happy path records dec_001 to .decisions/src/a.ts.dlog + .dmap.
- Reproduced WRITE escape: a directory symlink .decisions/sym -> /tmp/arch-evil caused write-decision to create a.ts.dlog/.dmap inside /tmp/arch-evil (outside the project root).
- Confirmed READ symlink escape IS blocked: source file symlinked to /tmp/outside_src/o.ts rejected with 'path resolves outside the project root' (EscapesProjectRoot).
- Reproduced gitignore negation bug: '*.ts' + '!root-only.ts' yields 'No decision issues found' (root-only.ts hidden from lint scan).
- Reproduced anchored/dir-only gitignore failures: '/src/', '/src/generated/', and 'src/generated' all fail to exclude their targets from the lint scan; only 'generated/' and '*.test.ts' work; 'src/**' and '**/g.ts' behave per the glob engine.
- Confirmed Windows reserved/invalid-char rejection fires on Linux for src/aux.ts, src/nul.ts, src/con.ts, src/prn.ts.
- Confirmed case (src/Case.ts vs src/case.ts) and Unicode (café.ts) produce independent identities/dlogs with no folding; overlong (300-char) names pass lexical validation and fail with the generic 'path validation failed' (PathErrorKind::Io).
- Confirmed gitignore.rs is a 1:1 port of the TS reference src/core/gitignore.ts, so the matcher divergences are inherited parity quirks, not Rust regressions.

### F49. [HIGH] Write path to .decisions/ is not canonicalized — a symlinked subdir escapes the project root

`defect` · location `src/core/paths.rs:103-117 (decision_base_path/dlog_path/dmap_path) and src/core/storage.rs:131-142 (write_dlog/write_dmap)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Read access resolves source files through canonical_source_path_if_exists (paths.rs:142-167), which canonicalizes and rejects results that escape the canonicalized root (EscapesProjectRoot). The write side has no equivalent defense: dlog_path/dmap_path/decision_lock_path are built with a literal project_root.join(".decisions").join(relative.to_path_buf()) and handed straight to atomic_write_text. atomic_write_bytes_impl (fs.rs) calls ensure_parent_dir (create_dir_all, which follows symlinks) then writes a temp sibling and renames into that directory. So if a directory under .decisions/ is a symlink, the dlog/dmap files are written into the symlink target, outside the project. This contradicts the system's own symlink threat model (the read side explicitly defends against it).

**Why it matters:** It breaks the core containment guarantee that all decision artifacts live under .decisions/ inside the repo. git preserves symlinks, so a malicious or mis-crafted repo can ship a checked-in symlink (e.g. .decisions/sub -> /abs/target); a victim who clones it and runs `archiva write-decision`, `hooks post-tool-use`, or `lint` (which can rewrite dmaps) writes attacker-directed dlog/dmap files outside the repo and can clobber an existing same-named file in the target directory.

**Impact:** Out-of-tree writes of structured YAML/dmap content; potential clobber of an existing file whose name matches `<source>.dlog`/`.dmap` in the symlink target directory. Content is constrained to dlog/dmap format, not arbitrary bytes.

**Likelihood:** Requires a pre-existing symlink under .decisions/. Realistic via a checked-in repo symlink; not reachable from CLI input alone.

**Evidence (reporter):** Reproduced in /tmp/arch-audit: `ln -s /tmp/arch-evil .decisions/sym; mkdir sym; echo source>sym/a.ts; archiva write-decision --json '{"file":"sym/a.ts",...}'` => 'Recorded dec_001.' and `ls /tmp/arch-evil` shows a.ts.dlog and a.ts.dmap created there. Contrast: a source symlink escaping the root (`ln -s /tmp/outside_src/o.ts linked.ts`; file:"linked.ts") is correctly blocked with 'path resolves outside the project root'.

**Independent verification:** Code reads match the citation exactly. paths.rs:103-117 build decision_base/dlog/dmap/lock paths as a literal project_root.join(".decisions").join(relative) with no canonicalization. storage.rs:131-142 (write_dlog/write_dmap) hand those raw paths straight to atomic_write_text. fs.rs:42-53 ensure_parent_dir calls fs::create_dir_all (follows symlinks) and fs.rs:115-120 atomic_write_bytes_impl writes a temp sibling + rename into that directory. The read side (project.rs:623/628 -> paths.rs:142-167 canonical_source_path_if_exists) canonicalizes and returns EscapesProjectRoot; the write side has no equivalent.

Reproduction (binary /home/ubuntu/archaeo/target/release/archiva):
1. /tmp/arch2: `archiva init`; `ln -s /tmp/evil2 .decisions/sym`; `mkdir sym`; `echo source >sym/a.ts`.
2. `archiva write-decision --json '{"file":"sym/a.ts","anchor":"fn:foo","lines":[1,1],"chose":"X","because":"Y","rejected":[]}'` => "Recorded dec_001." EXIT 0.
3. `ls -laR /tmp/arch2/.decisions/` shows ONLY the `sym` symlink (no files). `ls -laR /tmp/evil2/` shows a.ts.dlog (227 B) and a.ts.dmap (11 B) — written OUTSIDE the project root. Confirmed dlog body contains valid `file: sym/a.ts / schema: 1 / decisions: ...`.
4. Contrast confirmed: a source symlink escaping root (`ln -s /tmp/outside_src/o.ts linked.ts`; file:"linked.ts") is blocked with `Invalid project-relative path "linked.ts": path resolves outside the project root` EXIT 1.

Clobber check: in /tmp/arch4 I pre-planted `/tmp/evil4/y.ts.dmap` = "VICTIM DMAP DATA - DO NOT OVERWRITE", then wrote a decision for sub/y.ts via the symlinked .decisions/sub. The victim dmap was overwritten with `1-1:fn:qux` (clobber CONFIRMED for the dmap path). The .dlog clobber is partially gated: a pre-existing non-dlog file at the dlog path makes the load/parse step error before the write, so arbitrary .dlog clobber is blocked, but .dmap clobber is not.

**Verifier notes / severity correction:** Claim is accurate in mechanism, location, and reproduction. One correction to the impact wording: the clobber is asymmetric — the .dmap target is overwritten unconditionally (verified), but the .dlog target is protected by load_dlog's parse step (a pre-existing non-dlog file there causes an error before any write), so .dlog clobber of arbitrary victim files does not generally succeed. Out-of-tree writes themselves succeed for both files.

Severity: high is defensible because this is a write-side bypass of the exact symlink-escape control the project advertises ("path validation hardening") and explicitly enforces on the read side — a documented-threat-model asymmetry. However, two factors temper real-world exploitability toward medium: (1) the precondition is an attacker-controlled symlink already planted inside the repo's .decisions/ directory, which usually implies the attacker already has project write access; (2) written content is format-constrained to dlog/dmap, not arbitrary bytes. I'd accept high or medium; leaning high given it is a clean bypass of a security control the tool claims.

Recommended resolution: canonicalize the decision target's parent directory before writing (or reject when any component of .decisions/<relative> is a symlink that escapes the canonicalized .decisions root), mirroring canonical_source_path_if_exists on the write path in dlog_path/dmap_path/decision_lock_path consumers (write_dlog/write_dmap/lock acquisition).

**Recommended resolution:** Mirror the read-side hardening on writes: after computing decision_base_path, canonicalize the existing ancestor portion of the .decisions/ target (or canonicalize .decisions itself and verify each created component is not a symlink), and reject when the resolved write target does not start_with the canonicalized project_root/.decisions. Alternatively refuse to traverse symlinked directories when materializing the .decisions/ tree.

---

### F50. [MEDIUM] Anchored gitignore patterns never match descendants; dir-only branch is dead when anchored

`defect` · location `src/core/gitignore.rs:50-67` · reporter-confidence high · verification **CONFIRMED**

**Description:** match_pattern sets anchored = starts_with('/') and dir_only = ends_with('/'), strips both, then at line 61-63 does `if anchored { return glob.matches(normalized) }` — returning before the dir_only branch (line 64-66). glob.matches requires a full-string match against the whole relative path. So `/src/` becomes body 'src' matched against 'src/a.ts' => false; it ignores nothing. `/src/generated/` becomes 'src/generated' vs 'src/generated/g.ts' => false. Real git anchors these to the root and ignores everything beneath the directory. Unanchored slash-bearing patterns (e.g. `src/generated`) hit line 67 which also requires a full-path or single-segment match and likewise fail to ignore `src/generated/g.ts`, whereas real git anchors any slash-containing pattern to the root and matches the directory. Faithful port of gitignore.ts:42-48, so inherited parity quirk.

**Why it matters:** Directory-ignore patterns are the most common way to exclude generated/vendored trees from scanning. Here they have no effect, so the lint complexity scan walks and reports files the user explicitly tried to exclude (false positives / noise) and, combined with the negation bug, the matcher's behavior is broadly unreliable for anything beyond a bare filename or `dir/` (unanchored, no leading slash) or `*.ext`.

**Impact:** User-specified directory excludes are ignored by the scan; only unanchored `name`, `dir/`, and simple globs work as intended.

**Likelihood:** High — anchored and path-style ignores are standard.

**Evidence (reporter):** In /tmp/gi: `.gitignore`=`/src/` leaves all four src files in the scan; `=/src/generated/` and `=src/generated` both leave src/generated/g.ts in the scan; only `generated/` (dir-only, unanchored) and `*.test.ts` correctly drop their targets. The anchored return at gitignore.rs:62 precedes the dir_only branch at line 64.

**Independent verification:** Code at src/core/gitignore.rs:49-67 matches the claim verbatim. match_pattern sets anchored=starts_with('/') (line 50), dir_only=ends_with('/') (line 51), strips both (53-58), parses the remaining body into a Glob, then at line 61-62 `if anchored { return glob.matches(normalized) }` returns BEFORE the dir_only branch at 64-66. Glob::matches is a full-string match (matches_from terminal case at line 152-153 requires char_index == chars.len()), so anchored dir patterns can never match a descendant path.

Reproduced end-to-end with /home/ubuntu/archaeo/target/release/archiva in /tmp/gi (project with src/a.ts, src/generated/g.ts, src/keep.test.ts, all complexity-6 undecided). Baseline lint (empty .gitignore) reports all three files. Per-pattern lint scan:
- .gitignore=`/src/`        -> still lists src/a.ts, src/generated/g.ts, src/keep.test.ts (ignores NOTHING)
- .gitignore=`/src/generated/` -> still lists all three incl. src/generated/g.ts (ignores NOTHING)
- .gitignore=`src/generated`   -> still lists all three incl. src/generated/g.ts (ignores NOTHING)
- .gitignore=`generated/` (dir-only, unanchored) -> correctly drops src/generated/g.ts
- .gitignore=`*.test.ts` (simple glob) -> correctly drops src/keep.test.ts

This exactly matches the claimed behavior: anchored excludes and slash-bearing unanchored excludes are silently ignored; only unanchored name, dir/, and simple globs work.

Parity confirmed: src/core/gitignore.ts:34-49 is the original. TS matchPattern returns `regex.test(normalized)` at line 42 for anchored, before the dirOnly branch at 44-46, using a `^...# Archiva v2 — Release-Readiness Audit

**Auditor role:** Independent Principal Software Architect / Release Auditor
**Subject:** Archiva v2 — std-only Rust re-engineering of a TypeScript "decision memory for AI coding agents" tool
**Date:** 2026-07-01
**Branch audited:** `codex/archiva-v2-rust-validation` (HEAD `33f160e`, version 0.2.0)

**Method:** 115 independent agents across 17 subsystem/dimension reviews; every substantive finding adversarially re-verified by a second agent that reproduced it against the compiled release binary or read the cited code. Documentation and the team's own `docs/archiva-v2-review-status.md` were treated as **unverified claims**, not evidence.

**Independently verified baseline (by the auditor before fan-out):**

- `cargo build --release` — clean.
- `cargo clippy --all-targets --locked -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `cargo test` — 301 lib tests pass (1 ignored) + 9 + 1 + 3 integration tests pass.
- Binary functional: `archiva --version` → `0.2.0`; `archiva status` on the repo → 537 decisions, 0 stale, 0 orphan, 0 issues across 56 `.dlog` files.

**Verification outcome across the panel:** 96 findings `CONFIRMED`, 1 `PLAUSIBLE`, **0 `REFUTED`**. Severity distribution (corrected, non-refuted): **17 high / 27 medium / 44 low / 9 info**, plus 10 audit-coverage gaps from a completeness critic.

---

## 1. Executive Summary

Archiva v2 is a genuinely impressive piece of engineering: a zero-dependency, std-only Rust implementation of a multi-language anchor extractor, a from-scratch git object reader (SHA-1 **and** SHA-256, packs, deltas, alternates), hand-written JSON/YAML parsers, a CLI, and a stdio MCP server — all clean-compiling, clippy-strict, well-formatted, and backed by 300+ tests and an elaborate differential/stress/scale/corpus validation harness. The code quality at the unit level is high and the discipline is real.

**But it is not ready for a stable 1.0 public release, and it is not yet the reference implementation.** The audit confirmed a coherent cluster of release-blocking problems that the existing test strategy structurally cannot see:

1. **A class of trivially-reachable process aborts (panics / stack overflows) triggered by ordinary committed, team-shared data.** Three distinct crashes were reproduced (a lone `'` in a `.dlog` → `yaml.rs:700`; a mid-codepoint UTF-8 slice → `yaml.rs:311`; deeply-nested source → unbounded recursion in the anchor extractor), and the completeness critic found a **fourth** (empty block scalar) in minutes. Because `.decisions/` is git-tracked by default and shared across a team, a single malformed byte in one file aborts `status`, `lint`, the per-session `session-start` hook, and — most seriously — **kills the long-lived MCP server mid-session**, dropping all in-flight agent context. This is a class, not a list.

2. **The product's headline automatic workflow is broken end-to-end.** The auto-wired `PostToolUse` re-anchor hook is a confirmed no-op under real Claude Code: `init` wires it with no argument relying on `ARCHIVA_FILE`, but Claude Code delivers the edited path as JSON on stdin, which `post-tool-use` never reads. It errors on every edit and silently never re-anchors. Compounding this, even when invoked correctly, re-anchoring is **non-idempotent** and **falsely marks correct decisions STALE** whenever there is no committed HEAD baseline (new files, or multiple edits between commits) — and the corruption compounds and does not self-heal.

3. **Performance cliffs contradict the "scales to large repos" claim, and the scale harness is blind to both of them.** Anchor extraction is O(n²) per file (a 1.4 MB file ≈ 55–66 s for one file); the hot-file write path is O(n²) cumulative (1,200 decisions in one file ≈ 86 s). The scale-smoke harness uses tiny one-function files and skips any file > 256 KiB, so neither bottleneck is ever exercised.

4. **Operational diagnosability is essentially zero** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery (dmap repair, stale-lock takeover) — for a tool that runs unattended as an agent hook.

None of the high-severity findings are memory-unsafety or RCE — Rust aborts cleanly — so the worst case is availability/DoS and silent metadata corruption, not exploitation. The defects are concentrated, well-understood, and individually fixable without architectural change. With a focused 4–8 week remediation pass (panic-safety hardening, hook stdin contract, idempotent re-anchoring, the O(n²) fixes, and a logging channel), this can become an excellent 1.0.

**Verdict: Do not ship as-is. Strong foundation; specific, fixable blockers.**

---

## 2. System Overview

Archiva stores *why code exists* beside the code. Per source file it maintains `.decisions/<path>.dlog` (authoritative YAML, schema:1) and `.decisions/<path>.dmap` (compact derivative index). Decisions are anchored to AST identities (`fn:foo`, `struct:Bar`, `block:if_x`) rather than line numbers, carry a fingerprint for drift detection, and form supersession chains. The same core operations are reachable three ways — CLI (`init`/`why`/`history`/`lint`/`status`/`hooks`/`write-decision`/`mcp`), a stdio JSON-RPC MCP server (`why`/`write_decision`/`ghost_check`), and Claude Code hooks (`session-start`/`post-tool-use`). Distribution is via an npm wrapper that selects a platform-specific native binary; the runtime is a single native binary with **no dependencies** (`Cargo.toml` `[dependencies]` is empty).

The architecture is sound and the product thesis is coherent. The problems are in robustness, the hook integration contract, performance at scale, and observability — not in the concept or the module decomposition.

**Module sizes (Rust, src/):** `anchor.rs` 12,341 (incl. ~5,090 test lines) · `git.rs` 4,329 · `project.rs` 2,261 · `fs.rs` 1,487 · `yaml.rs` 1,465 · `mcp.rs` 1,174 · `cli.rs` 1,030 · `decision.rs` 963 · `storage.rs` 959 · `json.rs` 722 · `diff.rs` 657 · `property_tests.rs` 540 · `dlog.rs` 507 · `paths.rs` 487. Total ~31,464 lines.

---

## 3. Architectural Assessment — **8/10**

Module boundaries are clean and cohesive: `cli`/`mcp` entrypoints → `core::project` orchestration → typed core modules (`decision`, `storage`, `dlog`/`dmap`, `anchor`, `git`, `paths`, `fs`). Coupling is low and the data-flow ownership (`.dlog` authoritative, `.dmap` rebuildable, request-scoped git reader) is well-reasoned.

The central architectural tension is the **zero-dependency, reimplement-everything-by-hand** stance: ~2.6k lines of git plumbing including a from-scratch DEFLATE inflater (`git.rs`), hand-written JSON/YAML parsers, and a multi-thousand-line multi-language anchor tokenizer. This is defensible *as a product tradeoff* (trivial supply chain, tiny binary, no transitive CVEs) but it concentrates the entire bug surface in hand-rolled parsers that have **not been fuzzed** — and that is precisely where every confirmed panic lives. The std-only purity is the root cause of the dominant risk class.

Two concrete architectural weaknesses (both verified):

- **No schema migration story.** `DLOG_SCHEMA_VERSION` is a hardcoded `1` and the parser hard-rejects anything else; a single forward-version file aborts every whole-repo command. There is no migrate-on-read and no skip-with-warning.
- **The ground-truth anchor range is computed and then discarded** in the normal re-anchor path (`project.rs:290-304`) — the parser already knows the anchor's exact current position, but the code trusts a fragile HEAD-diff shift instead. This is the root cause of the idempotency/STALE corruption.

Top simplification opportunity: prefer the extractor's live anchor position over diff-shifting; this single change fixes two high-severity findings at once.

---

## 4. Workflow Assessment — **5/10**

The intended loop is *read map → ask why → edit → write decision → lint drift*. Traced end-to-end:

- **`init` → first decision → `why`**: works cleanly; idempotent on re-run.
- **`write-decision`**: works, but is **non-atomic across `.dlog`/`.dmap`** (a torn write durably commits the decision while reporting failure, exit 1) and the natural retry is **non-idempotent** (overwrites the just-committed record with a new id, losing the original reasoning with no history entry).
- **Auto re-anchor on edit (the core promise)**: **broken under real Claude Code** (hook ignores the stdin payload) and **corrupts line attribution** even when invoked correctly (false STALE + compounding line drift with no committed baseline).
- **Re-deciding an anchor**: silently destroys the prior decision and its entire history unless `supersedes:<id>` is passed; superseding *into* an anchor occupied by a different live decision silently deletes that unrelated decision.

The "happy path" demos work; the realistic agent workflow (uncommitted files, multiple edits per commit, re-decisions) has multiple silent-data-integrity failures.

---

## 5. Feature Completeness Assessment — **7/10**

The advertised command set is fully present and the CLI surface is consistent and complete. Gaps are in fidelity rather than coverage: MCP `why` cannot do line-based lookup (the `line` field is silently dropped and it returns a *confidently wrong* whole-file result rather than "not found"); no `--fix` audit trail; no migration tooling. Feature breadth is appropriate and intentionally narrow (the README's positioning vs. broad memory tools is honest and well-argued).

---

## 6. Behavioral Consistency Assessment — **6/10**

CLI ↔ MCP ↔ hooks largely agree on validation and path normalization (verified: `.//src/a.ts` and `src\a.ts` normalize to one identity across entrypoints). Confirmed divergences:

- **MCP `why` ignores `line`** and returns the wrong decision; CLI `why <file> <line>` is correct.
- **MCP tool errors are returned as JSON-RPC protocol errors (`-32000`)** instead of the MCP convention `result.isError:true` — a spec inconsistency for tool *execution* failures.
- **`lint` exit code conflates** "found issues" with "command failed"; `status` returns 0 even with outstanding issues. No exit-code taxonomy.

---

## 7. Data Model Assessment — **7/10**

The decision record (chose/because/rejected/anchor/fingerprint/lines_hint/history/supersession) is well-designed and matches the spec and the YAML schema. Two material risks: **unknown/forward-compat fields are silently dropped on every rewrite** (data loss for any external or future-version annotation), and the **no-supersede overwrite** path discards history. The model is good; its *evolution and preservation guarantees* are weak.

---

## 8. Storage & Persistence Assessment — **7/10**

Individual writes are atomic (temp + rename + fsync; Unix parent-dir fsync). Verified strengths: corrupt `.dmap` self-heals from `.dlog`; stale-lock recovery works. Verified weaknesses:

- **No cross-file atomicity** between `.dlog` and `.dmap` (torn-write false-failure).
- **PID-liveness lock veto can wedge all writes indefinitely** (PID reuse defeats the staleness check; no force-unlock).
- **Read-only `status`/`lint` acquire the per-file write lock** and fail on a read-only `.decisions/`.
- Parent-dir durability fsync is a **no-op on Windows** (crash-consistency claim is weaker there and untested in crash-injection form).

---

## 9. CLI Assessment — **8/10**

The strongest subsystem. Dispatch, help text, exit-code routing (0/1), `--` escaping, and unknown-flag/command handling are correct and extensively tested. Rough edges: `write-decision` **reads stdin before validating its own args** (a malformed call with an open stdin hangs), the `lint` exit-code conflation, inconsistent `error:`-prefixing between argument vs. semantic errors, and `why` accepting line `0`.

---

## 10. Protocol & Integration Assessment — **6/10**

JSON-RPC 2.0 framing, id handling, `notifications/*` swallowing, and method dispatch are correct and tested. **The integration contract with Claude Code is broken** (PostToolUse stdin payload ignored — the single most important integration defect). MCP tool-error encoding deviates from the MCP convention. And the long-lived server has **no panic isolation** — one crafted `tools/call` aborts the whole process.

---

## 11. Performance Assessment — **6/10**

Measured, not assumed:

- **Anchor extraction O(n²) per file** (`is_top_level`/`rust_depths_before` re-scan the token prefix per declaration): TS 10k/20k/40k lines = 0.5/2.3/9.9 s; a 1.4 MB file ≈ 55–66 s; a single huge Rust fn ≈ 47 s vs ~3 s for the TS equivalent. Memory stays modest (~70 MB) — purely CPU.
- **Hot-file writes O(n²) cumulative** (full render + redundant re-parse + full rewrite + fsync per write): #1,200 in one file ≈ 86 s cumulative; 1,500 timed out at 120 s. The `storage.rs:133` self re-parse of freshly-rendered YAML is unconditional wasted work.
- **`status` parses every `.dlog` twice** per invocation and sweeps the whole source tree.
- The `.dmap` index **is never read by any command** — every read re-parses the verbose `.dlog`, so the derivative index pays maintenance cost for zero read benefit.

Startup is excellent (~0.7 ms). For small/medium repos performance is fine; the cliffs are real and reachable.

---

## 12. Scalability Assessment — **5/10**

The "scales to 100k files / 1M decisions / Linux-kernel-and-LLVM corpora" claim is **not substantiated by the harness**, which uses tiny one-function files, skips files > 256 KiB, and defaults to 1 decision/file — exactly avoiding both quadratic paths. The blast radius is also wider than the per-command framing: because whole-repo commands re-extract anchors from *every* source file (decided or not), one large generated/minified/vendored file degrades `lint`/`status` repo-wide. Linear-in-file-count scanning is otherwise reasonable.

---

## 13. Reliability Assessment — **5/10**

Dominated by the panic class: `status`, `lint`, `session-start`, and the MCP server all abort (SIGABRT / exit 101 or 134) on malformed-but-committed input, and **a single corrupt `.dlog` aborts the entire repo-wide command** rather than being skipped and reported — taking down visibility for all healthy files. Recovery paths that exist (dmap repair, lock recovery) are correct but silent. Single-file commands are correctly scoped and limit some blast radius.

---

## 14. Failure Recovery Assessment — **6/10**

Good: atomic single-file writes, self-healing dmap, stale-lock recovery. Weak: no cross-file transaction; `lint --fix` is non-atomic across files and leaves a partially-fixed repo with no record of what changed on mid-run failure; no panic boundary so corruption aborts rather than degrades; no migration/forward-compat recovery. The recovery primitives are sound but not composed into command-level resilience.

---

## 15. Security Assessment — **6/10**

Threat model is correct (local CLI; parses untrusted repo/agent-supplied `.dlog`/source; no authn expected). No RCE, no memory unsafety, no command execution, no network. Confirmed issues:

- **DoS via panics** on committed/shared input (the dominant issue) — including killing the shared MCP server.
- **Write-side symlink escape**: the read path canonicalizes and rejects escapes, but the *write* path does not — a checked-in symlink under `.decisions/` writes `.dlog`/`.dmap` outside the repo and can clobber a same-named `.dmap` in the target. A clean bypass of the project's own advertised symlink control (asymmetric: `.dlog` clobber is gated by the parse step, `.dmap` is not).

Read-path path-validation hardening is genuinely strong (traversal, drive/UNC/device, reserved Windows names, trailing dot/space all rejected; verified). The git zlib path bounds delta depth (though the cap of 32 is *below* git's default 50 — a correctness bug, not a security one).

---

## 16. Testing Assessment — **6/10**

Volume is high and the differential-against-TS strategy is the right idea. But the confidence is narrower than the count implies:

- **The differential oracle covers only TS/JS.** The Rust and C/C++ extractors — the largest *novel* code the v2 effort added — have **no independent oracle**; correctness rests on same-team unit tests. A wrong range in a `.rs` file is silently accepted (verified: a decision with a deliberately wrong line range passes `lint`).
- **No fuzzing of the hand-written parsers** — the exact gap that let four panics survive into a release candidate.
- **No concurrency/crash-injection tests** — atomicity and locking verdicts rest on code reading.
- **The scale harness exercises a best case** (tiny files, 256 KiB skip, 1 decision/file).
- **The strongest suites (property soak, full scale, corpus matrix) run only weekly/on-dispatch, not on PRs.**
- **Cross-platform (Windows/macOS/arm/musl) is CI-only and unverified here**; the Windows crash-consistency path is a different (weaker) code path that is not crash-tested.

---

## 17. Documentation Assessment — **8/10**

README, spec, and architecture docs are clear, honest, and unusually well-written (the competitive-positioning table is fair; `docs/archiva-v2-review-status.md` candidly lists remaining gaps). Two doc-vs-reality drifts matter: the quick-start's auto-wired hook doesn't work as documented, and the "scales to large repos" claim is not borne out. Otherwise documentation is a strength.

---

## 18. Developer Experience Assessment — **7/10**

Onboarding is smooth and the CLI is pleasant and consistent. DX is undercut by: the broken auto-hook surfacing a recurring error on every edit, the absence of any diagnostic/verbose mode, confusing error messages with no file path on corrupt-store scans (`file: missing required field` reads as a sentence, not a schema field), and the silent data-loss footguns (re-decide without supersede). The bones are good; the failure-mode UX needs work.

---

## 19. Operational Readiness Assessment — **4/10**

The single biggest operational gap is **observability: there is none** — no logging, no `--verbose`, no `RUST_LOG`, and silent automatic recovery. For an unattended agent hook, any non-crash misbehavior currently requires recompiling with instrumentation to investigate. Combined with the panic class and the no-path corrupt-file errors, field diagnosability is poor.

---

## 20. Production Readiness Assessment — **5/10**

Not production-ready for general/public use today. It is usable in a controlled, single-user, small-repo, all-committed-files setting where the panic triggers and perf cliffs are unlikely. It is not ready for team use over a shared `.decisions/` tree (the panic propagation vector) or for large repos (the perf cliffs) or for the documented Claude Code auto-workflow (the hook contract).

---

## 21. Open Source Readiness Assessment — **7/10**

Strong: MIT license, clean repo, clippy-strict, formatted, CONTRIBUTING-grade docs, a real CI/validation/publish pipeline, and a thoughtful architecture doc with explicit extension points. Adoption-readiness is gated almost entirely by the production-readiness blockers above plus the missing panic-safety and fuzz gates that an open-source contributor base would expect before trusting it with their decision history. Packaging has one sharp edge: `import.meta.dirname` in the install tooling hard-requires Node ≥ 20.11 while npm doesn't enforce `engines`, so older-Node users get a cryptic postinstall crash.

---

## 22. Prioritized Findings

**Severity distribution (adversarially verified):** 0 critical *as labeled* / 17 high / 27 medium / 44 low / 9 info. 96 CONFIRMED, 1 PLAUSIBLE, **0 REFUTED**. The completeness critic argues — and I concur — that the *consolidated* "panic on committed/shared input" theme meets a **critical / release-blocking** bar even though no single line-item was labeled critical.

### Release blockers (must fix before 1.0)

| # | Finding | Location | Why it blocks |
|---|---|---|---|
| B1 | **Panic/abort class on committed input** (lone `'` → `yaml.rs:700`; mid-codepoint slice → `yaml.rs:310`; empty block scalar; deep-nesting recursion → `anchor.rs:733`) | `yaml.rs`, `anchor.rs` | One malformed byte in a shared `.dlog`/source aborts `status`/`lint`/`session-start` and **kills the MCP server**. It's a class — must be closed by fuzzing + depth bounds + a panic boundary, not point patches. |
| B2 | **PostToolUse hook is a no-op under Claude Code** (ignores stdin `file_path`) | `settings.rs:5`, `main.rs:46-51`, `cli.rs:234-258` | The core advertised automation never runs in the documented environment. |
| B3 | **Re-anchor falsely marks STALE + non-idempotent line drift** (no HEAD baseline / multiple edits per commit; ground-truth range discarded) | `project.rs:271-304`, `diff.rs:17-44` | Silent corruption of the authoritative store under normal agent activity; breaks line-based `why`. |
| B4 | **O(n²) anchor extraction** (per-file prefix re-scan) | `anchor.rs:6678`, `1729/1760` | Tens of seconds–minutes on a single large file; hooks read as hangs. |
| B5 | **One corrupt `.dlog` aborts the whole repo command** + **errors carry no file path** | `project.rs:76-82/402-408/133-149`, `error.rs:88-110` | Localized corruption blinds the whole repo; operators can't locate the bad file. |

### High (fix before or immediately after 1.0)

B6 non-atomic `.dlog`/`.dmap` write (false-failure + retry id-churn, `storage.rs:256-258`) · B7 write-side symlink escape (`paths.rs`/`storage.rs`) · B8 O(n²) hot-file writes + redundant re-parse (`storage.rs:131-133`) · B9 no logging/diagnostics anywhere · B10 Rust/C/C++ extractors have no differential oracle · B11 silent data loss on re-decide without `supersedes` · B12 MCP `why` returns confidently-wrong result for a `line` query.

### Medium (notable)

PID-liveness lock wedge · read-only `status`/`lint` take write locks · MCP tool errors as `-32000` not `isError` · `lint`/`status` exit-code taxonomy · unknown-field drop on rewrite · no schema migration · anchored-gitignore patterns never match descendants · C++ `enum class` phantom anchor · git delta-depth cap 32 < 50 · `.dmap` never read · scale harness blind to both quadratics · `lint --fix` non-atomic · Node-version postinstall crash · case-insensitive-FS identity collisions · heavy validation runs only weekly.

---

## 23. Recommended Roadmap

**Milestone 1 — Panic-safety & robustness (release-blocking, ~2 wks)**

1. Fuzz the YAML/JSON parsers (`cargo-fuzz` or in-repo property soak over arbitrary bytes); fix every panic; add a depth/recursion bound to the anchor extractor and block-scalar paths.
2. Wrap MCP per-request handling in `catch_unwind` → return `isError`; never let one request abort the server.
3. Make whole-repo commands skip-and-report corrupt files (continue, name the file) instead of aborting; attach the file path to all parse/schema/IO errors.
4. **Make "no panic on any `.dlog`/source input" and "MCP server survives a malformed request" hard PR-CI gates.**

**Milestone 2 — Core workflow correctness (release-blocking, ~1–2 wks)**

5. Parse the Claude Code hook stdin JSON (`tool_input.file_path`) in `post-tool-use`; add an integration test feeding the real payload shape.
6. Re-anchor from the extractor's ground-truth position whenever the anchor still resolves; reserve diff-shift for the orphan/incomplete case. Add a regression test asserting two consecutive `post-tool-use` runs leave `lines_hint`/STALE unchanged.
7. Detect re-decide-without-supersede and supersede-into-occupied-anchor; refuse or auto-chain into history.
8. Make `.dlog`+`.dmap` a recoverable transaction (treat a `.dmap` write failure as success-with-warning, since reads self-heal).

**Milestone 3 — Scale & observability (~1–2 wks)**

9. Eliminate the O(n²) extraction (single-pass depth tracking) and the O(n²) write path (drop the `storage.rs:133` re-parse in release; consider per-file decision bounds); add a large-single-file and a dense-single-file perf regression to the PR gate.
10. Add an env-gated stderr diagnostic channel (files scanned/skipped, lock acquire/recover, dmap repair, git-HEAD fallback).
11. Either make `status`/`session-start` actually read `.dmap`, or drop it.

**Milestone 4 — 1.0 hardening (~1–2 wks)**

12. Independent oracle for Rust/C/C++ extraction (cross-check ranges vs tree-sitter or rustc/clang spans) over a real corpus.
13. Define a schema versioning/migration policy; preserve unknown fields on rewrite.
14. Canonicalize the write path (close the symlink escape); add concurrency + crash-injection tests; promote heavy validation into the release gate; fix the lock-wedge backstop, exit-code taxonomy, gitignore anchoring, git delta-depth cap, and Node-version preflight.
15. Run the multi-process lock and differential suites on Windows/macOS CI (not just `cargo test`).

---

## Subsystem Scorecard

| Subsystem / Dimension | Score |
|---|---|
| CLI surface & dispatch | 8/10 |
| MCP stdio JSON-RPC server | 7/10 |
| Anchor extraction engine | 7/10 |
| Native Git object reader | 8/10 |
| Decision logic (validation/supersession/history) | 7/10 |
| Storage / locking / atomicity / recovery | 7/10 |
| Project workflow orchestration | 8/10 |
| Serialization (JSON/YAML/dlog/dmap) | 6/10 |
| Path validation & portability | 7/10 |
| Diff / reanchor / line-shifting | 7/10 |
| Security (cross-cutting) | 6/10 |
| Performance & scalability | 6/10 |
| Reliability / recovery / observability | 7/10 |
| Testing strategy & confidence | 7/10 |
| Release engineering & packaging | 8/10 |
| API/CLI/behavioral consistency & DX | 8/10 |
| Overall architecture & maintainability | 8/10 |

*(Per-subsystem scores reflect each module's intrinsic quality. The system-level scores below are lower because the dominant failures are cross-cutting — they emerge from interactions, e.g. parser-panic × shared-committed-store × long-lived-server.)*

### Overall scores

- **Engineering quality (unit level): 8/10** — clean, disciplined, well-tested-in-volume, idiomatic, zero-warning.
- **Production readiness: 5/10** — concentrated, reproducible blockers; safe only in a controlled single-user setting today.
- **Validation confidence: 6/10** — broad and partly differential, but blind to the exact failure classes that bite (no fuzz, no scale realism, no Rust/C oracle, no concurrency/crash injection, no real-hook integration); heavy suites are off the PR path.

---

## Final Questions — Answered Explicitly

**Does the implementation fully achieve its stated goals?**
No. It achieves the *static* goal (a fast, repo-native, zero-dependency decision store with a working CLI and MCP surface) but not the *dynamic* goal: the automatic agent workflow it is built around (auto re-anchor on edit) is broken end-to-end under real Claude Code and corrupts line attribution even when invoked correctly, and the "scales to large repos" claim is not substantiated.

**Is the architecture appropriate for long-term evolution?**
Mostly yes. Module boundaries, ownership, and extension points are sound. The two evolution gaps — no schema migration path and the zero-dependency stance concentrating un-fuzzed parser risk — are addressable without restructuring.

**Is the implementation internally consistent?**
Largely, with confirmed exceptions: CLI vs MCP `why` line semantics, MCP error encoding vs spec, and exit-code conventions. These are localized.

**Would you approve this for a stable public release?**
No. The panic class on committed/shared input, the broken auto-hook, the re-anchor corruption, and the absence of observability are release-blocking for a stable 1.0.

**Would you approve it as the reference implementation?**
Not yet. A reference implementation must be panic-safe against its own committed data format and must have the core workflow proven end-to-end. Once Milestones 1–2 land and panic-safety + real-hook integration are CI gates, it is a credible reference candidate.

**Highest-priority improvements before release?**
(1) Fuzz and bound the parsers + add a panic boundary + skip-and-report corrupt files; (2) fix the PostToolUse stdin contract; (3) make re-anchoring idempotent from ground-truth positions; (4) fix the O(n²) extraction/write paths; (5) add a diagnostic logging channel. In that order.

**What work remains before 1.0?**
Milestones 1–4 above. Realistically 4–8 focused weeks. The critical path is panic-safety + workflow correctness (M1–M2).

**What risks remain after release (assuming blockers fixed)?**
Cross-platform behavior (Windows/macOS/arm/musl) remains CI-only and the Windows crash-consistency path is weaker; the Rust/C/C++ extractors stay oracle-light until an independent ground truth exists; the hand-rolled git/DEFLATE code is a maintenance hotspot that needs an ongoing differential-against-`git` fuzz harness; and concurrency/crash-consistency guarantees rest on reasoning until fault-injection tests exist.

**How does engineering quality compare with mature, well-regarded OSS in the same domain?**
At the *craft* level (code cleanliness, lint discipline, documentation, the differential-testing instinct) it compares favorably with well-run early-stage OSS and exceeds many. At the *production-hardening* level it is behind mature tools like ripgrep, gitoxide/`gix`, or tree-sitter, which earned trust through extensive fuzzing, fault injection, and battle-tested robustness against adversarial input — exactly the layer Archiva has not yet built. It has the bones of a top-tier project and an unusually honest self-assessment; it has not yet done the hardening that separates a strong prototype from a definitive reference.

---
---

# APPENDIX — Full Verified Findings

Every finding below survived independent adversarial verification (reproduction against the release binary or direct code read). `verdict` is the second agent's call; `severity` is the corrected severity. Findings are grouped by subsystem/dimension, then sorted high → info.

-anchored full-string regex. The Rust is a faithful 1:1 port, so this is an inherited parity quirk, not a Rust-introduced regression. The repo's own test keeps_typescript_directory_only_behavior (gitignore.rs:251-253) asserts `src/generated/` does NOT ignore src/generated/a.ts, codifying the broken behavior as intended parity.

**Verifier notes / severity correction:** Claim is accurate in code location, mechanism, reproduction, and impact. The cited line numbers are off by one in the prose (the anchored early-return is at gitignore.rs:62, dir_only branch at 64-66, glob.matches full-string requirement at the terminal memo case 152-153) but the description's own line references (50-67, return at 62, branch at 64-66) are correct. Severity medium is appropriate: it is a correctness defect in user-facing exclude handling that only affects the lint-scan file enumeration (list_lint_source_files via project.rs:164-186), not decision storage, security, or the path-traversal hardening — and it is faithful to the TS source the port deliberately preserves (verified by the dedicated parity test). Worth noting it is a confined functional defect / inherited parity quirk rather than an architectural flaw: the blast radius is limited to which source files lint inspects; users relying on anchored or nested-dir .gitignore entries get extra (not missing) lint warnings, a fail-open-toward-noise behavior. Recommended resolution: reorder so dir_only is handled before the anchored early-return and make anchored matching prefix-aware (treat a dir pattern as matching the directory plus everything beneath it), or, if strict TS parity must be preserved, document the quirk explicitly. Any fix should update the keeps_typescript_directory_only_behavior test, which currently asserts the buggy outcome.

**Recommended resolution:** Rework match_pattern to real gitignore semantics: a pattern containing a non-trailing slash is anchored to the root; trailing-slash patterns match a directory and all descendants; combine anchoring and dir-only handling instead of returning early on `anchored`. At minimum, when both anchored and dir_only, match the directory prefix against path segments rather than requiring a full-string glob match.

---

### F51. [MEDIUM] No case-folding: case-only path variants produce distinct decision identities (collision/split on case-insensitive filesystems)

`operational` · location `src/core/paths.rs:169-223 (normalize_relative_path performs no case normalization)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** RelativePath identity is the raw byte string after slash/dot normalization, with no case folding. src/Case.ts and src/case.ts are treated as two distinct identities and two distinct .dlog files. On macOS (APFS default) and Windows (NTFS default), these are the SAME underlying file, so the two decision logs alias one source file; conversely a why/history lookup may resolve to a different-cased real file than the dlog it reads.

**Why it matters:** On case-insensitive filesystems this silently splits a single file's decision history across two logs, or collides two intended-distinct logs onto one path, corrupting the decision-memory mapping the tool exists to provide. The path layer claims to normalize to 'one identity' but does not account for FS case-insensitivity.

**Impact:** Split or merged decision history; why/history returning stale or wrong-file results on macOS/Windows.

**Likelihood:** Medium on macOS/Windows where mixed-case references occur; nil on case-sensitive Linux.

**Evidence (reporter):** In /tmp/gi (Linux, case-sensitive) the two are genuinely different files and produce src/Case.ts.dlog distinct from a would-be src/case.ts.dlog; the validator accepts both without normalization (paths.rs:169-223 has no to_ascii_lowercase or fold). Impact manifests on case-insensitive FS, which I could not exercise here.

**Independent verification:** Code read — /home/ubuntu/archaeo/src/core/paths.rs:169-223 (normalize_relative_path): the function lowercases nothing. It does backslash→slash, strips leading "./", rejects absolute/UNC/drive/dot/dotdot/empty/Windows-invalid segments, then returns the string verbatim. RelativePath(String) (paths.rs:8) stores that raw string as identity; dlog_path/dmap_path (paths.rs:107-113) build storage filenames directly from it. A repo-wide grep for case folding in the identity path (paths.rs, storage.rs, dmap.rs, dlog.rs) found only paths.rs:240 to_ascii_uppercase (Windows reserved-name CON/PRN check, not identity) and dmap.rs:166 eq_ignore_ascii_case (numeric infinity/nan parse). No to_ascii_lowercase / fold on the path at all.

Runtime repro in /tmp/casefold (Linux, case-sensitive ext4) with the release binary:
  - wrote {"file":"src/Case.ts","anchor":"export:a",...} → "Recorded dec_001."
  - wrote {"file":"src/case.ts","anchor":"export:b",...} → "Recorded dec_001." (independent dec_001, not a supersede)
  - find .decisions -type f yielded FOUR distinct files: src/Case.ts.dlog, src/Case.ts.dmap, src/case.ts.dlog, src/case.ts.dmap
  - `archiva why src/Case.ts` → export:a / "upper" / reasonA; `archiva why src/case.ts` → export:b / "lower" / reasonB (two distinct identities)
  - `archiva status` lists both src/Case.ts and src/case.ts as separate entries, "Total: 2 decisions".
This exactly matches the prior auditor's Linux observation. The case-insensitive-FS impact (macOS APFS / Windows NTFS) follows by construction and was not exercised here, consistent with the auditor's own stated limitation: on such a FS the two .dlog filenames differ only in case, so they collapse onto one physical file (second write opens/clobbers the first), and a why/history path computed from one casing opens whatever physical-cased file exists — yielding merged or wrong-file results.

**Verifier notes / severity correction:** Claim confirmed as stated, including location (paths.rs:169-223), mechanism (no case folding; raw normalized string is the identity), and the Linux evidence. Severity medium is correct and not inflated: it only manifests on case-insensitive filesystems (macOS/Windows), requires actual case-variant paths for the same source file (uncommon but realistic during refactors or across contributors with mixed conventions), and degrades decision-history correctness (split or clobbered logs, stale/wrong-file why/history) rather than crashing or destroying source code. One refinement to the auditor's framing: on a case-insensitive FS the dominant failure mode is collapse/clobber onto a single physical .dlog (the differing-case filenames alias), not the Linux "two distinct files" mode — i.e. the split-identity is in-memory keying while the on-disk storage merges, which can silently overwrite one logical file's log with another's. Recommended resolution: this is a deliberate design choice to use the verbatim relative path as a portable, byte-stable identity (case folding would break legitimately case-sensitive Linux repos and Unicode edge cases). Rather than always folding, detect filesystem case-sensitivity at init (or probe per-repo) and, on case-insensitive roots, either (a) canonicalize identity via the on-disk actual filename casing, or (b) warn/refuse when two recorded identities differ only by case. At minimum, document the limitation and add a lint check that flags case-only collisions among recorded decision paths.

**Recommended resolution:** Decide on a canonical identity policy: either document that identities are case-sensitive and out of scope for case-insensitive FS, or fold/compare case for identity on platforms where the FS is case-insensitive. At minimum, detect existing case-variant .dlog siblings and warn.

---

### F52. [LOW] Gitignore negation patterns (!) are parsed but never applied

`defect` · location `src/core/gitignore.rs:39-41` · reporter-confidence high · verification **CONFIRMED**

**Description:** matches_gitignore iterates patterns and does `if pattern.starts_with('!') { continue; }`, so negations are silently dropped. parse_gitignore (line 26-32) deliberately keeps `!` lines, so the data is present but the matcher ignores it. Real git treats `!pat` as a re-include that can override a prior ignore. This is a faithful port of the TS reference (gitignore.ts:28), so it is an inherited parity quirk rather than a Rust-introduced regression — but it is still wrong relative to real .gitignore semantics.

**Why it matters:** The matcher gates list_lint_source_files (project.rs:184), the source-file enumeration for the lint complexity scan. A user who writes `*.ts` followed by `!keep.ts` expects keep.ts to be scanned; instead the negation is ignored, `*.ts` ignores keep.ts, and the file is silently excluded from lint — a false negative that hides complex, undecided code.

**Impact:** Files the user intended to re-include are omitted from the complexity scan / status; no error is shown.

**Likelihood:** Common — negation is a frequently used .gitignore feature.

**Evidence (reporter):** In /tmp/gi with `.gitignore` = `*.ts` + `!root-only.ts`, `archiva lint` prints 'No decision issues found.' (exit 0); root-only.ts (complexity 7, undecided) is not scanned. Code path: gitignore.rs:40 `continue`.

**Independent verification:** Code: /home/ubuntu/archaeo/src/core/gitignore.rs:38-41 — matches_gitignore loops patterns and does `if pattern.starts_with('!') { continue; }`, so every negation line is skipped by the matcher. parse_gitignore (lines 26-32) only filters empty/`#` lines, so `!`-lines ARE retained in the pattern vector (confirmed by unit test line 193-199 which asserts "!rescue.ts" survives parsing). Net effect: the negation data is present but the matcher never consults it.

Runtime reproduction in /tmp/gi (init'd project):
- Wrote root-only.ts with a function `complex` measured by the binary at complexity 6 (>= the threshold 5 at project.rs:601).
- .gitignore = `*.ts\n!root-only.ts\n` -> `archiva lint` prints "No decision issues found." exit 0 (file silently dropped despite the re-include intent).
- No .gitignore -> `archiva lint` prints "WARNING arc/undecided root-only.ts fn:complex: fn:complex has complexity 6 and no decision" exit 0.
- .gitignore = `*.ts` only (no negation) -> "No decision issues found." exit 0.
This proves the negation line has zero effect: with or without `!root-only.ts`, the `*.ts` ignore wins and the file is excluded.

Parity claim verified: /home/ubuntu/archaeo/src/core/gitignore.ts:28 does the identical `if (pattern.startsWith("!")) continue;`. So this is a faithful TS port / inherited parity quirk, not a Rust-introduced regression — exactly as the prior auditor stated. The behavior is even codified by a passing Rust unit test (gitignore.rs:208 asserts `!matcher.is_ignored("rescue.ts")` for input `ignored.ts\n!rescue.ts`).

Minor correction: the cited "complexity 7" was the auditor's own fixture; my fixture measured 6. The exact value is not load-bearing — any source file >= complexity 5 reproduces the silent omission.

**Verifier notes / severity correction:** The defect is real and precisely located (gitignore.rs:39-41), reproduces deterministically, and is correctly framed as an inherited TS-parity quirk rather than a Rust regression. I am downgrading severity medium -> low: (1) it affects only the advisory lint/status complexity scan (project.rs:164-186, 601), not decision storage, retrieval (why/history), or any data-correctness path; (2) the behavior is an intentionally preserved parity contract with a codifying passing test (gitignore.rs:203-212), so it is documented/expected within the project's own contract, not an accidental break; (3) gitignore negation re-includes of files you simultaneously want complexity-scanned are an uncommon configuration. It is a genuine silent correctness gap relative to real git `.gitignore` semantics (a `!pat` re-include can override a prior ignore; here it is dropped with no error and exit 0), so it is a legitimate low-severity defect / minor portability-fidelity weakness — medium is defensible but overstates the blast radius. Recommended resolution: implement last-match-wins semantics — track whether the most recent matching pattern was an ignore or a negation rather than returning true on first ignore match; or, if parity must be preserved, emit a one-time warning when a `!`-pattern is present so users are not silently misled. If touched, update the TS reference and the parity test together to keep the two implementations aligned.

**Recommended resolution:** Implement last-match-wins ordering: track whether the final matching pattern is a negation and return its inclusion state, rather than skipping `!` lines. If strict TS parity must be preserved, document the limitation explicitly and surface it to users.

---

### F53. [LOW] Windows reserved-name, invalid-char, and trailing-dot/space rejection applied on all platforms

`tradeoff` · location `src/core/paths.rs:208-219, 234-253` · reporter-confidence high · verification **CONFIRMED**

**Description:** normalize_relative_path unconditionally rejects segments that are Windows reserved device names (CON/PRN/AUX/NUL/COM1-9/LPT1-9/CONIN$/CONOUT$), contain `< > : " | ? *`, or end with a space or dot — regardless of the host OS. On Linux/macOS these are all legal filenames that may genuinely exist in a repo.

**Why it matters:** A real source file such as src/aux.ts, src/nul.ts, or a file with a colon in its name on a Unix checkout can never receive a decision: write-decision, why, and history all fail at validation before touching the file. The intent (keep .decisions/ portable to Windows checkouts) is reasonable, but the enforcement is lossy and silent about being Windows-motivated.

**Impact:** Legitimate Unix-named files are permanently unaddressable by the tool.

**Likelihood:** Low-to-moderate; depends on project naming.

**Evidence (reporter):** In /tmp/arch-audit, with src/aux.ts present: write-decision file:"src/aux.ts" => 'Invalid project-relative path "src/aux.ts": Windows reserved names are not allowed'. Same for nul.ts/con.ts/prn.ts.

**Independent verification:** Code at src/core/paths.rs:198-220 runs the segment validation loop inside normalize_relative_path with NO platform gate (no #[cfg(windows)], no runtime OS check). The three checks fire unconditionally: has_windows_invalid_character (paths.rs:249-253, rejects < > : " | ? *), segment.ends_with([' ', '.']) (paths.rs:214), and is_windows_reserved_name (paths.rs:234-247, matches CON/PRN/AUX/NUL/CONIN$/CONOUT$ and COM1-9/LPT1-9 via 4-char stem with digit 1-9). I read the whole function (paths.rs:169-223) — the only OS-conditional logic anywhere is absent.

Reproduced on Linux (uname -s = Linux) against /home/ubuntu/archaeo/target/release/archiva in /tmp/arch-audit (git init + archiva init):
- write-decision --json {"file":"src/aux.ts",...} => 'Invalid project-relative path "src/aux.ts": Windows reserved names are not allowed'
- con.ts, nul.ts, prn.ts, com1.rs => same "Windows reserved names" rejection
- "foo|bar.ts", "name?.ts", "star*.ts" => "Windows-invalid path characters are not allowed"
- "trailingdot.ts.", "trailspace .ts " => "path segments cannot end with a space or dot"
A real src/aux.ts file existed on disk; the tool still refused to address it.

**Verifier notes / severity correction:** Claim is accurate and correctly scoped. The reserved-name set, invalid-char set, and trailing dot/space rule all match the cited code exactly, and the cited line ranges (208-219 for the checks, 234-253 for the helpers) are correct. Severity tradeoff/low is appropriate: this is a deliberate cross-platform-portability decision (error kinds are explicitly named Windows*), ensuring a .decisions log written on Linux references only paths that can also be checked out on Windows. The downside is genuine — Unix files named aux.ts/con.ts/nul.ts or containing :"|?*<>* or trailing dot/space become permanently unaddressable — but such filenames are uncommon and the restriction is intentional hardening, not a logic defect. One minor scoping nuance worth noting: the claim's "a:b.ts" style case is actually caught earlier by the drive-prefix check (paths.rs:179, starts_with_drive_prefix) rather than the invalid-character check, but mid-segment colons like "foo:bar.ts" hit WindowsInvalidCharacter, so the net effect the claim describes holds. Recommended resolution if portability is not a hard requirement: gate these three checks behind cfg!(windows) or an opt-in --strict-paths flag, while keeping NUL-byte, absolute, drive/UNC, and .. traversal checks unconditional (those are security/correctness, not portability).

**Recommended resolution:** Either gate the Windows-specific checks behind cfg(windows)/an opt-in 'portable' mode, or keep them but emit a clearer message explaining the cross-platform-portability rationale and offering an override. Document the deliberate cross-platform rejection.

---

### F54. [LOW] No Unicode normalization: NFC/NFD variants split decision identity

`operational` · location `src/core/paths.rs:169-223` · reporter-confidence medium · verification **CONFIRMED**

**Description:** Path segments are compared as raw UTF-8 with no Unicode normalization. A file like café.ts can be encoded NFC (é = U+00E9) or NFD (e + U+0301); these are byte-distinct and yield different RelativePath identities and .dlog files, even though macOS normalizes filenames and treats them as one file.

**Why it matters:** Same class of split/alias problem as the case issue, on normalizing filesystems. An anchor recorded under one normalization form is not found under the other.

**Impact:** Decision history split across normalization forms; lookups miss on macOS.

**Likelihood:** Low — requires non-ASCII filenames plus a normalizing FS.

**Evidence (reporter):** In /tmp/gi, write-decision with file:"café.ts" recorded .decisions/café.ts.dlog; no normalization step exists in normalize_relative_path. Cross-form aliasing only manifests on a normalizing FS (not reproducible on this Linux ext4).

**Independent verification:** Code: src/core/paths.rs:11-13 `RelativePath::new` calls ONLY `normalize_relative_path`. Read of normalize_relative_path (paths.rs:169-223): it does `input.replace('\\','/')`, strips leading `./`, rejects absolute/UNC/drive/dot/parent/empty/Windows-invalid segments, but performs NO Unicode NFC/NFD normalization. The validated String is stored verbatim (paths.rs:8,12) and used directly to build dlog/dmap paths (decision_base_path/with_extension_suffix, paths.rs:103-117). RelativePath derives Eq/PartialEq on the raw String (paths.rs:7), so NFC and NFD strings are distinct identities.

Reproduction on /home/ubuntu/archaeo/target/release/archiva in /tmp/gi (Linux ext4):
- Created two byte-distinct source files: NFC `café.ts` = 63 61 66 c3 a9 2e 74 73 (é=U+00E9) and NFD `café.ts` = 63 61 66 65 cc 81 2e 74 73 (e + U+0301).
- Fed two write-decision payloads differing only in normalization form (built via printf with é vs ́ escapes to avoid shell normalization):
  {"file":"café.ts",...,"chose":"use nfc"} -> "Recorded dec_001."
  {"file":"café.ts",...,"chose":"use nfd"} -> "Recorded dec_001."
- Result: TWO independent dlog files (verified with od -tx1 on filenames):
  .decisions/...63 61 66 c3 a9...ts.dlog  -> chose: use nfc
  .decisions/...63 61 66 65 cc 81...ts.dlog -> chose: use nfd
  Both independently numbered dec_001, with separate .dmap indexes. Decision identity is split across normalization forms exactly as claimed.

**Verifier notes / severity correction:** Claim is accurate in mechanism, location, and severity. One correction to the claimed evidence: the prior auditor said the split is "not reproducible on this Linux ext4" and only the cross-form *aliasing* manifests on a normalizing FS. In fact the identity SPLIT (two distinct .dlog/.dmap files for the same logical file) IS directly reproducible on Linux ext4 — I demonstrated it above. The macOS-specific concern is the inverse: a normalization-insensitive FS (APFS) folds the two names to one file, so the two distinct RelativePath identities and dmap keys collide on one on-disk dlog, causing silent overwrite/lookup mismatch rather than a clean split. Either way the user-facing effect is fragmented or colliding decision history for non-ASCII filenames. Severity low is correct: requires non-ASCII paths authored in differing normalization forms (uncommon; most toolchains emit NFC), no security/data-integrity impact beyond the affected file's history, fully deterministic and benign on the dominant Linux/NFC path. Recommended resolution unchanged: apply Unicode NFC normalization to each path segment inside normalize_relative_path before validation/storage (std-only requires a small embedded NFC table or vendored normalization, since std has no Unicode normalization), or at minimum document the NFC-only contract and reject non-NFC input. Confidence high.

**Recommended resolution:** If macOS support matters, normalize identities to a fixed Unicode form (NFC) before constructing RelativePath, or document the non-normalization explicitly.

---

### F55. [LOW] No path-length validation; overlong names surface as an opaque 'path validation failed'

`techdebt` · location `src/core/paths.rs:169-223 (no length guard) and src/core/error.rs:185 (PathErrorKind::Io => "path validation failed")` · reporter-confidence high · verification **CONFIRMED**

**Description:** normalize_relative_path imposes no limit on segment or total length. Overlong names pass lexical validation and only fail later inside canonical_source_path_if_exists when the OS returns ENAMETOOLONG, which is mapped to the generic PathErrorKind::Io whose message is 'path validation failed'. Because the .dlog/.dmap target is .decisions/ + the relative path + a suffix, a source name that is itself near NAME_MAX can produce a .dlog path that exceeds NAME_MAX even when the source is readable.

**Why it matters:** The error message is misleading (it is an FS limit, not a validation rule) and the asymmetry (source fits, derived .dlog name does not) can make write-decision fail in a confusing way for long filenames.

**Impact:** Confusing diagnostics; potential write failure for long source names whose +.dlog suffix overflows NAME_MAX.

**Likelihood:** Low.

**Evidence (reporter):** In /tmp/gi, a 300-char single-segment name was accepted by RelativePath::new and failed downstream with 'Invalid project-relative path "aaaa...": path validation failed' (PathErrorKind::Io from canonicalize/try_exists), not a dedicated length error.

**Independent verification:** CODE (both cited locations exact):
- src/core/paths.rs:169-223 normalize_relative_path has NO length guard. It checks empty/NUL/UNC/drive/absolute/dot/parent/empty-segment/Windows-invalid-char/trailing/reserved — never segment or total length. RelativePath::new (paths.rs:11-13) is the only validation gate.
- src/core/error.rs:185: PathErrorKind::Io => "path validation failed". The From<PathError> impl (error.rs:156-162) maps only kind().as_reason() into ArchivaError::InvalidPath and DROPS PathError.detail (the real io string, set in PathError::io at paths.rs:62-68). So the OS reason is discarded.
- canonical_source_path_if_exists (paths.rs:142-167) wraps try_exists()/canonicalize() errors via PathError::io -> PathErrorKind::Io.

RUNTIME (binary /home/ubuntu/archaeo/target/release/archiva in /tmp/gi, NAME_MAX=255):
1. 300-char single-segment name accepted lexically, failed downstream:
   echo '{"file":"<300 a>",...}' | archiva write-decision
   -> 'Invalid project-relative path "aaaa...": path validation failed'  (EXIT 1)
   Confirms RelativePath::new accepted it and the opaque Io message surfaced — exactly the claimed evidence.
2. detail dropped: same with 300 z's -> 'path validation failed' (not 'File name too long').
3. +suffix overflow for a readable near-NAME_MAX source: created a real 252-char .ts source (fits NAME_MAX=255, create-source-exit=0), valid anchor extracted, then:
   -> 'Failed to create lock file /tmp/gi/.decisions/cccc....ts.lock: File name too long (os error 36)' (EXIT 1).
   .decisions/ + name + '.lock'/'.dlog' suffix = 257 chars > NAME_MAX, so a source that is itself writable/readable cannot get a decision recorded. Fails closed: 0 partial artifacts left behind.

**Verifier notes / severity correction:** CONFIRMED, techdebt/low is correct. Both code citations and both reproductions are accurate. Two small scoping corrections:

(a) The claim conflates two distinct downstream failure sites with different messages. The opaque "path validation failed" (PathErrorKind::Io) comes specifically from canonical_source_path_if_exists' try_exists/canonicalize (e.g. when the long source does NOT exist — the 300-char repro). But the actual write path for a name whose +.dlog/.lock suffix overflows surfaces a DIFFERENT, more descriptive message: "Failed to create lock file ...: File name too long (os error 36)" because the lock/write goes through fs, not PathError. So the "confusing diagnostic" and the "write failure for overflowing suffix" are largely separate paths, not one. The claim's framing implies the suffix-overflow case yields the opaque message; in practice it yields a raw-but-clearer OS error. Both real, both worth fixing.

(b) Impact is genuinely low/cosmetic + edge-case: real filenames near NAME_MAX (252+ chars) are rare; behavior fails closed with no corruption or partial writes. The defect is diagnostic quality (dropped detail string) plus absence of an early, dedicated length error.

Recommended resolution: add a length guard in normalize_relative_path (per-segment <= NAME_MAX-some_suffix_budget, e.g. -5 for ".dlog"/".lock", and an overall path budget) returning a new PathErrorKind::TooLong with reason like "path segment exceeds the maximum filename length"; and stop dropping PathError.detail — include it in the InvalidPath reason for the Io kind so OS errors aren't masked as "path validation failed".

**Recommended resolution:** Either validate a maximum segment/total length up front with a clear PathErrorKind, or distinguish FS ENAMETOOLONG from a generic Io failure in the error text so users understand it is a filesystem limit on the derived .decisions/ path.

---

## Diff / reanchor / line-range shifting  — score 7/10

> The line-diff engine in diff.rs is a correct, well-tested LCS implementation (full-table for small inputs, Hirschberg-style linear-memory for large) and its range-shifting helper apply_line_changes_to_range is a faithful port of the TypeScript oracle (reanchor.ts applyDiffToRange). time.rs computes UTC timestamps with pure epoch arithmetic, is timezone-independent by construction, and matches its fixtures. The version surface (Cargo 0.2.0, package.json @jalkarna/archiva 0.2.0, MCP serverInfo 0.2.0) is consistent. However, the integration of the diff into post_tool_use (src/core/project.rs:223-344) has a serious correctness defect: it diffs the stored lines_hint against the fixed git HEAD baseline and then persists the shifted hint, so the operation is NOT idempotent. Because the post-tool-use hook is intended to fire after every edit while git HEAD only advances on commit, repeated runs between commits compound the shift and silently mis-attribute decisions to the wrong code, also flipping decisions to spurious STALE. The anchor extractor already computes the true current line range but the normal code path discards it in favor of the drifting diff-shifted hint.

*Score rationale:* The diff algorithm itself is correct, well-factored, and thoroughly tested (full-table vs linear-memory cross-check, oracle parity fixtures, CRLF handling). time.rs and version.rs are clean and stable. The score is held down by a genuine, easily-triggered system-level correctness defect in how the otherwise-correct diff is integrated into post_tool_use: persisting a hint that is shifted from a fixed HEAD baseline makes the hook non-idempotent, so normal agentic usage (hook after every edit, commit occasionally) silently corrupts the core attribution data the tool exists to protect. The fix is small and the ground-truth data is already on hand, which is why this is high-not-critical and very addressable.

**Verified behaviors (checked, not assumed):**

- Built binary version surface consistent: archiva --version=0.2.0, MCP initialize serverInfo.version=0.2.0, Cargo.toml version=0.2.0, package.json @jalkarna/archiva 0.2.0.
- diff.rs apply_line_changes_to_range is a line-for-line faithful port of reanchor.ts applyDiffToRange (oldLine/offset/start semantics identical), and the LCS path (full-table <=1M cells, linear-memory otherwise) is correct per its self-cross-checking tests.
- Reproduced non-idempotent compounding: with a fixed git HEAD baseline, repeated `hooks post-tool-use` runs shift lines_hint each time (e.g. 3-5 -> 5-7 -> 7-9 with NO file change; [4,6]->[7,9] in /tmp/av7) and flip decisions to spurious STALE.
- Confirmed mis-attribution end to end: after drift, `why <file> <true-line>` returns 'No decision found' while `why <file> <wrong-line>` returns the decision; .dmap derivative and `status` both reflect the corrupted range and fabricated STALE count.
- Confirmed the trigger is multiple hook runs per HEAD: committing the edit (advancing HEAD) restores full idempotency across reruns (/tmp/av5).
- Confirmed anchor extractor computes the correct current range (info.start/info.end) but post_tool_use only uses it in the rename-fallback branch, discarding it in the normal path.
- Insertion inside an anchored region keeps the stored range width unchanged (faithful to oracle); deletion above the anchor shifts idempotently in the cases tested.
- time.rs timestamp formatting uses pure SystemTime/UNIX_EPOCH math with no TZ/chrono dependency, so output is UTC and timezone-independent by construction; unit fixtures (epoch, 2026 value, leap-day parse) pass.

### F56. [HIGH] post-tool-use is non-idempotent: repeated runs between commits compound the line-range shift and silently mis-attribute decisions

`defect` · location `src/core/project.rs:271-302 (apply_line_changes_to_range over read_git_head_file baseline); mirrors src/core/reanchor.ts:10-41` · reporter-confidence high · verification **CONFIRMED**

**Description:** post_tool_use computes old_content from git HEAD (read_git_head_file, line 272-273) and new_content from the working tree, diffs them (line 275), then for every decision sets decision.lines_hint = apply_line_changes_to_range(changes, decision.lines_hint) and WRITES it back (lines 288-302, 328). The baseline is always the committed HEAD, but the stored hint is mutated and persisted each run. So the transform is only correct if the hook runs exactly once per HEAD advance. The post-tool-use hook is designed to run after EVERY agent edit, while commits are infrequent, so the second and subsequent runs re-apply the same HEAD->worktree shift to an already-shifted hint, compounding the offset on each invocation.

**Why it matters:** Correct line attribution is the entire purpose of this 'decision memory' tool. Drifting hints make `why <file> <line>` return 'No decision found' for the real code location and attribute the decision to unrelated lines, and corrupt the .dmap derivative and `status` output (spurious STALE). The corruption is silent and triggered by the tool's own intended usage pattern.

**Impact:** Decisions silently point at the wrong code; users querying the true line get nothing while an unrelated line falsely shows the decision. Health reports (status/lint) show fabricated STALE counts. Data integrity of the authoritative .dlog degrades with normal agent activity.

**Likelihood:** High in real use: agentic loops invoke post-tool-use after each tool edit, so multiple runs per commit is the common case, not an edge case.

**Evidence (reporter):** Clean repro (/tmp/av3): file committed with alpha at lines 1-3, decision lines_hint=[1,3]. Edit adds 1 header line, run hook -> [2,4] (correct, alpha now 2-4). Add a 2nd header line, run hook -> [4,6] (WRONG; alpha is at lines 3-5). `archiva why src/app.ts 3` => 'No decision found for src/app.ts at line 3.' while `archiva why src/app.ts 5` => 'fn:alpha ... (lines 4-6) [STALE]'. Even starker (/tmp/av5,/tmp/av7): with NO file change between two consecutive hook runs the hint drifts 3-5 -> 5-7 -> 7-9 and [4,6]->[7,9], and flips 0 stale -> 1 stale. Committing the edit (HEAD catches up) restores idempotency (/tmp/av5: stays [3,5] across reruns), confirming the trigger is multiple hook runs per HEAD baseline. dmap and status both reflect the corrupted [4,6]:STALE (/tmp/av3).

**Independent verification:** Read the exact code path and reproduced the defect twice from scratch with the release binary.

CODE (src/core/project.rs, post_tool_use):
- L272-273: `old_content = read_git_head_file(project_root, old_git_file)...` — baseline is ALWAYS the committed HEAD.
- L275: `line_changes = diff_lines(&old_content, &new_content)` — diff is HEAD vs current worktree.
- L288-289 & L302: `decision.lines_hint = apply_line_changes_to_range(&line_changes, decision.lines_hint.clone())` — the shift is applied to the *already-stored* hint.
- L328-329: `write_dlog(...)` / `write_dmap(...)` — the mutated hint is persisted every run.
src/core/diff.rs:17-44 apply_line_changes_to_range simply translates a range by the net add/remove offset before its start; it has no notion of an absolute baseline, so feeding it an already-shifted hint re-applies the full HEAD->worktree offset.

Thus the transform is correct only if the hook fires exactly once per HEAD advance. Persisting the result while keeping the HEAD baseline fixed makes it non-idempotent.

REPRO 1 (/tmp/av_audit): committed `src/app.ts` with alpha at lines 1-3; `write-decision` stored lines_hint [1,3] (dmap `1-3:fn:alpha`). Added one uncommitted header line (alpha truly now at 2-4; HEAD still 1-3). Then ran `hooks post-tool-use src/app.ts` three times with NO file change and NO commit between runs:
  run #1 -> "0 stale" -> dmap `2-4:fn:alpha`  (correct)
  run #2 -> "1 stale" -> dmap `3-5:fn:alpha:STALE`  (WRONG)
  run #3 -> "1 stale" -> dmap `4-6:fn:alpha:STALE`  (WRONG, compounded)
The offset compounds +1 per invocation even though nothing changed. Mis-attribution confirmed against the corrupted [4,6] state: `archiva why src/app.ts 2` and `... 3` (alpha's true lines) both print "No decision found"; `archiva why src/app.ts 5` falsely prints "fn:alpha dec_001 (lines 4-6) [STALE]". `archiva status` reports "1 stale ... 2 issues" — a fabricated STALE.

REPRO 2 (/tmp/av_audit2): identical setup; after the first hook run (uncommitted) dmap was `2-4:fn:alpha`. I then `git commit`ed the edit so HEAD==worktree. Re-running the hook twice more left dmap stable at `2-4:fn:alpha` both times. This isolates the trigger exactly as the prior auditor stated: the drift occurs only while the worktree diverges from the HEAD baseline and the hook runs more than once; committing (HEAD catching up, empty diff) restores idempotency.

**Verifier notes / severity correction:** The finding is accurate in mechanism, location, evidence, and impact — no corrections needed. The cited lines map precisely: HEAD baseline read at src/core/project.rs:272-273, diff at 275, shift-and-assign at 288-289/302, persist at 328-329.

Precondition (worth stating explicitly for scope, though the auditor already captured it): drift requires (a) the worktree to differ from committed HEAD AND (b) the post-tool-use hook to run more than once against that same HEAD baseline. Both are the normal operating mode — the hook is wired to run after every agent edit while commits are comparatively rare — so the bug fires under ordinary use rather than only in a contrived case. When worktree==HEAD the diff is empty and no drift occurs.

Severity HIGH is correct: this is silent, cumulative corruption of the authoritative .dlog (and derived .dmap/status/lint) under expected agent activity. Decisions migrate away from the code they describe, the true line returns nothing, an unrelated line falsely surfaces the decision, and health metrics report fabricated STALE counts.

Suggested fix direction (not required for the verdict): make re-anchoring idempotent w.r.t. repeated runs against the same baseline — e.g. recompute lines_hint from a stored baseline range rather than mutating-in-place, or skip the shift when the dlog has already been re-anchored for the current HEAD (track the HEAD SHA the hint was last anchored against and short-circuit when unchanged), or prefer the live anchor-extraction position (already available via `extraction.anchors`) as the source of truth when the anchor still exists instead of the diff-shifted hint.

**Recommended resolution:** Make re-anchoring idempotent. When the anchor still resolves in the current extraction (extraction.complete && anchors contains the key), set lines_hint to the extractor's ground-truth info.start/info.end (already computed at project.rs:290-296 but currently used ONLY in the rename+fallback branch) instead of the diff-shifted value. Reserve the diff-shift for orphaned anchors / incomplete extraction where no current position exists. Alternatively, persist the HEAD blob SHA the hint was last shifted against and skip re-shifting when HEAD is unchanged. Add a regression test asserting two consecutive post-tool-use runs (no file change) leave lines_hint and stale/orphan counts unchanged.

---

### F57. [HIGH] Ground-truth anchor line range is computed but discarded in the normal re-anchor path

`architecture` · location `src/core/project.rs:290-304` · reporter-confidence high · verification **CONFIRMED**

**Description:** extract_anchors returns AnchorInfo{start,end} (src/core/anchor.rs:41-44) giving the exact current line range of every anchor found in new_content. In post_tool_use this authoritative position is used only when moved_from.is_some() && using_current_content_fallback (rename with no git baseline). In the ordinary case the code instead trusts shifted_range derived from the HEAD diff, which is both the source of the non-idempotency defect above and strictly weaker than the parser's own answer (the parser re-locates the anchor regardless of how many edits happened).

**Why it matters:** The system already holds the correct answer for any anchor that still exists; preferring a heuristic diff-shift over it is the root architectural cause of attribution drift. Using info.start/info.end whenever the anchor resolves would make re-anchoring exact and naturally idempotent for the common case.

**Impact:** Avoidable attribution errors whenever an anchor is present but its hint was shifted imperfectly; tighter coupling to the fragile diff path.

**Likelihood:** Medium - affects every post-tool-use where the anchor still extracts (the majority).

**Evidence (reporter):** /tmp/av7: extractor correctly knows fn:alpha is at lines 4-6 (first hook produced [4,6]) yet the second hook drifted to [7,9] because shifted_range, not info, is used. The branch at project.rs:291-299 shows info{start,end} is only taken in the rename-fallback sub-case.

**Independent verification:** Code at src/core/project.rs:287-304 matches the claim exactly. For each decision it computes `shifted_range = apply_line_changes_to_range(&line_changes, decision.lines_hint.clone())` (line 288-289), then when the extractor DID find the anchor (`extraction.anchors.get_str(anchor)` is Some, line 290), it assigns `info.start/info.end` ONLY in the sub-branch `moved_from.is_some() && using_current_content_fallback` (lines 291-296); the ordinary case takes `shifted_range` (line 298). AnchorInfo{start,end} from extract_anchors (src/core/anchor.rs:41-47) holds the parser's exact current line range but is thrown away in the normal path.

Reproduced end-to-end with the release binary in /tmp/av_audit (git repo):
1. Committed src/a.js with `function alpha` at lines 1-3; `write-decision` recorded fn:alpha lines_hint [1,3].
2. Edited file to insert 3 header lines so alpha is TRULY at lines 4-6 (verified `grep -n function alpha src/a.js` -> line 4).
3. Repeated `hooks post-tool-use src/a.js` with NO further content change after run 1. lines_hint drifted +3 each run while the extractor relocates alpha to line 4 every time:
   after write: hint=1,3
   run 1: "0 stale" hint=4,6  (correct once)
   run 2: "1 stale" hint=7,9
   run 3: hint=10,12
   run 4: hint=13,15
   run 5: hint=16,18
   Each run re-diffs HEAD(1-3) vs current(4-6), gets +3 offset, and ADDS it to the already-shifted hint instead of trusting info.start=4. This is the non-idempotency: stable file content yields ever-growing wrong ranges and a spurious recurring STALE flag.

User-facing impact verified: with hint at 16-18, `archiva why src/a.js 4` (the TRUE line) returns "No decision found", while `archiva why src/a.js 16` and `archiva why src/a.js fn:alpha` report "lines 16-18 [STALE]". Line-based attribution points at empty space; the parser's correct answer (line 4) is unreachable.

Second facet observed (src/b.js, uncommitted so read_git_head_file fails -> old_content==new_content fallback, but moved_from is None): beta truly at 4-6, hint stays frozen at 1-3 across runs because the Some-branch still selects shifted_range (no offset since old==new) instead of info{4,6}. So even the no-baseline non-rename case discards the authoritative range. Confirms info.start/end is honored only in the rename+fallback sub-case.

**Verifier notes / severity correction:** CONFIRMED and the code location (src/core/project.rs:290-304) and root-cause description are accurate. Two corrections:

1) Severity: the prior auditor's own enclosing report rated this architecture/medium, but the demonstrated behavior is a correctness defect, not merely an architectural weakness. On any anchor whose hint is shifted but still located by the parser, repeated post-tool-use hooks (the normal agent workflow — the hook fires after every edit) produce monotonically diverging line ranges AND a recurring false STALE status, and break line-based `why` lookups entirely. Because post-tool-use is the routine hot path and the corruption compounds with each invocation on unchanged content, I rate this HIGH.

2) Scope: the claim frames it primarily as "strictly weaker than the parser's answer" plus a coupling concern. The concrete user-visible harms are stronger: (a) non-idempotent hint drift on stable content, (b) spurious repeated STALE marking, (c) line-based attribution misses the true location. The fix is trivial and removes the entire fragile diff dependency for the located-anchor case: when `extraction.anchors.get_str(anchor)` is Some, always assign `LineRange{start: info.start, end: info.end}` and only fall back to `shifted_range` in the `else` (anchor-not-found / incomplete-parse) branch. The diff path should be the fallback, not the default. This also fixes the b.js no-baseline case for free.

**Recommended resolution:** Use info.start/info.end as the authoritative hint whenever extraction.anchors contains the anchor (with complete extraction); keep diff-shifting only for the orphan/incomplete path. This subsumes and fixes the idempotency defect.

---

### F58. [LOW] Insertions inside an anchored region do not extend its end (range can no longer cover the anchor body)

`tradeoff` · location `src/core/diff.rs:26-30 (Added only shifts when old_line <= start); src/core/reanchor.ts:51-54` · reporter-confidence high · verification **CONFIRMED**

**Description:** apply_line_changes_to_range only moves a range when added lines fall at or above start; insertions strictly inside [start,end] move neither start nor end, so the stored range stays the same width while the anchor body grows. This is faithful to the oracle and consistent with the project's documented behavior (test post_tool_use_preserves_stored_range_when_anchor_body_grows), so it is a design tradeoff rather than a bug, but it means lines_hint can under-cover the real region after in-region growth.

**Why it matters:** `why <file> <line>` for a line in the grown part of the body will miss the decision; fingerprinting still keys off the original width.

**Impact:** Minor under-coverage of the anchored region after in-region insertions; mitigated by anchor re-extraction if the ground-truth fix above is adopted.

**Likelihood:** Low-medium; common when bodies grow but usually re-resolved on next commit baseline.

**Evidence (reporter):** /tmp/av4: inserting one line inside alpha's body (now lines 1-4) left lines_hint at [1,3] and reported 1 stale.

**Independent verification:** CODE (src/core/diff.rs:17-44): apply_line_changes_to_range shifts both start and end by a single shared `offset`. Offset is incremented for Added blocks ONLY when `old_line <= start` (lines 26-30), and decremented for Removed blocks only when the removed run ends strictly before start (lines 31-36). An insertion whose old_line lands strictly inside (start, end] contributes nothing to offset, so neither start nor end moves — the range keeps its original width while the real region grows.

ORACLE PARITY (src/core/reanchor.ts:51-54): `if (change.added) { if (oldLine <= start) offset += count; continue; }` — identical semantics; end derived from the same offset. So this is deliberate TS parity, not a Rust regression.

CONSUMER PATH (src/core/project.rs:287-304): post_tool_use computes shifted_range via apply_line_changes_to_range and, when the anchor still exists, assigns decision.lines_hint = shifted_range (line 298). The freshly-extracted true range (info.start/info.end, available at lines 294-296) is used ONLY in the moved-file + current-content-fallback branch (line 291-297), never for the ordinary in-place edit, so re-extraction does not correct the width.

CITED TEST EXISTS (src/core/project.rs:1670-1705): post_tool_use_preserves_stored_range_when_anchor_body_grows writes fn:kept at 1-3, grows the body to 4 lines, asserts lines_hint stays {1,3}, status STALE, dmap "1-3:fn:kept:STALE\n". This encodes the documented tradeoff.

LIVE REPRODUCTION against /home/ubuntu/archaeo/target/release/archiva in /tmp/av-audit:
1. init; wrote src/a.ts = `function alpha(){ return 1; }` (3 lines).
2. write-decision fn:alpha lines [1,3] -> dlog lines_hint 1-3, dmap "1-3:fn:alpha".
3. Edited to insert a line inside body: `function alpha(){ let value=1; return value; }` (now lines 1-4).
4. `hooks post-tool-use src/a.ts` -> "Re-anchored src/a.ts: 1 stale, 0 orphan."; dlog lines_hint STILL 1-3 (status STALE), dmap "1-3:fn:alpha:STALE".
5. `why src/a.ts 4` (the new closing brace, true end of region) -> "No decision found for src/a.ts at line 4."; `why src/a.ts 3` and `why src/a.ts fn:alpha` -> hit, reported as (lines 1-3) [STALE].
This matches the prior auditor's /tmp/av4 reproduction precisely.

**Verifier notes / severity correction:** CONFIRMED, and the original scoping (tradeoff / low) is correct — no severity correction needed. Every particular of the claim checks out: the diff.rs:26-30 condition, the reanchor.ts:51-54 oracle counterpart, the dedicated parity test, and the live repro (lines_hint stuck at [1,3], 1 stale, line 4 uncovered by `why`).

Two refinements that REDUCE practical impact below what "under-coverage" alone implies, reinforcing the low severity:
1. In-region growth necessarily changes the anchor body fingerprint, so the decision is marked STALE in the same pass (verified: status STALE, dmap suffix :STALE). The user is therefore already told the region needs re-review; the slightly-short lines_hint is a secondary cosmetic effect on top of an already-flagged-stale decision, not a silent loss of information.
2. `why <file> <anchor>` and `why` at any line within [1,3] still resolve the decision; only the newly-added trailing line(s) of the region (here line 4) fall outside coverage.

One correctness nuance on the proposed "ground-truth fix": the freshly-extracted range (info.start/info.end) is already in hand at project.rs:294-296 and is used in the moved-file fallback branch, so adopting it for the in-place case is low-effort. However doing so would deliberately diverge from the TS oracle and break the existing parity test (post_tool_use_preserves_stored_range_when_anchor_body_grows). That is a genuine design tension, not a free win — re-extending to the true anchor body is arguably more useful to users but sacrifices the stated cross-implementation parity guarantee. Recommend treating as a conscious future-enhancement decision (parity vs. accuracy), not a release blocker.

**Recommended resolution:** Accept as documented tradeoff, or (preferred) adopt the info.start/info.end ground-truth fix which makes the stored range track the real body exactly.

---

## DIMENSION: Security  — score 8/10

> Archiva v2 is, with one important exception, a notably well-hardened std-only codebase against the stated threat model (local CLI parsing untrusted .dlog/.dmap/JSON and git objects from a possibly-malicious repo, plus MCP over stdio). Path handling (paths.rs) rejects absolute/UNC/drive/`..`/NUL/control-char paths and enforces a canonicalize+starts_with symlink-escape check before reading source; I confirmed traversal, absolute-path, and symlink-escape attempts are all rejected by the binary. The native git object reader (git.rs) is meticulous: pack offsets/sizes use checked arithmetic, inflated output is capped per-byte (push_output) at 10MB, loose objects at ~11MB, delta target sizes are bounded, every object is SHA-1/SHA-256 verified after inflation, and `objects/info/alternates` cannot become an arbitrary-read primitive because reads are gated on oid path layout + hash match. The JSON parser enforces depth=512 and a 10MB byte cap; the MCP stdio loop caps each request line at 10MB and frees per-request, so flooding/unbounded-memory is mitigated. settings.json merge writes only hardcoded commands (no injection), and init writes no secrets and runs no commands. I fuzzed the anchor extractor (60k adversarial inputs across TS/JS/Rust/C/C++) and the JSON/MCP path (40k inputs) with zero panics. The single material defect: a malformed single-quoted YAML scalar consisting of a lone `'` triggers an out-of-bounds slice panic (yaml.rs:700), which is reachable from untrusted .dlog content and aborts why/status/session-start and — most seriously — the long-lived MCP server. Because .decisions/ is git-tracked by default, a poisoned .dlog propagates to every collaborator and crashes their agent sessions at startup.

*Score rationale:* Security engineering here is well above average for a from-scratch std-only reimplementation: disciplined checked arithmetic throughout the git pack/delta/zlib paths, per-byte output caps that defeat decompression bombs, mandatory object-hash verification, depth+byte limits on both hand-written parsers, a robust path-validation layer with a real symlink-escape check, and an MCP loop with per-line byte limits. Extensive fuzzing (100k+ adversarial inputs across anchor/JSON/MCP) surfaced no panics in those surfaces. The score is held below 9 by one genuinely reachable parser panic (yaml.rs:700) that escaped a large test suite and, due to default-tracked .decisions/, becomes a cross-collaborator DoS for the long-lived MCP server and the session-start hook — a class of bug (unhardened manual parser panicking on untrusted input) that should be caught by property/fuzz tests gating release. Fixing that one-line guard and adding catch_unwind at the MCP/command boundary would justify a 9.

**Verified behaviors (checked, not assumed):**

- Path traversal blocked: `archiva why ../archiva-sec-secret.txt` → 'parent path segments are not allowed'; absolute path → 'absolute paths are not allowed'; `src/../../x` → rejected (ran binary in /tmp)
- Symlink escape blocked: a symlink src/link.ts → /tmp secret resolved to 'No decisions found' (canonical_source_path_if_exists enforces canonicalize+starts_with(root)); confirmed by paths.rs unix test too
- YAML field-forgery via newlines is contained: write-decision with chose='real\nstatus: ORPHAN\nfingerprint: deadbeef' serialized as a literal `|-` block and round-tripped back to the exact original string via `why` — no field injection
- Anchor field cannot inject YAML structure: a multiline anchor with embedded `injected_key:` was validated against real extracted anchors and rejected ('does not exist ... Available anchors: ...')
- CONFIRMED PANIC: .dlog with a lone single-quote scalar (`id: '`) aborts `why` (rc=101), `status` (rc=101), `hooks session-start` (rc=101), and `mcp` stdio server (rc=101) at src/core/yaml.rs:700:15 'byte range starts at 1 but ends at 0'
- One poisoned .dlog under .decisions/ (unrelated path) crashes repo-wide `status` and `session-start` while a valid decision exists elsewhere — confirms blast radius
- Proposed fix validated in isolation (rustc): adding `input.len() < 2 ||` makes parse_single_quoted("'")→None while preserving ("''")→"" and ("'ab'")→"ab"
- Anchor extractor fuzz: 60,000 adversarial source inputs across .ts/.js/.rs/.c/.cpp/.h/.hpp/.jsx/.tsx/.mjs via `why` → 0 panics
- JSON/MCP fuzz: 40,000 adversarial JSON-RPC lines (incl. surrogate escapes, deep nesting tokens, control chars) via `archiva mcp` → 0 panics, 0 hangs, 0 memory blowups
- git native HEAD:path read works end-to-end via post-tool-use hook (real git repo, SHA-1) and computes correct git-relative path when project_root is a subdir of git_root (sub/src/app.ts)
- init writes only .decisions/, .claude/settings.json (fixed hardcoded archiva commands), AGENTS.md, optional .gitignore — no secrets, no command execution; settings merge rejects non-object settings.json
- Code-read verification: git.rs pack offset/size/delta paths use checked_add/checked_mul/checked_sub with error returns; zlib push_output enforces max_output per byte; loose/packed objects hash-verified (verify_git_object_hash); JSON depth=512 & 10MB caps; YAML depth=512; .dlog/.dmap reads capped at 10MB (TEXT_FILE_MAX_BYTES)
- No unwrap/expect/panic in non-test code of git.rs/yaml.rs/json.rs/dlog.rs/dmap.rs/anchor.rs; remaining indexing in cli.rs/time.rs/diff.rs/decision.rs is length-guarded (verified each guard)

### F59. [HIGH] Lone single-quote in YAML scalar panics the process (reachable from untrusted .dlog → DoS of MCP server, status, and session-start hook)

`defect` · location `src/core/yaml.rs:700 (parse_single_quoted), reached via parse_scalar_value at yaml.rs:422 and parse_mapping_key at yaml.rs:1122` · reporter-confidence high · verification **CONFIRMED**

**Description:** parse_single_quoted() guards only `input.starts_with('\'') && input.ends_with('\'')`. For an input of exactly one `'` character, both predicates are satisfied by the same byte, so execution reaches `input[1..input.len()-1]` = `input[1..0]`, which panics: "byte range starts at 1 but ends at 0". parse_double_quoted() correctly guards with `input.len() < 2`, but the single-quoted path does not. A .dlog scalar value (or block scalar) equal to a lone `'` is parsed as this degenerate scalar and aborts the process (exit 101).

**Why it matters:** The YAML parser consumes .dlog files which are explicitly untrusted (committed to git and shared across a team, and writable by other agents). A single malformed byte sequence in any .dlog under .decisions/ converts every read into a hard crash rather than a graceful schema error. The parser otherwise returns Result errors for malformed input; this one input bypasses all error handling.

**Impact:** Denial of service. Confirmed exit code 101 (abort) on: `why <file> <anchor>` (rc=101), repo-wide `status` (rc=101), the `hooks session-start` command run at the start of EVERY agent session (rc=101), and the long-lived `mcp` stdio server (rc=101) — for the MCP server a single crafted `why`/`ghost_check` tool call, or one poisoned .dlog, kills the whole server process and thus all in-flight sessions. Because `archiva init` tracks .decisions/ by default (only `--gitignore-decisions` excludes it), a poisoned .dlog travels via `git clone`/`git pull` to every collaborator and crashes their session-start hook and status command. No data corruption, but complete availability loss for affected commands.

**Likelihood:** High for a malicious/poisoned repo or hostile collaborator (trivial to craft and commit). Low-to-moderate accidental (a hand-edited .dlog ending a value with a stray quote).

**Evidence (reporter):** Minimal repro in /tmp: .decisions/src/t.ts.dlog containing `file: src/t.ts\nschema: 1\ndecisions:\n  fn:t:\n    id: '\n` then `archiva why src/t.ts fn:t` → `thread 'main' panicked at src/core/yaml.rs:700:15: byte range starts at 1 but ends at 0`, rc=101. Also reproduced repo-wide: an unrelated poisoned `.decisions/lib/x.ts.dlog` made `archiva status` and `archiva hooks session-start` both abort with rc=101 at yaml.rs:700:15 while a valid decision existed for src/t.ts. Discovered via differential fuzzing (~20k random YAML-ish .dlog bodies through the binary; 6 distinct crashing inputs, all the same root cause). Source: yaml.rs:696-701 `if !input.starts_with('\'') || !input.ends_with('\'') { return None; } Some(input[1..input.len()-1]...)`.

**Independent verification:** Code at src/core/yaml.rs:696-701 matches the claim exactly: parse_single_quoted guards only `!input.starts_with('\'') || !input.ends_with('\'')`, then slices `input[1..input.len()-1]`. For input == "'" (len 1), both predicates pass on the same byte and the slice becomes input[1..0], which panics. parse_scalar_value (yaml.rs:422) calls it before any length check, and parse_double_quoted (yaml.rs:703-706) correctly guards `input.len() < 2` while the single-quoted path does not.

Reproduced with the release binary in /tmp/archiva_audit (git init + `archiva init`, rc=0). Wrote .decisions/src/t.ts.dlog containing:
  file: src/t.ts
  schema: 1
  decisions:
    fn:t:
      id: '

All four reachable entry points abort with rc=101 at the same site:
- `archiva why src/t.ts fn:t` -> "thread 'main' panicked at src/core/yaml.rs:700:15: byte range starts at 1 but ends at 0", rc=101
- `archiva status` -> same panic, rc=101 (repo-wide scan hits the poisoned file)
- `archiva hooks session-start` -> same panic, rc=101 (runs at the start of every agent session)
- `archiva mcp` over stdio: initialize (id=1) and tools/list (id=2) succeed, then a single `tools/call` why on src/t.ts produces the panic and the whole server process dies (rc=101) with NO response to id=3, killing all in-flight sessions.

Propagation path confirmed: src/cli.rs:108 `gitignore_decisions = false` by default; only `--gitignore-decisions` (cli.rs:111) excludes .decisions/. src/core/init.rs:54 only writes a gitignore entry when that flag is set, so by default .decisions/ is git-tracked and a poisoned .dlog travels via clone/pull to every collaborator and crashes their session-start hook and status command.

**Verifier notes / severity correction:** Claim is accurate in every particular: root cause, file:line (yaml.rs:700:15), the four affected entry points, exit code 101, the asymmetry vs parse_double_quoted's len<2 guard, and the git-tracked-by-default propagation vector. Severity high is appropriate: complete availability loss for the affected commands (including the per-session session-start hook and the long-lived MCP server) triggerable by a single 1-byte malformed scalar in any tracked .dlog, with no data corruption. Not critical only because it is a clean abort with no memory unsafety, code execution, or data integrity loss. Fix is one line: guard `input.len() < 2` (return None) in parse_single_quoted, mirroring parse_double_quoted; a fuzz/regression test for a lone "'" scalar should accompany it. One scope refinement worth noting: the same degenerate input is also reachable via parse_mapping_key (yaml.rs:1122) for single-quoted keys, so the fix and test should cover both the value and key paths, not just scalar values.

**Recommended resolution:** Add a length guard mirroring parse_double_quoted: `if input.len() < 2 || !input.starts_with('\'') || !input.ends_with('\'') { return None; }`. Validated this fix in isolation: parse_single_quoted("'")→None, ("''")→Some(""), ("'ab'")→Some("ab") all correct, no panic. Add a regression test for the lone-quote scalar AND lone-quote mapping key. Separately, consider catching parser panics at the command boundary / making the MCP serve loop resilient (catch_unwind around per-request handling) so that no single malformed input can ever take down the long-lived server — defense in depth, since std-only manual parsers are panic-prone surfaces.

---

### F60. [LOW] Untrusted git alternates/commondir paths are followed verbatim (object-store confinement relies on oid layout + hash verification, not path containment)

`operational` · location `src/core/git.rs:526-550 (alternate_object_dirs), 472-496 (common_git_dir), 191-214 (git_dir_for_work_tree)` · reporter-confidence high · verification **CONFIRMED**

**Description:** `.git/objects/info/alternates`, `.git/commondir`, and the `.git` gitfile `gitdir:` marker are read from the repo and, when absolute, used verbatim as filesystem locations to search for git objects (recursion bounded at GIT_ALTERNATES_MAX_DEPTH=8 with a canonicalized seen-set to prevent cycles). A crafted alternates file can therefore point archiva's object lookups at arbitrary directories on the host.

**Why it matters:** This matches real git behavior, but in archiva's untrusted-repo threat model it lets a malicious repo steer object reads outside the repo tree. It is NOT an arbitrary-file-read primitive: any candidate path is only opened as `<dir>/<oid[..2]>/<oid[2..]>`, then zlib-inflated and SHA-verified against the requested oid (verify_git_object_hash, git.rs:2236), so content that isn't a valid git object hashing to the exact oid is rejected and never surfaced. Worst realistic effect is information disclosure of whether a specific object-shaped file exists at an attacker-named path, plus extra I/O.

**Impact:** Bounded: probing for existence of attacker-specified paths and modest extra filesystem traversal during HEAD:path resolution (only the post-tool-use re-anchor path reaches this). No content exfiltration without a SHA collision.

**Likelihood:** Low — requires a malicious repo and only yields existence/timing signals.

**Evidence (reporter):** git.rs:543 `if path.is_absolute() { alternates.push(path); }` (verbatim absolute path); git.rs:491 same for commondir; reads gated by validate_oid_hex + read_loose/read_packed which join oid components and then verify_git_object_hash. No test exercises a hostile alternates file pointing outside the repo.

**Independent verification:** CODE (verbatim matches claimed lines):
- src/core/git.rs:542-547 alternate_object_dirs: `let path = PathBuf::from(line); if path.is_absolute() { alternates.push(path); } else { alternates.push(object_dir.join(path)); }` — absolute alternates pushed verbatim, no containment check.
- src/core/git.rs:490-494 common_git_dir: same pattern for `.git/commondir` (absolute → used verbatim).
- src/core/git.rs:208-212 git_dir_for_work_tree: same for the `.git` gitfile `gitdir:` marker.
- Recursion bound confirmed: collect_object_dir (git.rs:498-524) errors at depth > GIT_ALTERNATES_MAX_DEPTH and uses a canonicalized `seen` set (git.rs:513-518) to break cycles.
- Hash gate confirmed: read_git_object_inner (git.rs:575-587) calls validate_oid_hex then, on a successful read, verify_git_object_hash (git.rs:579). verify_git_object_hash (git.rs:2236-2250) recomputes SHA-1/SHA-256 over the `kind+len\0+data` preimage and rejects on mismatch. Loose reads also enforce size/preimage consistency (git.rs:427-455).
- Reach bound confirmed: read_git_head_file is reached only from project.rs:273 (re-anchor / post-tool-use path); its result is wrapped in `.unwrap_or_else(|_| new_content.clone())` so a failed/forged read silently falls back to current file content.

RUNTIME (strace of the release binary driving `hooks post-tool-use`):
- With `.git/objects/info/alternates` = `/tmp/altpoc/external`, archiva opened objects at that absolute path verbatim, e.g. `openat(".../external/e9/4e66...") = 3`, and probed `.../external/info/alternates` for chained alternates.
- With alternates = `/tmp/altpoc/evilstore` (an arbitrary dir outside the repo), archiva probed `/tmp/altpoc/evilstore/info/alternates`, `/tmp/altpoc/evilstore/e9/4e66...`, and `/tmp/altpoc/evilstore/pack` — proving attacker-controlled absolute paths are followed verbatim for existence-probing and extra traversal. Native `git cat-file -p HEAD:src/a.ts` confirmed the same alternates were honored.
- Forged-object substitution is blocked by the git.rs:579 hash gate: any object served from the attacker dir whose content does not hash to the requested oid is rejected, so no content injection without a SHA-1/SHA-256 collision.

**Verifier notes / severity correction:** Claim is accurate in both mechanism and bounded impact. Two contextualizing caveats, neither of which raises severity: (1) Threat model — the attack requires the adversary to already control files inside the victim's `.git/` (the alternates/commondir/gitfile markers). Any attacker with `.git` write access has far stronger primitives in a normal git workflow (e.g. `.git/hooks/*` → arbitrary code execution on the next git command), so archiva's marginal added risk is genuinely low. This is a confinement gap, not a privilege escalation. (2) The realistic exposure is even narrower than "HEAD:path resolution generally": read_git_head_file is reached only on the re-anchor/post-tool-use code path, and its failure is masked by the new-content fallback at project.rs:273, so a hostile alternates file mostly yields silent extra filesystem probing rather than any observable behavior change. Confirmed impact ceiling: existence-probing of attacker-named absolute paths and modest extra directory traversal during re-anchoring; no content exfiltration or substitution absent a hash collision. Recommended resolution (defense-in-depth, optional for v2): when honoring absolute alternates/commondir/gitdir targets, canonicalize and require containment within the work-tree's git dir hierarchy (or an explicit GIT_ALTERNATE_OBJECT_DIRECTORIES-style allowlist), and/or skip alternates entirely since archiva only needs to read its own repo's HEAD blobs. Document the trust assumption that `.git` contents are trusted input.

**Recommended resolution:** Accept as documented git-compat behavior, or optionally confine followed alternate/commondir paths to within the discovered git root (or a configurable allowlist) and skip absolute alternates that escape it. At minimum, document that running archiva inside an untrusted clone will follow its alternates exactly as git would.

---

### F61. [LOW] Default-tracked .decisions/ makes untrusted-parser robustness a supply-chain concern

`architecture` · location `src/cli.rs:107-129 (run_init), src/core/init.rs:54-62, src/core/project.rs (status/session-start scan all .dlog under .decisions/)` · reporter-confidence high · verification **CONFIRMED**

**Description:** `archiva init` tracks .decisions/ in git by default; `.gitignore` exclusion is opt-in via `--gitignore-decisions`. Combined with repo-wide scans in status and session-start that parse every .dlog, the security posture of the whole tool depends on the YAML/dmap parsers never panicking or over-allocating on attacker-authored files received via normal git workflow.

**Why it matters:** It elevates any reachable parser crash (e.g. the yaml.rs:700 finding) from a local-footgun to a cross-collaborator availability issue: clone/pull a repo, run any archiva command, get crashed. It also means parser hardening must be treated as security-critical, not merely robustness.

**Impact:** Amplifies the blast radius of parser defects across a team via git. By itself (with parsers fixed) it is a design tradeoff, not a vulnerability.

**Likelihood:** N/A (design property).

**Evidence (reporter):** cli.rs:108 `let mut gitignore_decisions = false;` (default tracked); init_help text at cli.rs:447 describes `--gitignore-decisions` as opt-out of tracking; project.rs status path loads every dlog via load_dlog → parse_dlog_yaml.

**Independent verification:** Every factual sub-claim verified by reading source and running the binary.

1) Default-tracked, gitignore is opt-in: src/cli.rs:108 `let mut gitignore_decisions = false;` and the loop at cli.rs:109-129 only flips it to true on `--gitignore-decisions`. src/core/init.rs:54-62 gates the .gitignore write behind `if gitignore_decisions`. init_help (cli.rs:447) confirms the flag means "add .decisions/ to .gitignore instead of tracking decisions" (i.e. tracking is the default).

2) Live repro: `cd /tmp/av2test && git init && archiva init` produced NO .gitignore ("(no .gitignore created)"). A `.decisions/src/x.ts.dlog` file then staged cleanly via `git add -A` — `git status --porcelain` shows `A  .decisions/src/x.ts.dlog`. Confirms decisions enter normal git workflow by default.

3) Repo-wide scan that parses every .dlog: src/core/project.rs:69-83 session_start() calls list_dlog_files() (lists all `*.dlog` under .decisions via list_storage_files, project.rs:55-59) then `load_dlog(...)` on each, which routes to parse_dlog_yaml. status() (project.rs:87) → load_project_status_summaries which iterates the same set (project.rs:74-80 / 404).

4) Live repro of attacker-file parsing during scan: dropping a malformed `.dlog` (missing required fields) and running `archiva status` produced `decisions: missing required field` — proving status loads and parses arbitrary repo-resident .dlog content. A team member pulling such a file via git triggers the parser on it with no opt-in.

The architectural fact (default-tracked + repo-wide unconditional parse of every .dlog) is exactly as described, so the YAML/dmap parser robustness becomes a property the whole team inherits over git.

**Verifier notes / severity correction:** Claim is accurate as stated, including its own self-limiting framing: this is an amplifier/design tradeoff, not a standalone vulnerability. The phrase "By itself (with parsers fixed) it is a design tradeoff, not a vulnerability" is correct — the architecture only converts parser defects into a supply-chain concern; it introduces no exploit on its own. Severity low is appropriate; I would not inflate it. Two refinements for accuracy: (a) the threat model requires the parsers to actually be exploitable (panic/over-allocation) — I did not audit parser robustness here, so the "supply-chain concern" is conditional on a separate parser finding; on a clean parser this reduces to pure technical-debt/operational-risk territory. (b) Mitigation worth noting in the resolution: making `--gitignore-decisions` the default (or printing a one-line notice that .decisions/ will be committed) would shrink blast radius, but the tool's core value proposition is sharing decision memory across a team via git, so untracked-by-default would undercut the product intent — the better fix is hardening the parsers (bounded allocation, no panics on malformed input) rather than changing the tracking default.

**Recommended resolution:** No change required to the default if sharing decisions is the product goal, but: (1) treat all .dlog/.dmap/YAML parsing as a security boundary in tests (fuzz/property tests for non-panicking on arbitrary bytes — the property_tests.rs harness exists and should be extended to cover degenerate quote/escape inputs), and (2) consider parsing each .dlog defensively during repo-wide scans so one corrupt file degrades to a per-file warning rather than failing the entire status/session-start run.

---

### F62. [INFO] MCP server has no per-process resource ceiling beyond per-line byte cap (acceptable for local stdio)

`operational` · location `src/mcp.rs:206-267 (serve_reader_writer), 275-320 (read_protocol_line_with_limit)` · reporter-confidence high · verification **CONFIRMED**

**Description:** Each JSON-RPC request line is capped at json::DEFAULT_MAX_BYTES (10MB) and processed then dropped, so there is no unbounded accumulation across requests. There is no cap on total number of requests or cumulative CPU, but that is inherent to a local stdio server the user themselves launches.

**Why it matters:** Confirms the flooding/unbounded-memory concern in the threat model is adequately handled for a local CLI; noted for completeness so it isn't mistaken for a gap.

**Impact:** None beyond expected local resource use.

**Likelihood:** N/A.

**Evidence (reporter):** mcp.rs:214 read_protocol_line_with_limit(&mut reader, json::DEFAULT_MAX_BYTES); oversize lines return a -32700 error and the session continues (verified by mcp_stdio_rejects_oversized_request_and_continues_session test and by 40k-iteration MCP fuzz with no hang/panic/memory blowup).

**Independent verification:** Code: src/mcp.rs:214 caps each line at json::DEFAULT_MAX_BYTES (=10*1024*1024, src/core/json.rs:5). Oversize lines (mcp.rs:218-234) emit a -32700 error and `continue` the read loop rather than terminating the session. read_protocol_line_with_limit (275-320) uses sentinel_limit = max_bytes.saturating_add(1) and stops copying into the buffer once the cap is reached (copy_len = remaining.min(chunk_len)), so the held buffer is bounded at ~10MB even for an arbitrarily long line; it still drains the remainder via consume(). ProjectToolHandler (mcp.rs:96-105) carries only project_root — no per-request state accumulates. Each response is written/flushed then dropped.

Runtime (built release binary):
1) Fed a 10,485,825-byte first line (10MB of '{' + overflow) followed by a valid `initialize` line. Output: {"...error":{"code":-32700,"message":"JSON input exceeds configured byte limit"}} THEN {"...id":1,"result":{...serverInfo...}} — proving the oversize line is rejected and the session continues. /usr/bin/time -v peak RSS = 12,416 KiB (~12MB), i.e. buffer bounded by the cap, not by the true line length.
2) Fed 200,000 sequential `initialize` requests: server emitted 200,000 responses, peak RSS = 2,560 KiB — flat, confirming no cumulative memory growth across requests.

**Verifier notes / severity correction:** Claim is accurate as written. The per-line 10MB cap is enforced and memory-bounded (does not buffer the full oversize line), the session survives oversize/non-UTF-8 lines, and there is no per-request accumulation. The only uncapped resources (total request count, cumulative CPU/wall time) are inherent to a local stdio server the user themselves launches and feeds, so info severity is correct. No correction to scope or recommendation needed.

**Recommended resolution:** No action required. Absence of authn is expected and correct for a local stdio server.

---

## DIMENSION: Performance and Scalability  — score 6/10

> Archiva v2 stores decisions repo-local as per-source-file .dlog YAML (authoritative) plus a compact .dmap derivative index, and exposes status/lint/session-start/why/history/post-tool-use/write-decision plus an MCP stdio server. Memory behavior is excellent: every repo-wide command streams one .dlog at a time (status/lint at 2000 decisions + 10000 source files held RSS at ~4MB), so nothing loads the whole repo into memory. The git object reader is well-engineered (fanout + binary search per pack .idx, delta resolution; not a scale risk). However there are two measured super-linear blowups that defeat the project's headline scaling claims: (1) the TS/JS and Rust/C anchor extractors are O(n^2) in declarations-per-file because is_top_level() re-scans tokens[..index] for every candidate token across ~10 separate full-token loops — a single 1.3MB / 20k-function TS file takes 66s to lint (RSS 69MB), and 2k/4k/8k functions measured 0.44/2.28/8.54s; (2) the write path is O(current dlog size) per write, so accumulating decisions in one hot file is O(n^2) cumulative — 1200 decisions into one .dlog took 86s and 1500 timed out at 2 minutes. Compounding inefficiencies: the .dmap index is never read by any command (dead derivative — every read re-parses verbose .dlog YAML), status parses every .dlog at least twice, and write_dlog re-parses its own rendered output on every write. Critically, the scale-smoke harness is blind to all of this: it caps corpus files at 256KB, uses 1 decision/file (seeded 10), and tiny one-function synthetic files, so it exercises a best case and never touches either quadratic path.

*Score rationale:* Memory discipline is genuinely strong (streaming, ~4MB RSS at 2000 decisions/10k files) and the git object reader (fanout + binary search) is well done. But two measured super-linear blowups — O(n^2) anchor extraction per file and O(n^2) cumulative hot-file writes — directly undermine the project's central large-repo/long-horizon scaling claim, and they are compounded by a dead .dmap index, a double-parsing status command, and a redundant re-parse on every write. Most damaging for release-readiness: the scale harness is designed in a way that cannot observe any of these, so the green scale signal is not trustworthy for the scalability dimension. Correctness and safety appear solid; raw algorithmic scalability and the validation harness need work before the stated scale claims hold.

**Verified behaviors (checked, not assumed):**

- Built /tmp project with 4000 TS source files and seeded 2000 decisions via the release CLI; status=3.67s, lint=3.53s, session-start=0.03s, why<0.01s; max RSS stayed 3-4.6MB throughout (no whole-repo in-memory load).
- Added 6000 more undecided source files (10001 total): lint=3.76s, status=3.66s — adding source files barely moved the needle, confirming dlog operations (lock+parse+fsync ~1.8ms each) dominate, not source walking, at this file size.
- Single large file extraction is O(n^2): TS lint at 2000/4000/8000 functions = 0.44/2.28/8.54s; a 1.3MB/20000-function TS file = 66.37s wall, 69400KB RSS. Rust .rs at 2000/4000/8000 = 0.22/0.99/4.04s (same curve). 256KB file (harness cap) = 1.86s.
- Hot-file write path is O(n^2) cumulative: writing decisions into one .dlog via the CLI — #300 cumulative 18.9s, #600 39.2s, #900 61.5s, #1199 85.8s; a 1500-decision single-file run exceeded a 120s timeout. Per-write latency climbed 57ms -> 96ms as the file grew 245B -> 260KB.
- Confirmed by code+grep that load_dmap has no non-test callers (cli.rs/mcp.rs/project.rs/decision.rs) — the .dmap index is written and validated but never read as a fast path; status/session-start/why all use load_dlog.
- Confirmed status() runs both load_project_status_summaries and a full lint_project_issue_count, parsing every .dlog at least twice and walking the full source tree once per status call.
- Confirmed write_dlog (storage.rs:133) re-parses its own freshly-rendered YAML on every write, and atomic_write (fs.rs:131) + lock create (fs.rs:319) each fsync per write.
- Inspected the git object reader: pack index lookup uses the 256-entry fanout table + binary search (git.rs:702-728); find_packed_object iterates pack .idx files (typically 1-few) — O(packs x log objects), not a scale risk.
- Reviewed tools/archiva-scale-smoke.ts: synthetic files are single small functions (renderSource), corpus files >256KB are skipped (corpusMaxFileBytes), decisionsPerFile defaults to 1 (seeded 10), and the 1M-decision seeded path writes artifacts directly to disk rather than via the write-decision CLI — so the harness exercises neither quadratic path.

### F63. [HIGH] Anchor extraction is O(n^2) per file: is_top_level() re-scans the token prefix for every declaration

`defect` · location `src/core/anchor.rs:6678 (is_top_level), called from collect_function_anchors:2238, collect_class_anchors:2320, collect_variable_anchors:3025/3075/3110/3212, collect_export*:3688/3737/3815, and is_declared_declaration:6656` · reporter-confidence high · verification **CONFIRMED**

**Description:** extract_anchors runs ~10 separate `for index in 0..tokens.len()` loops, and most guard each candidate token with is_top_level(tokens, index), which itself does `for token in &tokens[..index] { brace depth }` — an O(index) scan. For a file with n top-level declarations (n proportional to token count), this is O(n^2). is_declared_declaration (line 6655-6656) and the leading_export_index fold (6689-6696) add more prefix re-scans. The Rust and C/C++ extractors exhibit the same quadratic shape. Memory is also non-trivial: 69MB RSS on a 1.3MB input.

**Why it matters:** This directly contradicts the project's claim to scale to large repos / long-horizon corpora (Linux kernel, LLVM). lint and status walk and extract anchors for EVERY source file in the repo every run; a single large or generated/minified file stalls the whole command. status counts undecided complexity across all source files, so this fires even with zero decisions.

**Impact:** lint/status latency explodes on repos containing any large source file. A 1.3MB TS file = 66s for one file; a 5-10MB generated/bundled file would take many minutes to tens of minutes, effectively hanging the command. SOURCE_FILE_MAX_BYTES is 128MB (fs.rs:17) with no token/line guard before extraction, so pathological files are accepted.

**Likelihood:** high — large bundled/generated/minified JS/TS, big C++ translation units, and large Rust files are common in real repos and in the very corpora the README cites.

**Evidence (reporter):** Measured with the release binary on single-file projects: TS lint N=2000 funcs=0.44s, N=4000=2.28s (5.2x for 2x input), N=8000=8.54s (3.7x); a 1.3MB/20000-func file = 66.37s wall, 69400KB max RSS (`/usr/bin/time -v archiva lint`). Rust .rs: N=2000=0.22s, 4000=0.99s, 8000=4.04s — same quadratic curve. A 256KB TS file (the harness's own cap) already = 1.86s for one file. Code path confirmed: is_top_level at anchor.rs:6680 `for token in &tokens[..index]`.

**Independent verification:** Code path confirmed by reading src/core/anchor.rs: is_top_level (line 6678-6680) does `for token in &tokens[..index]` (O(index) prefix scan over brace depth), invoked as a guard inside ~20 separate `for index in 0..tokens.len()` collector loops (function@2238, class@2320, variable@3025/3075/3110/3212, export/import@3688/3737/3815, enum@4320, type-like@5355, etc.), plus a second prefix fold in the leading_export_index branch (6688-6696). Net per-collector cost is O(n^2) in token count.

Empirical (release binary /home/ubuntu/archaeo/target/release/archiva, isolated single-file lint, /usr/bin/time):
- TS N=2000 -> 0.46s/9.8MB; N=4000 -> 2.11s/17MB (4.6x time for 2x input); N=8000 -> 8.28s/31MB (3.9x). Quadratic curve confirmed.
- TS 1.45MB / 20000-func file -> 55.65s wall, 72040KB max RSS for ONE file.
- Rust .rs: N=2000 -> 0.18s; 4000 -> 0.88s (4.9x); 8000 -> 3.24s (3.7x). Same quadratic shape confirmed in the native Rust extractor.

No pre-extraction guard: extract_anchors (anchor.rs:241) runs tokenization + all collectors unconditionally; the only ceiling is SOURCE_FILE_MAX_BYTES = 128*1024*1024 (fs.rs:17). lint walks and extracts source-file anchors even with ZERO decisions present (my 55s run was on a freshly init'd repo with no .dlog decisions), so impact does not depend on prior decision data.

**Verifier notes / severity correction:** Claim is fully confirmed in mechanism (is_top_level prefix re-scan), curve (quadratic for both TS and Rust extractors), large-file latency (55-66s for ~1.4MB single file), and absence of any token/line guard before extraction (128MB byte cap only). One scoping correction: this is severe BOUNDED latency degradation / DoS-on-pathological-input, not unbounded hang or OOM. The command does complete and memory stays modest (~70MB for 1.45MB input — roughly 50x file size, linear in input, not explosive). The realistic trigger is a single large multiline source file (vendored/bundled JS, generated clients, large generated .ts/.rs) which makes lint/status take tens of seconds to minutes. Recommended fix: add a pre-extraction token-count or byte guard (e.g., skip or emit an incomplete-extraction diagnostic above a threshold well below 128MB), and/or eliminate the O(n) is_top_level prefix scan by tracking running brace depth in a single pass and tagging each token with its depth (or precomputing a prefix-depth array once per file), reducing extraction to O(n).

**Recommended resolution:** Precompute a per-token brace-depth array once (single O(n) pass) and have is_top_level/is_declared_declaration do an O(1) lookup instead of re-scanning the prefix. Same for the leading_export_index depth fold. Add a token-count or byte-size threshold above which a file is skipped with an arc/parser-style note rather than scanned, so one giant file cannot hang a repo-wide command.

---

### F64. [HIGH] Hot-file write path is O(n^2) cumulative: each write_decision re-renders, re-parses, and atomically rewrites the entire .dlog

`defect` · location `src/core/storage.rs:131 (write_dlog), 229-261 (write_decision_record_locked); render+parse+atomic_write each call` · reporter-confidence high · verification **CONFIRMED**

**Description:** Every write-decision/post-tool-use/lint-fix loads the full .dlog, mutates it in memory, then write_dlog renders the whole file, re-parses the rendered YAML for validation (line 133), and atomic_write_text rewrites the entire file with an fsync (fs.rs:131) plus a lock acquire/release with its own fsync (fs.rs:319). Cost per write grows linearly with the number of decisions already in that file, so seeding/accumulating m decisions in one source file is O(m^2) total work and bytes written.

**Why it matters:** The README/seeded harness advertise 1M-decision scale. Decisions cluster by source file; a frequently-edited or decision-dense file accumulates many entries, and every subsequent write re-serializes and re-fsyncs the growing file. There is no append or partial-update path.

**Impact:** Recording the 1200th decision in a single file took ~96ms and the cumulative time to reach 1200 was 85.8s; 1500 decisions in one file exceeded a 2-minute timeout. Write throughput for hot files degrades from ~30ms to 100ms+ and keeps climbing; total bytes written grows quadratically.

**Likelihood:** medium — depends on decision density per file, but the supersede/post-tool-use workflows naturally concentrate decisions per file, and the seeded scale config uses 10 decisions/file deliberately.

**Evidence (reporter):** Measured single-file accumulation via CLI: decision #0=57ms (dlog 245B), #300=68ms cumulative 18.9s (64KB), #600=75ms cumulative 39.2s (130KB), #900=83ms cumulative 61.5s (195KB), #1199=96ms cumulative 85.8s (260KB). A 1500-write run timed out at 120s. write_dlog re-parses its own output: storage.rs:133 `parse_dlog_yaml(&rendered)?` runs on every write.

**Independent verification:** Read storage.rs:131-135 (write_dlog): render_dlog_yaml renders ALL decisions, then parse_dlog_yaml(&rendered) RE-PARSES the entire rendered output on every single write (line 133, ungated — not behind any cfg/debug flag), then atomic_write_text rewrites the whole file. write_decision_record_locked (storage.rs:229-261) calls load_or_create_dlog (full read+parse via load_dlog storage.rs:26-33), mutates in memory, then write_dlog + write_dmap, each an atomic rewrite. atomic_write_bytes_impl (fs.rs:131) fsyncs the full payload via sync_all; lock path has its own fsync (fs.rs:319). render_dlog_yaml->dlog_to_yaml and parse_dlog_value (dlog.rs:58,73) both iterate the entire decisions map, so per-write work is O(decisions-in-file), cumulative seeding O(n^2).

Reproduced via /home/ubuntu/archaeo/target/release/archiva in /tmp/archiva_perf (git init + archiva init, single src/main.rs, write-decision --json loop, schema lines:[s,e]/chose/because/rejected). Accumulating 1300 decisions into ONE file:
  #0:    write=38.7ms  dlog=270B
  #300:  write=52.1ms  dlog=71456B   cumulative=12.2s
  #600:  write=47.9ms  dlog=143174B  cumulative=26.4s
  #900:  write=60.2ms  dlog=214892B  cumulative=42.0s
  #1199: write=57.8ms  dlog=287578B  cumulative=59.5s
  #1299: write=66.5ms  dlog=312086B  cumulative=65.7s  TOTAL=65.7s
Per-300-decision block cost rises monotonically (12.2->14.2->15.6->17.5s), confirming superlinear cumulative cost; dlog size grows strictly linearly to 312KB. Process-spawn baseline is negligible (median 0.7ms via --help, 15 runs), so per-write latency growth is genuine file work, not startup. The redundant self re-parse at storage.rs:133 is real and unconditional.

**Verifier notes / severity correction:** Every architectural assertion is verified: O(n) per write, O(n^2) cumulative, linear byte growth, and the unconditional self re-parse at storage.rs:133. The claim's absolute numbers are slightly pessimistic versus this host (I measured 66ms at ~#1300 and 65.7s cumulative vs the claimed 96ms / 85.8s; faster disk here), but the asymptotic behavior and code path match exactly. Severity high is appropriate but with scope caveat: decisions are keyed per source file, so this only bites a single HOT source file that accumulates many hundreds-to-thousands of distinct decisions — unusual but realistic in a long-lived repo, and the re-parse-on-every-write (storage.rs:133) is pure wasted work that doubles parse cost for zero correctness benefit in release builds. Recommended fixes: (1) gate the storage.rs:133 self re-parse behind cfg!(debug_assertions) or remove it (the renderer is already exercised by tests); (2) consider an append-friendly on-disk format or batched/coalesced writes for hot files so per-write cost is not proportional to total accumulated decisions.

**Recommended resolution:** Drop the redundant re-parse of freshly-rendered YAML in write_dlog (it validates output the code just produced). For high-density files, consider a bounded per-file decision count or a chunked storage layout. At minimum, document the per-file decision-count expectation and avoid re-fsyncing both lock and data on every write where durability allows batching.

---

### F65. [MEDIUM] .dmap derivative index is never read by any command — every read re-parses the verbose .dlog YAML

`architecture` · location `src/core/storage.rs:58 (load_dmap) and dmap_entries_from_dlog; consumers grep shows zero non-test callers in cli.rs/mcp.rs/project.rs/decision.rs` · reporter-confidence high · verification **CONFIRMED**

**Description:** The system maintains a compact .dmap index (start-end:anchor[:STATUS]) alongside each .dlog and spends effort keeping it current (ensure_dmap_current on status/session-start/lint/load-dmap). But no command actually reads .dmap as a fast path: status uses status_summary_from_dlog (full .dlog), session-start uses load_dlog, why/history use load_dlog. load_dmap is only referenced from storage.rs itself and tests. The .dmap is pure write-amplification.

**Why it matters:** The .dmap exists precisely to be the cheap, scannable index for repo-wide reads, yet status/session-start parse the full multi-field YAML for every file instead. This is the single biggest missed optimization for the 'scan the whole repo' commands and is why status/lint cost ~1.8ms/dlog rather than the much cheaper dmap line parse.

**Impact:** status/session-start do far more parsing and allocation than necessary at repo scale, and every mutating command pays to keep an index nothing consumes (extra render + atomic write + fsync of .dmap on each write).

**Likelihood:** high — affects every status/session-start/lint and every write.

**Evidence (reporter):** `grep -rn load_dmap` outside storage.rs/tests returns nothing. session_start (project.rs:78) and load_project_status_summaries (project.rs:404) both call load_dlog, not load_dmap. write_dmap is called on every write (storage.rs:257) and ensure_dmap_current re-renders+compares on every status/session-start.

**Independent verification:** SOURCE: `grep -rn "load_dmap"` returns only the definition at src/core/storage.rs:58 and test-module references (storage.rs:266,288,339,691-824). Zero non-test callers in cli.rs, mcp.rs, project.rs, decision.rs. Read paths confirmed: why/why_for_line/history (project.rs:39-52) all call load_dlog; status -> load_project_status_summaries (project.rs:399-410) calls load_dlog + status_summary_from_dlog (full .dlog parse) then ensure_dmap_current; session_start (project.rs:78-79) calls load_dlog + ensure_dmap_current. Write paths call write_dmap on every mutation (storage.rs:128,165,180,257; project.rs:329,546). ensure_dmap_current (storage.rs:100-118) does render_dmap_from_dlog + read file + string-compare, then write_dmap on mismatch.

RUNTIME (/tmp/dmaptest, real binary): wrote dec_001 (anchor fn:foo) -> .decisions/a.ts.dlog AND a.ts.dmap both created (.dmap content "1-1:fn:foo"). (1) Deleted a.ts.dmap, ran `why a.ts fn:foo` -> full correct output ("fn:foo dec_001 ... Chose/Because/Recorded"); .dmap was NOT recreated -> why neither reads nor needs it. (2) Overwrote a.ts.dmap with "GARBAGE-NOT-A-VALID-LINE", ran `why` again -> still correct output, corrupt .dmap ignored -> proves .dmap is not on the read path. (3) Deleted .dmap, ran `status` -> .dmap recreated -> confirms ensure_dmap_current write-amplification on a read command.

**Verifier notes / severity correction:** CONFIRMED as stated: the .dmap derivative index has zero in-binary consumers; every command reads the authoritative .dlog, and the .dmap is maintained (rendered/compared on status/session-start/lint, atomically rewritten on every mutation) purely as write-amplification. The line->anchor index it encodes would naturally serve why_for_line, yet that path also uses load_dlog, so even the one plausible consumer ignores it.

One correction to the claimed IMPACT: the assertion that status/session-start "do far more parsing and allocation than necessary" overstates the savings a consumed .dmap would yield. status_summary_from_dlog needs full decision records (id, chose, because, status, fingerprints), which the compact .dmap (start-end:anchor[:STATUS]) cannot supply — so status MUST parse the full .dlog regardless; .dmap could never be a fast path for it. The real cost is therefore the extra work to MAINTAIN an unused index (one render+read+string-compare per file on every status/session-start/lint, plus one extra atomic_write+fsync per mutation), not foregone read acceleration. Magnitude is modest: .dmap is ~one short line per decision, small next to .dlog YAML I/O. Net: a genuine architecture finding (a fully-maintained index with no consumer is dead write-amplification and a correctness/consistency liability for no benefit) correctly rated medium; the scalability framing is the weakest part of the original claim. Recommended resolution: either wire load_dmap into why_for_line / a line-index fast path to justify it, or drop .dmap generation+maintenance entirely and treat .dlog as the sole store.

**Recommended resolution:** Either make status/session-start read .dmap (which already carries lines+anchor+status — sufficient for the status summary and session map) and only fall back to .dlog when the dmap is stale/missing, or drop the .dmap entirely if it has no reader. Keeping a write-amplifying index with no reader is the worst of both.

---

### F66. [MEDIUM] status parses every .dlog at least twice per invocation

`defect` · location `src/core/project.rs:87-91 (status), 399-410 (load_project_status_summaries), 114-162 (lint_project_inner via lint_project_issue_count)` · reporter-confidence high · verification **CONFIRMED**

**Description:** status() first calls load_project_status_summaries (loads + ensure_dmap_current for every .dlog) and then lint_project_issue_count, which independently lists all .dlog files again, takes a per-file lock, loads each .dlog again, runs full lint (including extract_anchors over every source file and a full source-tree walk for undecided complexity). So each .dlog is parsed twice and the entire source tree is walked once, just to print decision totals plus an issue count.

**Why it matters:** status is the most likely command an agent runs frequently for a health check; it is implemented as 'summaries + a full lint pass', making it the most expensive read command rather than the cheapest.

**Impact:** At 2000 decisions + 10000 source files, status = 3.66s, essentially the same as a full lint (3.76s) plus the summary pass — double the necessary .dlog parsing and a full extract_anchors sweep just to produce a single issue count.

**Likelihood:** high — every status invocation.

**Evidence (reporter):** Measured: status 3.66s vs lint 3.76s at 2000 dlogs/10001 source files, RSS ~4.6MB. Code: status() calls both load_project_status_summaries (project.rs:404 load_dlog per file) and lint_project_issue_count (project.rs:128 list_dlog_files again, :135 lint_dlog_locked -> :421 load_dlog again, plus :151 lint_complex_undecided which walks all source via list_lint_source_files).

**Independent verification:** CODE: status() (src/core/project.rs:87-91) makes two independent top-level calls. Pass 1 = load_project_status_summaries (project.rs:399-409): list_dlog_files + load_dlog per file (line 404) + ensure_dmap_current + status_summary_from_dlog. Pass 2 = lint_project_issue_count -> lint_project_inner(fix=false, collect_issues=false) (project.rs:122-176): list_dlog_files AGAIN (line 128), and per file lint_dlog_locked (line 135) -> with_decision_file_lock -> load_dlog AGAIN (line 421) + lint_dlog (line 459) which runs extract_anchors on that file's source (line 464); then lint_complex_undecided (line 552) which walks the ENTIRE source tree via list_lint_source_files (line 559) and runs extract_anchors on every file (line 580). No caching/shared state between the two passes, so each .dlog is parsed twice and the full source tree is walked+anchor-extracted once, just to produce decision totals plus one issue count.

RUNTIME: Built a /tmp project with 2000 .dlog files and 10000 .rs source files (2000 decision-backed via `archiva write-decision`, 8000 undecided). Using /usr/bin/time -v:
- `archiva status`: 3.69s / 3.60s / 3.57s wall, Max RSS ~4468-4608 KB. Output ends "Total: 2000 decisions  0 stale  0 orphan  0 issues".
- `archiva lint`: ~3.39s wall, Max RSS ~4224 KB. Output "No decision issues found."
status >= lint (status is even slightly slower because it adds the summary pass on top of equivalent lint work), instead of being far cheaper as a summary-only command should be. This matches the claim's 3.66s vs 3.76s within measurement noise and the ~4.6MB RSS figure.

**Verifier notes / severity correction:** Claim confirmed on all substantive points: (1) each .dlog is loaded/parsed twice per status invocation (load_dlog at project.rs:404 in the summary pass, then again at project.rs:421 via lint_dlog_locked in the lint pass); (2) status performs a full extract_anchors sweep over the entire source tree (lint_complex_undecided -> list_lint_source_files at project.rs:559, extract_anchors at 580) purely to compute one issue count; (3) status costs essentially the same as a full lint at 2000 dlogs / 10000 source files (~3.6s). Severity medium is correct: real, reproducible redundant work with no functional impact, invisible at small scale, noticeable (~3.6s) at large scale. One minor correction to the claim's incidental ordering: in my measurements status was marginally SLOWER than lint (3.6s vs 3.4s), not faster — consistent with status doing lint's work plus the extra summary pass; the claim's "status 3.66 vs lint 3.76" ordering is within noise and does not affect the finding. Recommended resolution: have status compute summaries and the issue count in a single dlog pass (reuse the loaded DlogFile for both summary and lint counting), and avoid the redundant list_dlog_files; the per-undecided-file complexity check is the only part that legitimately needs a source-tree walk and could be made optional or summarized cheaply for the status view.

**Recommended resolution:** Compute summaries and issue counts in a single pass over the dlog set (lint already loads each dlog and knows stale/orphan counts), and reuse the loaded dlog for the status summary instead of reloading. Avoid the full source-tree extract_anchors sweep when only an aggregate issue count is needed, or cache anchor extractions across the two passes.

---

### F67. [MEDIUM] Scale-smoke harness exercises a best case and is blind to both quadratic paths

`operational` · location `tools/archiva-scale-smoke.ts:127 (decisionsPerFile default 1), :142 (corpusMaxFileBytes 256KB), :150 (seeded decisionsPerFile 10), renderSource:771-786 (tiny one-function files)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The benchmark/scale harness the project relies on to validate scaling never feeds either bottleneck: (a) synthetic source files contain a single small function (renderSource), so extract_anchors always runs on a near-empty token stream; (b) corpus files are skipped if larger than 256KB (corpusMaxFileBytes), capping single-file extraction cost; (c) decisionsPerFile defaults to 1 (seeded 10), so no .dlog ever accumulates enough decisions to expose the O(n^2) write path. The seeded 1M-decision config writes artifacts directly to disk (seedSyntheticDecisionArtifacts) rather than via write-decision, so it never measures the quadratic CLI write path at all, only read/lint over many small files.

**Why it matters:** The headline 'scales to 100k files / 1M decisions / Linux-kernel-class corpora' claim is validated by a harness whose design choices specifically avoid the two cases that break. This is the gap between 'tests pass' and 'actually scales'.

**Impact:** A real large file or a decision-dense file in production hits 66s / 86s+ stalls that the green scale-smoke run never surfaces. Release-readiness signal is misleading for the scalability dimension.

**Likelihood:** high — the harness defaults are what CI runs.

**Evidence (reporter):** renderSource (tools/archiva-scale-smoke.ts:771-786) emits one ~5-line function per file. selectCorpus skips files with stat.size > corpusMaxFileBytes (:1240, default 256*1024 at :142). seededConfig uses decisionsPerFile 10 (:150) and seeds via direct file writes (seedSyntheticDecisionArtifacts:826), not the CLI write path. Contrast with my measurements where a single 1.3MB file = 66s and 1200 decisions in one .dlog = 86s.

**Independent verification:** HARNESS CLAIMS (all confirmed by reading tools/archiva-scale-smoke.ts):
- :127 fullConfig decisionsPerFile default 1; :133 parity default 1; :150 seeded 10. With decisionsPerFile=1, every synthetic decision lands in its OWN .dlog (one source file per decision via sourceFile(fileIndex), fileIndex=floor(index/decisionsPerFile)), so no .dlog ever accumulates >1 decision in the full/parity scenarios.
- renderSource (:771-786) emits exactly `functionsPerFile` (= decisionsPerFile) tiny ~5-line functions per file, so source files are 5-50 lines and extract_anchors runs on a near-empty token stream.
- :142 corpusMaxFileBytes=256*1024; :1240 `if (!stat.isFile() || stat.size > corpusMaxFileBytes) continue;` — corpus files >256KB are skipped, capping single-file extraction cost.
- :367 the seeded path calls seedSyntheticDecisionArtifacts(root, config) which (:826-838) writes .dlog/.dmap directly to disk via fs.writeFile, NOT via the write-decision CLI. The full-scale decision.write phase (:289-293) uses the CLI but with decisionInput (:746) pointing each decision at a distinct file, so the CLI quadratic write path is never exercised either.

UNDERLYING QUADRATIC PATHS (confirmed by running /home/ubuntu/archaeo/target/release/archiva):
1) dlog accumulation write path. Source held at 1300 lines, growing one .dlog: 1..100 writes=71ms/write, 101..300=75, 301..600=80, 601..900=87, 901..1200=111ms/write; cumulative ~106s to reach 1200 decisions in one .dlog. Per-write cost rises monotonically with dlog size (super-linear total) — consistent with the claimed ~86s. Control: 50 writes each to its OWN tiny file (dlog always size 1) = 7ms/write flat.
2) large-file extraction. write-decision on a single growing file: 99KB=328ms, 303KB=3420ms, 607KB=14032ms (2x size -> ~4x time = clean O(n^2)); extrapolates to ~64s at 1.3MB (matches claimed 66s); a 2.6MB file exceeded 120s on a single write-decision. `why`/`lint` over the same file inherit the same extraction cost.

Net: the green scale-smoke run feeds neither bottleneck (tiny files, one decision per .dlog, 256KB corpus cap, seeded artifacts bypass the CLI), while both bottlenecks are demonstrably real in the shipped binary.

**Verifier notes / severity correction:** Claim is accurate on every specific: line numbers, the decisionsPerFile=1 default, renderSource feeding tiny files, the 256KB corpus skip, and the seeded path bypassing the CLI write path. Both quadratic behaviors reproduce. Two refinements: (a) the claimed "86s for 1200 decisions in one .dlog" is in the right ballpark — I measured ~106s total, but that includes per-write source fingerprinting of a 1300-line file; the dlog-parse component alone (parse+reserialize whole growing YAML each write) is the super-linear term and 86s is plausible for a smaller source. (b) The CLAIM's framing (harness blindness = operational/medium) is correct as scoped, but note the underlying defects it conceals are themselves higher-severity performance bugs: a single ~1.3MB source file or a decision-dense file causes minute-plus CLI stalls / effective hangs (2.6MB exceeded 120s), which for an agent-invoked tool on real repositories is closer to high severity in its own right. The release-readiness signal from the green scale-smoke is misleading for the scalability dimension, exactly as claimed.

**Recommended resolution:** Add scale scenarios that (1) include at least one multi-MB source file (and a minified one-line file) to exercise extract_anchors at size, and (2) write many decisions into a single .dlog via the actual write-decision CLI to measure the cumulative write curve. Assert on per-operation latency growth, not just correctness/parity, so a quadratic regression fails CI.

---

### F68. [LOW] post-tool-use on an undecided/new file does O(decisions) source stat() calls via moved_dlog_candidate

`operational` · location `src/core/project.rs:346-375 (moved_dlog_candidate), called from post_tool_use:236 when the edited file has no existing .dlog` · reporter-confidence medium · verification **CONFIRMED**

**Description:** When post-tool-use runs on a file that has no .dlog yet (a new or untracked file), and anchor extraction is complete, moved_dlog_candidate lists every .dlog in the repo and, for each, calls read_source_text_if_exists (a canonicalize + existence check) to find a deleted-source rename candidate, loading the candidate .dlog only when its source is gone. This is O(total decision files) filesystem probes on every edit of any file that lacks decisions.

**Why it matters:** post-tool-use is intended to run on every file edit (it is a hook). On a large repo, editing any of the (vast majority) files without decisions triggers a full sweep of the .decisions tree.

**Impact:** Bounded by stat() cost (fast on warm cache — measured 0.02s at 2000 dlogs), but grows linearly with repo decision count and runs on the hot edit path. On cold cache / network filesystems this becomes noticeable.

**Likelihood:** medium — fires on every edit of an undecided file, which is most files, but only when no .dlog exists for that path.

**Evidence (reporter):** post_tool_use (project.rs:227-257): the `else` branch (no current dlog) calls moved_dlog_candidate, which iterates list_dlog_files (project.rs:357) and probes each source (project.rs:359). Measured fast at 2000 dlogs (0.02s) because the early `continue` on existing source avoids dlog parse, but the directory listing + per-entry stat is O(decision files) every time.

**Independent verification:** Read src/core/project.rs:223-375. post_tool_use:227 takes the `else` branch only when `!has_current_dlog` (src/core/project.rs:224); a file WITH a .dlog skips moved_dlog_candidate entirely. moved_dlog_candidate (src/core/project.rs:346-375) returns early unless extraction.complete (line 352), then iterates list_dlog_files (line 357 -> list_storage_files walks .decisions, src/core/fs.rs:631) and for each dlog calls read_source_text_if_exists (line 359) which is canonical_source_path_if_exists (canonicalize + try_exists, src/core/paths.rs:142-167) THEN File::open + full content read (read_text_if_exists_with_limit, src/core/fs.rs:59-69). The `continue` at line 359 skips load_dlog only when the candidate source still exists.

Empirical reproduction with /home/ubuntu/archaeo/target/release/archiva in /tmp/archiva_perf (git init + archiva init, then N decisions one per file, then `hooks post-tool-use` on an undecided new file src/undecided.ts). strace -c stat-family + openat counts scale linearly with dlog count:
  100 dlogs:  104 statx, 107 openat
  1000 dlogs: 1004 statx, 1007 openat
  2000 dlogs: 2004 statx, 2007 openat
(~1 statx + 1 openat per dlog, i.e. O(total decision files)). Decided-file control: post-tool-use on src/file_1.ts (which has a .dlog) at 2000 dlogs = only 9 statx total — confirms the walk is bypassed when a current dlog exists.

Early-continue confirmed: deleting 5 candidate sources raised openat by exactly +5 (2007->2012) — each existing source skips the dlog parse, each missing source adds one dlog open.

Timing: warm cache ~0.01-0.02s at 2000 small dlogs (matches self-report's 0.02s). Note read cost: 6008 read syscalls at 2000 dlogs. With ~170KB candidate sources, time rose to ~0.08s — cost scales with total candidate source BYTES, not just file count.

**Verifier notes / severity correction:** Core claim CONFIRMED: post-tool-use on a dlog-less file performs O(total decision files) filesystem operations on the hot edit path, guarded only by extraction.complete, and bypassed when the file already has a .dlog. Linearity proven across 100/1000/2000 dlogs.

Two corrections to the claim, both making it slightly worse than described but not changing severity:
1. The probe is NOT just "stat() / canonicalize + existence check." For every candidate whose source still exists, read_source_text_if_exists does a full File::open + content read (src/core/fs.rs:64-65). So cost is O(decisions) opens PLUS reading the full byte content of every still-present decided source on every edit of an undecided file. Demonstrated: ~170KB sources push the time from 0.01s to 0.08s at 2000 dlogs. The cost scales with total decided-source bytes, not merely decision count — relevant on large repos with big files.
2. The trigger is broader than "new/untracked file." ANY edited file lacking a .dlog whose anchor extraction is `complete` triggers the walk — including a plain-text file (notes.txt) that tokenizes cleanly to zero anchors (confirmed: 104 statx at 100 dlogs). The claim's framing of 'new/untracked source file' undersells how often this path runs.

Severity low is correct: bounded, fast on warm cache, no correctness impact, and only on the undecided-file branch. It is a genuine operational/scalability weakness on cold caches, network filesystems, or repos with many large decided files, on a hot per-edit path. Recommended resolution: skip moved_dlog_candidate unless a deleted-source rename is actually plausible (e.g. gate on a cheap pre-filter — a dmap-level fingerprint/anchor-name index rather than reading every source), or cache the dlog->source-existence map across the session. Confidence: high.

**Recommended resolution:** Short-circuit moved_dlog_candidate when the edited file is brand new and the repo has no recently-deleted sources, or maintain a small index of decision-files-whose-source-is-missing rather than re-probing every decision file on each edit.

---

### F69. [LOW] No upper bound on per-file extraction work despite a 128MB source byte limit

`operational` · location `src/core/fs.rs:17 (SOURCE_FILE_MAX_BYTES = 128MB); src/core/anchor.rs:241 (extract_anchors has no token/line guard)` · reporter-confidence high · verification **CONFIRMED**

**Description:** read_source_text reads files up to 128MB, and extract_anchors then runs its O(n^2) passes with no token-count or line-count ceiling and no time budget. A single accepted file can therefore dominate or hang an entire repo-wide lint/status with no diagnostic to the user.

**Why it matters:** Combined with finding #1, the absence of a guard turns a single adversarial or generated file into a denial-of-service for the whole command. Even a legitimate large generated file (protobuf output, bundled JS, amalgamated C) trips it.

**Impact:** Unbounded CPU/time on one file; the command appears hung rather than reporting that a file was too large to analyze.

**Likelihood:** medium — large generated files exist in many real repos.

**Evidence (reporter):** fs.rs:17 SOURCE_FILE_MAX_BYTES 128*1024*1024 with the comment-free constant; grep for token/line caps in anchor.rs before extraction found only the TSX token_limit (a correctness limit, not a size guard). Empirically a 1.3MB file already needs 66s.

**Independent verification:** Code: src/core/fs.rs:17 `pub const SOURCE_FILE_MAX_BYTES: usize = 128 * 1024 * 1024;` — read_source_text/read_source_text_if_exists (project.rs:622-630) read source up to 128MB. extract_anchors (anchor.rs:241) has no line/token/time ceiling; the only token cap is the TSX `token_limit` (anchor.rs:255-281), a correctness recovery limit, not a size guard. The cost driver is genuinely super-linear: `is_top_level` (anchor.rs, ~line just below 2228) scans `tokens[..index]` on every call, and it is called inside `0..tokens.len()` loops in collect_function_anchors (2237), collect_class_anchors (2320), collect_variable_anchors (3025/3075/3110/3212), collect_*_export (3688/3737/3815) — classic O(n^2).

Empirical (binary /home/ubuntu/archaeo/target/release/archiva, scratch /tmp/archiva_perf with `archiva init`):
- 1.27MB JS (20000 top-level functions): `archiva lint` = 17.28s user / 17.35s wall, 99% CPU, RSS 76MB. Completely silent for the whole 17s, then "No decision issues found."
- 2.55MB JS (40000 functions, 2x size): 90.00s user / 1:30.17 wall, RSS 151MB. 2x input -> ~5.3x time, confirming super-linear (O(n^2)-class) scaling, not merely the read.

Extrapolation: a file near the 128MB ceiling is ~100x larger than the 1.27MB sample; under the observed ~n^2.4 scaling this is hours-to-days of CPU on one file with zero progress output — indistinguishable from a hang. lint walks every source file (list_lint_source_files), so one pathological accepted file dominates a repo-wide lint/status; write-decision/assert_anchor_exists on that file does the same. No diagnostic tells the user a file was too large to analyze.

The claim's specific "66s for 1.3MB" I measured as 17s for a function-dense 1.27MB file; denser/deeper real code reaches that range, and the mechanism and impact hold regardless of the exact constant.

**Verifier notes / severity correction:** Claim is accurate on every load-bearing point: 128MB read ceiling, no token/line/time guard in extract_anchors, O(n^2) passes, single file can dominate lint with no diagnostic and appears hung. One refinement: the dominant cost is the O(n^2) tokenizer/matching passes (esp. is_top_level rescanning the prefix), not the 128MB read by itself — so even files well under 128MB (a few MB of dense code) already cost tens of seconds. Severity low is defensible: triggering requires a multi-MB single source file, and the common bulk dirs (node_modules/dist/build/coverage) are skipped (fs.rs SKIPPED_WALK_DIRS), so this is an edge/accidental-large-generated-file or adversarial scenario rather than a routine one. It is arguably medium given the total silence and hours-scale worst case at the allowed ceiling. Recommended resolution: add a pre-extraction guard (line and/or token count cap, e.g. emit a "file too large to analyze" AnchorDiagnostic and skip) well below 128MB, and/or replace the per-token prefix rescans in is_top_level and the collect_* loops with a single precomputed depth array to make extraction O(n).

**Recommended resolution:** Introduce a separate, much smaller analysis ceiling (e.g. cap by token count or a few hundred KB) above which extract_anchors short-circuits to an incomplete/parser diagnostic, decoupled from the 128MB storage read limit. This bounds worst-case per-file time even before the O(n^2) is fixed.

---

## DIMENSION: Reliability, Failure Recovery, Observability  — score 7/10

> Archiva v2 stores decisions repo-locally as authoritative `.dlog` (YAML) plus a derivative `.dmap` index, mutated under per-file locks with atomic temp-sibling+fsync+rename writes. Crash consistency is genuinely strong: I verified by fault injection that the `.dmap` is never read by any production command (only by tests) — every command derives its answer from the `.dlog`, so a stale, missing, or corrupt `.dmap` can never produce a wrong answer and is lazily repaired by status/lint/session-start. Atomic writes mean an interrupted or SIGKILL'd write leaves either the old or new complete file, never a torn one. Locking is robust: live-PID locks block, stale locks (dead PID or expired timestamp) recover, and 8 concurrent writers serialized correctly with sequential IDs and no leftover locks. Where it is weak is the operator-facing surface: there is no logging/verbose/diagnostic mode at all, parse and storage errors carry no file-path or line context (so on a multi-file scan the operator cannot tell which file is bad), one corrupt `.dlog` aborts an entire repo-wide command instead of being skipped, and the exit-code scheme conflates "lint found issues" with "the command itself failed." For a tool whose whole job is to be a silent always-on hook in agent workflows, the post-release diagnosability is the real risk, not data integrity.

*Score rationale:* The data-integrity and crash-consistency engineering is excellent and verified: atomic temp+fsync+rename writes, a purely-derivative .dmap that no production command reads (so it can never yield a wrong answer and is lazily self-repaired), corrupt .dlog files preserved rather than clobbered, robust PID-aware stale-lock recovery, correct serialization of concurrent writers, and a resilient MCP loop. Those fundamentals are release-grade. The score is held back by the operator-facing reliability surface, which is weak for an unattended hook tool: zero logging/diagnostics, parse/storage errors with no file-path or line context, repo-wide commands that abort entirely on one corrupt file rather than isolating it, an exit-code scheme that conflates findings with failures, and non-atomic multi-file lint --fix with no progress record. None of these threaten stored data, but together they would make field incidents genuinely hard to diagnose. Fixing the error-context and per-file-isolation findings (both small, localized changes) plus adding an env-gated diagnostic channel would move this to a 9.

**Verified behaviors (checked, not assumed):**

- Crash consistency: read src/core/fs.rs atomic_write_bytes_impl (temp sibling -> write -> sync_all -> rename -> parent dir fsync) and its child-process SIGABRT tests; an interrupted write leaves old-or-new complete file, never torn.
- Verified .dmap is NEVER read in production: `grep -rn 'load_dmap('` shows callers only in src/core/storage.rs tests. All why/history/status/session-start/lint paths read the .dlog. Confirmed empirically: corrupted src/b.ts.dmap to garbage, `why src/b.ts` still returned correct output from the .dlog (no repair, no wrong answer).
- .dmap auto-repair works for scanning commands: deleted/garbled .dmap was regenerated to '1-3:fn:pay' after running `status` and `session-start`.
- Stale-derivative-after-crash recovery: storage.rs test next_locked_write_regenerates_dmap_after_crash_left_stale_derivative confirms next locked write rewrites the .dmap from the .dlog.
- Lock correctness: live-PID lock (used my shell PID) blocked write for ~1.0s then errored with full path; dead-PID+old-timestamp lock was recovered and write succeeded (dec_002), no leftover .lock; dead-PID+fresh-timestamp correctly timed out at 1.0s rather than breaking a possibly-live lock.
- Concurrency: 8 parallel `write-decision` to same file/anchor all succeeded with sequential dec_001..dec_008, final .dlog valid, zero leftover .lock files.
- Corrupt .dlog is preserved, not overwritten: write-decision against a corrupt store failed with the parse error, left the corrupt bytes unchanged, wrote no .dmap, left no lock (storage.rs test + CLI test malformed_dlog_fails_without_rewriting_or_leaving_locks).
- Single corrupt .dlog aborts whole multi-file command: with src/b.ts.dlog set to 'schema: nope', `status`, `lint`, and `hooks session-start` ALL exited 1 with 'schema: expected integer' and produced no output for the healthy src/a.ts; `why src/a.ts` (single-file) still worked.
- lint --fix partial completion: 3 stale files a/m/z, corrupted middle file m's .dlog; `lint --fix` marked src/a.ts STALE (1), aborted on m with 'file: missing required field', never reached src/z.ts (0 STALE). Earlier files mutated, later files untouched, no file named in error.
- Error messages lack file context: parse/schema errors render via ArchivaError::user_message with no path; confirmed 'schema: expected integer' and 'file: missing required field' across status/lint/session-start/MCP with no filename.
- YAML errors discard captured line numbers: YamlError carries line() (error.rs From<YamlError> sets line) but user_message only clones message; injected a malformed mapping on line 8 and got 'Unexpected indentation in mapping' with no line.
- No observability: grep for verbose/--debug/log/RUST_LOG/tracing/env_logger found none; only exit codes 0/1 via process::exit in main.rs.
- Exit-code conflation: `lint` exits 1 both when it finds real issues and when the command crashes on a corrupt dlog; `status` exits 0 even when issues are present. Differentiator is only stdout vs stderr.
- MCP resilience: drove `archiva mcp` over stdio; a why() call against a corrupt .dlog returned a JSON-RPC error (code -32000) and the server kept serving the next request successfully (server-exit=0).
- Read-only .decisions: `chmod -R a-w .decisions` then write-decision gave a clear actionable error with full path: 'Failed to create lock file /tmp/aa2/.decisions/src/b.ts.lock: Permission denied (os error 13)'.
- Init idempotency: re-running `init` twice returned 'Archiva initialized.' exit 0 each time; init.rs merges settings/AGENTS.md/.gitignore without clobbering existing content.

### F70. [HIGH] Storage and parse errors carry no file-path context, making corrupt files unlocatable on repo-wide commands

`defect` · location `src/core/error.rs:88-110 (user_message); src/core/storage.rs:26-33,131-135 (load_dlog/write_dlog); src/core/project.rs:69-91 (session_start/status)` · reporter-confidence high · verification **CONFIRMED**

**Description:** When the .dlog parser or schema validator fails, the resulting ArchivaError::Yaml/Schema/Json renders only the bare message (e.g. 'schema: expected integer', 'file: missing required field', 'Unexpected indentation in mapping') with no indication of which file produced it. The scanning commands status, lint, and session-start iterate every .dlog in the repo and propagate the first error verbatim. The operator is told a file is broken but not which one, in a repo that may have hundreds of .dlog files.

**Why it matters:** Diagnosability is the core of release readiness for a tool that runs unattended as an agent hook. A field operator who sees 'schema: expected integer' from `archiva status` has no path, no line, no way to find the offending file short of manually grepping .decisions. The .dlog can be hand-edited or merged via git, so corruption in the field is plausible.

**Impact:** Operators cannot triage a corrupt store without manual filesystem archaeology. Support burden and mean-time-to-recovery rise sharply.

**Likelihood:** High once any .dlog is corrupted (bad merge, partial external edit, disk issue).

**Evidence (reporter):** Set /tmp/aa2/.decisions/src/b.ts.dlog to 'schema: nope'; `archiva status`, `archiva lint`, `archiva hooks session-start` all printed exactly 'schema: expected integer' to stderr, exit 1, with no filename. `grep -rn 'with_file_context|\.dlog:' src/core/*.rs` returns nothing — no code path ever attaches the path to a parse error.

**Independent verification:** CODE: error.rs:99-101 — Json/Yaml render `message.clone()`, Schema renders `field: message`; none of the Json/Yaml/Schema enum variants carry a PathBuf (error.rs:25-43). storage.rs:30 — `load_dlog` does `parse_dlog_yaml(&content)?`, propagating the bare error with no path. project.rs:76-82 (session_start), 87-91 (status→lint_project_issue_count), 133-135 (lint_project_inner) each iterate every .dlog and propagate the first load_dlog error via `?`. main.rs:11/30/41 surface only `error.user_message()`; no path added at the boundary. `grep -rn 'with_file_context' /home/ubuntu/archaeo/src/core/*.rs` → exit 1 (absent), matching the claimed grep result.

RUNTIME (binary /home/ubuntu/archaeo/target/release/archiva in /tmp/aa2; two valid decisions written via stdin JSON for src/a.ts and src/b.ts, then b.ts.dlog corrupted): `status` printed `schema: expected integer` (when schema:notint) and `file: missing required field` (when `file:` key absent); `lint` and `hooks session-start` printed the identical bare message; all exit 1; no filename in any output. Reproduced the claim's exact cited messages (`schema: expected integer`).

BEYOND CLAIM: (1) With BOTH a.ts.dlog and b.ts.dlog corrupted, output was `decisions.fn:a.lines_hint: missing required field` — names neither file, and since list_storage_files does `output.sort()` (fs.rs:634) the operator only ever sees the alphabetically-first corrupt file with no signal that others are broken. (2) The `file: missing required field` message uses the dlog schema field name `file`, which an operator can misread as 'a file is missing a field' — actively misleading.

**Verifier notes / severity correction:** Claim is accurate in mechanism, evidence, and impact. Minor corrections/refinements: (a) the claim's example message 'schema: nope' actually renders as 'file: missing required field' for a bare 'schema: nope' file because the missing `file:` key fails first; the integer message 'schema: expected integer' reproduces with a present-but-non-integer schema — both confirm the no-path defect. (b) Two aggravations strengthen it: with multiple corrupt files only the sort-first one is reported (no count, no 'and N others'), and the literal text 'file: ...' is itself misleading because 'file' is a schema field name, not a path. Severity high is defensible for a repo-wide-scan tool, though it sits near the high/medium line since the store is plain text and grep-able for manual triage. Recommended fix: wrap parse/schema/io errors at the load_dlog / scan-loop boundary (storage.rs:26-33, project.rs:76/133) with the source path — e.g. add an optional `path` to the error variants or introduce a `with_path(error, &dlog_path)` adapter applied in load_dlog and in each scanning loop, and print 'src/b.ts.dlog: <message>'. Consider continuing the scan to collect/report all corrupt files rather than aborting on the first.

**Recommended resolution:** Wrap load_dlog / parse errors with the file path at the call sites in session_start, status, lint_project_inner, and load_project_status_summaries (e.g. map_err to prefix `{path}: {message}`). Cheap, localized change; the path is already in scope as `file`/`dlog_file`.

---

### F71. [HIGH] A single corrupt .dlog aborts the entire repo-wide command instead of being skipped and reported

`architecture` · location `src/core/project.rs:76-82 (session_start loop), :402-408 (status loop), :133-149 (lint_project_inner loop)` · reporter-confidence high · verification **CONFIRMED**

**Description:** session_start, status, and lint all loop over every .dlog and use the `?` operator on load_dlog, so the first unparseable file terminates the whole command with zero output. There is no per-file error isolation: one bad file makes the entire repo's decision health invisible, including all the healthy files.

**Why it matters:** These three commands are the primary read surface (session-start is injected into every agent session via the installed hook). One corrupt file silently disables decision context for the whole repo, degrading the agent's behavior repo-wide rather than for one file.

**Impact:** Total loss of decision visibility from a localized corruption. The hook that is supposed to inform the agent instead emits an error and nothing else.

**Likelihood:** Medium — requires one corrupt .dlog, which the codebase itself treats as a real scenario (multiple dedicated tests exist).

**Evidence (reporter):** With src/b.ts.dlog corrupted, `archiva status` produced no row for the healthy src/a.ts at all (exit 1, only the error). Code: project.rs:78 `if let Some(dlog) = load_dlog(...)?` — the `?` propagates and ends the loop.

**Independent verification:** Code read (exact paths/lines match claim):
- src/core/storage.rs:26-32 `load_dlog` calls `parse_dlog_yaml(&content)?` and propagates Err on a corrupt file (it does NOT swallow parse errors into Ok(None) — Ok(None) is only for a missing file at line 27-28).
- src/core/project.rs:78 (session_start) `if let Some(dlog) = load_dlog(project_root, &file)?` — the trailing `?` propagates the parse Err and terminates the for-loop (lines 76-82).
- src/core/project.rs:404 (status via load_project_status_summaries, lines 402-408) — identical `load_dlog(...)?` inside the loop.
- src/core/project.rs:133-149 (lint_project_inner) — loops and calls lint_dlog_locked, which at src/core/project.rs:421 does `load_dlog(project_root, file)?` inside the lock closure; the Err propagates out and aborts the loop at line 135.
None of the three loops has a per-file catch/continue on a load error.

Runtime reproduction with /home/ubuntu/archaeo/target/release/archiva in /tmp/archtest (git-init project, two healthy decisions on src/a.ts and src/b.ts):
- BASELINE (both healthy): `status` listed both rows (src/a.ts, src/b.ts) exit 0; `hooks session-start` printed the decision map for 2 files exit 0; `lint` -> "No decision issues found." exit 0.
- Corrupted only .decisions/src/b.ts.dlog with invalid YAML:
  status  -> stdout/stderr just "Unterminated flow collection", exit 1, ZERO row for the healthy src/a.ts.
  hooks session-start -> "Unterminated flow collection", exit 1, no map emitted at all.
  lint -> "Unterminated flow collection", exit 1, no issues reported for any file.
- Order-independence check: corrupted .decisions/src/a.ts.dlog (alphabetically first) instead -> status again exits 1 with only "Unterminated flow collection" and no row for the healthy src/b.ts.

Additional observability defect observed: the abort error ("Unterminated flow collection") does not name the offending file, so the operator/agent cannot tell which .dlog is corrupt.

Single-file commands (why/history on one path) are correctly scoped — they only load that one file — so the blast radius is limited to the repo-wide iteration commands (status, session-start, lint), exactly as claimed.

**Verifier notes / severity correction:** Claim is accurate in every particular: locations, the `?`-propagation mechanism, and the user-visible effect (total loss of decision visibility from one localized corruption; the session-start hook meant to inform the agent emits only an error). Severity high is appropriate: the affected surfaces include the agent-facing hook and the human-facing status/lint, all of which become fully blind to every healthy file. One nuance to add to the writeup: this is partly an observability failure too — the abort message does not identify which .dlog is corrupt, so recovery requires manually bisecting .decisions/. Recommended resolution: isolate per-file loads in the three loops (project.rs:76-82, :402-408, :133-149) — convert the `?` into a match that, on load/parse error, records a per-file diagnostic (e.g. a synthetic parser/corruption issue or a status row flagged "unreadable" naming the file+error) and `continue`s, so healthy files still render. Lint already has precedent for surfacing a Parser LintIssue (project.rs:430), so a Corruption issue fits the existing model.

**Recommended resolution:** In the scanning loops, collect per-file errors instead of bailing: skip the corrupt file, emit a diagnostic line naming it (e.g. a synthetic Parser-style lint issue or a stderr warning), and continue reporting healthy files. Keep exit non-zero so CI still notices.

---

### F72. [HIGH] No logging, verbose, or diagnostic mode anywhere — the tool is a black box in the field

`operational` · location `whole crate (src/main.rs, src/cli.rs); verified absence via grep` · reporter-confidence high · verification **CONFIRMED**

**Description:** There is no --verbose/--debug flag, no RUST_LOG/env-gated tracing, no structured diagnostics, and no way to see what files were scanned, which locks were contended, whether a stale lock was recovered, or which .dmap was repaired. The only output is the command's normal result plus a single-line error on failure.

**Why it matters:** Archiva runs as an unattended hook (session-start / post-tool-use) inside agent toolchains. When something misbehaves in the field — a lock not recovering, a re-anchor doing nothing, a git HEAD read silently falling back — there is no lever to make the tool explain itself. Reproduction depends on guessing.

**Impact:** Field issues are very hard to diagnose; the maintainer must add instrumentation and ship a new build to investigate any non-crash misbehavior.

**Likelihood:** High that some field issue will be hard to reproduce; this is the dominant operational risk for this release.

**Evidence (reporter):** `grep -rn 'verbose|--debug|log::|RUST_LOG|tracing|env_logger' src/` (excluding the dlog module-path noise) returns no logging facility. Exit handling in main.rs only ever writes the user_message and exits 0/1.

**Independent verification:** Code: Cargo.toml has an empty [dependencies] section — no log/tracing/env_logger crate. `grep -rE '\blog::|\btracing::|debug!\(|info!\(|env_logger'` over src/ (excluding tests) returns nothing. The only non-test stderr writes are in src/main.rs lines 11/21/30/41, all of which emit either error.user_message() or result.stderr (the command's own one-line error). main.rs only ever calls process::exit(0) or process::exit(1) — no diagnostic verbosity path. The only runtime env vars read in production code are ARCHIVA_FILE (cli.rs:249) and ARCHIVA_SESSION (project.rs:194); ARCHIVA_ATOMIC_WRITE_CHILD_* (fs.rs) exist only to drive a test subprocess. There is NO RUST_LOG, --verbose, --debug, --quiet, or any diagnostic env gate.

Runtime (binary /home/ubuntu/archaeo/target/release/archiva in /tmp/audit-log, real git repo):
- `archiva init --verbose` -> "error: unknown option '--verbose'", exit 1.
- `archiva --debug status` -> "error: unknown option '--debug'", exit 1.
- `RUST_LOG=debug archiva status` -> identical normal output, no extra diagnostics.
- `archiva --help` grepped for verbose|debug|log|diagnos|quiet -> NONE.

Silent internal recovery confirmed (the core of the operational concern):
1. dmap repair: wrote dec_001 against src/a.rs (anchor fn:foo), then overwrote .decisions/src/a.rs.dmap with "TOTALLY-BROKEN-GARBAGE-99999". Running `archiva status` produced the normal table (1 decisions 0 stale 0 orphan), exit 0, ZERO mention of the corruption, and silently rewrote the dmap back to "1-4:fn:foo". A maintainer would never know the derivative index was corrupt.
2. stale-lock recovery: planted .decisions/src/a.rs.dlog.lock with timestamp 2020-01-01. The subsequent write-decision (supersede dec_001) printed only "Recorded dec_002.", exit 0 — the stale lock from a presumed crashed prior process was recovered and overwritten with no diagnostic whatsoever (recover_stale_lock in fs.rs:344 returns a bool and emits nothing).

Both recovery paths are exercised by tests in src/core/storage.rs (recovers_expired_lock..., load_dmap_repairs_...) but neither surfaces any observable signal to an operator at runtime.

**Verifier notes / severity correction:** Claim is accurate in full: no --verbose/--debug, no RUST_LOG/tracing/env_logger, no structured diagnostics; main.rs only emits the command result + a single-line error and exits 0/1. I independently demonstrated the two most consequential silent events the claim calls out: a corrupted .dmap is silently repaired and a stale lock is silently recovered, both with no operator-visible signal. The claim that "the maintainer must add instrumentation and ship a new build to investigate any non-crash misbehavior" is correct — there is no env-gated path to enable diagnostics without recompiling.

Scope correction / fairness: the claimed location "verified absence via grep" is right, but the missing diagnostics matter most precisely because the tool performs *automatic, lossy-looking recovery* (dmap repair, stale-lock takeover, and — worth a follow-up — anchor/file scanning that can skip files). Those are exactly the cases where silence hides real problems, so the high severity is justified for a 'decision memory' tool where silent derivative-index corruption could mask deeper dlog issues. A reasonable reviewer could argue medium given: it is a repo-local, per-invocation CLI (not a long-running service), recovery is correct and test-covered, and the .dlog (authoritative) is YAML the operator can always read by hand. Recommended resolution: add an env-gated diagnostic channel (e.g. ARCHIVA_DEBUG / RUST_LOG-style) that logs to stderr — at minimum: which files were scanned/skipped, lock contention + stale-lock recovery, and dmap repair events with the reason (missing/stale/corrupt/oversized) — without requiring a new build to enable.

**Recommended resolution:** Add a lightweight ARCHIVA_LOG/-v env-gated stderr diagnostic channel: log files scanned, lock acquire/recover/release events, .dmap repairs, and git-HEAD fallback. std-only; a guarded eprintln helper suffices. This is the single highest-leverage reliability improvement.

---

### F73. [MEDIUM] Exit codes conflate 'lint found issues' with 'command failed', and status ignores issues

`operational` · location `src/cli.rs:358-382 (run_lint), :40-49 (run_cli); src/main.rs:43` · reporter-confidence high · verification **CONFIRMED**

**Description:** There is no exit-code taxonomy. `lint` returns 1 both when it successfully finds lint issues and when it crashes on a corrupt .dlog; the only differentiator is whether output went to stdout (issues) or stderr (failure). `status` returns 0 even when it reports outstanding stale/orphan/issue counts. Every other failure is also exit 1.

**Why it matters:** CI and automation cannot distinguish 'lint ran and found problems' (actionable, expected) from 'lint itself broke' (infrastructure failure) by exit code alone — they must scrape stderr. status being always-0 means it cannot be used as a gate.

**Impact:** Brittle CI integration; scripts must parse text streams rather than rely on exit codes. Misclassification of tool failure as 'just lint findings'.

**Likelihood:** Medium — surfaces as soon as anyone wires these commands into CI.

**Evidence (reporter):** `lint` with a real stale finding exited 1; `lint` with a corrupted dlog also exited 1 (only stderr vs stdout differed). `status` with a stale decision present exited 0.

**Independent verification:** Read src/cli.rs and src/main.rs and reproduced with /home/ubuntu/archaeo/target/release/archiva in /tmp/arc-audit.

Code paths (no exit-code taxonomy; only 0/1 exist):
- CliResult::err() hardcodes `status: 1` (src/cli.rs:32-37). Every error path in run_cli_result -> CliResult::err collapses to 1 (cli.rs:45-48).
- run_lint computes `status = if has_error_issue(&issues) { 1 } else { 0 }` (cli.rs:380); has_error_issue returns true only for LintSeverity::Error (src/core/lint.rs:96-100). Findings go to stdout; crashes go to CliResult::err -> stderr + status 1 (cli.rs:351-355).
- run_status returns Ok(...) on success regardless of counts (cli.rs:329-348); status() embeds issue_count purely as text (src/core/project.rs:87-91).
- main.rs:43 `process::exit(result.status)` propagates this 0/1 only.

Reproduction (same binary):
1) Stale ERROR finding: after fingerprint mismatch, `lint` -> stdout "ERROR arc/stale ...", exit 1.
2) Corrupt .dlog: `lint` -> stdout EMPTY, stderr "Unterminated flow collection", exit 1. Identical exit code to (1); only stdout-vs-stderr distinguishes finding from crash.
3) `status` with 1 stale + "2 issues" printed -> exit 0.
4) `status` on corrupt .dlog -> exit 1 (stderr).

Refinement to the claim: lint returns 1 only for ERROR-severity findings (e.g. arc/stale). A WARNING-severity finding (arc/orphan, source deleted) prints to stdout but exits 0 (reproduced: "WARNING arc/orphan ..." with exit 0). So lint already distinguishes warning(0) from error(1) internally, but it does NOT distinguish error-finding(1) from command-crash(1) other than by output stream.

**Verifier notes / severity correction:** Claim is substantially correct and the medium/operational rating is appropriate. Two corrections of scope: (1) lint does not return 1 for "lint issues" in general — it returns 1 only for Error-severity issues (stale); Warning-severity issues (orphan) print to stdout but exit 0. So the genuine conflation is specifically error-severity-finding(1) vs command-crash(1), differentiable only by stdout vs stderr — exactly as the claim's evidence shows. (2) status conflation is fully confirmed: exit 0 even with nonzero stale/orphan/issue counts; status only flips to 1 on an actual command failure (corrupt dlog). Net CI impact is real: a script cannot use `archiva lint`'s exit code alone to tell "code drifted from a recorded decision" apart from "the decision store is corrupt," and `archiva status` cannot be used as a gate at all (always 0 unless it crashes). Recommended resolution: introduce a small exit-code taxonomy (e.g. 0=clean, 1=findings present, 2=usage/parse/IO error) wired through CliResult instead of the hardcoded 1, and have status return a nonzero "findings present" code (or add a --strict/--exit-code flag) when stale/orphan/issue counts are nonzero. Side observation, out of scope but worth a separate ticket: in the orphan repro `status` printed "0 orphan" in both the per-file and total columns while simultaneously reporting "1 issues", so the orphan summary counter and the lint issue counter disagree.

**Recommended resolution:** Define distinct exit codes: 0 success/clean, 1 lint findings present, 2 (or >=64) operational/parse failure. Document the taxonomy. At minimum, separate command-failure from rule-violation so automation can branch on it.

---

### F74. [MEDIUM] lint --fix is not atomic across files; a mid-iteration failure leaves a partially-fixed repo with no summary

`operational` · location `src/core/project.rs:123-157 (lint_project_inner), :412-428 (lint_dlog_locked), :544-547 (per-file write_dlog/write_dmap)` · reporter-confidence high · verification **CONFIRMED**

**Description:** lint --fix mutates each file under its own lock as the loop progresses (mark stale, remove orphans, write dlog+dmap). If a later file in the iteration is corrupt or otherwise errors, the command aborts with `?`. Files processed earlier are already mutated and persisted; files after the failure are untouched. The aborted run prints only the error, not a record of what it did change.

**Why it matters:** The operator re-runs after the abort and sees different state than before, with no log of the partial mutation. Each file's write is individually atomic and crash-safe, but the multi-file operation has no transactionality or progress report, so partial application is silent.

**Impact:** Surprising partial state after a failed --fix; combined with the no-logging finding, the operator cannot tell which files were already fixed. Re-running does converge (fixes are idempotent), so this is recoverability-degrading, not data-destroying.

**Likelihood:** Medium — needs one problematic file among several being fixed.

**Evidence (reporter):** 3 stale files a/m/z, middle file m corrupted. `lint --fix` set src/a.ts to STALE, aborted on m ('file: missing required field'), left src/z.ts untouched (0 STALE). Verified via grep of the resulting .dlog files.

**Independent verification:** Reproduced end-to-end with the release binary in /tmp/linttest.

Setup: `archiva init`; three TS sources a.ts/m.ts/z.ts each with a recorded decision (write-decision --json). Modified all three sources so every fingerprint goes stale (return 999). Then corrupted ONLY the middle file's dlog by deleting the required `id:` field from .decisions/m.ts.dlog.

Run: `archiva lint --fix` →
  stdout: `decisions.fn:m.id: missing required field`
  exit code: 1

Per-file state AFTER the aborted run (grep status: in each dlog):
  a.ts → `status: STALE` (+ `stale_since:` written) — MUTATED and persisted
  m.ts → no status field — untouched (parse failed before lint logic)
  z.ts → no status field — untouched (loop never reached it)

This exactly matches the claim's partial-state pattern: a was fixed, m errored, z was skipped.

Code path confirms the mechanism:
- src/core/fs.rs:631-636 list_storage_files sorts output (`output.sort()`), so iteration order is deterministic a→m→z; the corrupt file is genuinely in the middle.
- src/core/project.rs:133-149 lint_project_inner loops files; for each it calls lint_dlog_locked, which propagates errors with `?` (line 135 `let Some(...) = lint_dlog_locked(...)? else`). load_dlog on m.ts fails the required-field check and the whole command aborts.
- src/core/project.rs:412-428 lint_dlog_locked acquires a per-file lock and writes inside that lock; src/core/project.rs:544-547 write_dlog+write_dmap run per file as soon as `changed` is true. So a.ts is durably written before m.ts is ever loaded. There is no cross-file transaction or rollback.
- src/cli.rs:351-355 + 379-381: on Err, run_lint_cli returns CliResult::err(error.user_message()), so only the raw error string is printed. format_lint_issues (src/core/lint.rs:56-75) never runs on the abort path, and it has no "fixed/applied/changed" summary even on success — output is purely the list of remaining issues. The operator gets no record of what --fix already mutated.

Convergence on re-run confirmed: restored a valid m.ts.dlog and re-ran `archiva lint --fix`; it then reported all three files stale (arc/stale a/m/z) and all three dlogs ended with `status: STALE`. So the fixes are idempotent and re-running converges — recoverability-degrading, not data-destroying.

Minor wording nit vs. the claim: the actual abort message is `decisions.fn:m.id: missing required field`, not `file: missing required field` — same class of error (missing required field), slightly different field/path. Does not affect the finding.

**Verifier notes / severity correction:** Claim is accurate as written. lint --fix is per-file atomic (each file under its own lock) but NOT atomic across files, and there is no all-or-nothing transaction spanning the iteration. A mid-iteration parse/IO failure leaves earlier files already mutated+persisted, later files untouched, and prints only the error with zero record of what was changed. Confirmed idempotent/convergent on re-run, so impact is recoverability/observability degradation, not corruption — medium is the right severity. Confidence: high.

One refinement to the recommended resolution: the cheapest high-value fix is observability, not full cross-file transactionality. Either (a) emit a per-file progress/summary line as each file is fixed (so an abort still tells the operator which files were already mutated), or (b) accumulate fixes and flush a "fixed N decisions across M files; aborted on <file>: <err>" summary even on the error path (currently the Err path in src/cli.rs:354 discards all of that). Full cross-file atomicity (stage all writes, commit at end) is the stronger but heavier fix and is arguably overkill given fixes are idempotent. The partial state itself is acceptable; the lack of any record of it is the real defect.

**Recommended resolution:** Either (a) collect per-file failures and continue fixing the rest, emitting a final summary of files changed/failed, or (b) at minimum print each file as it is fixed so an aborted run leaves an audit trail. Pair with the per-file error isolation from the earlier finding.

---

### F75. [LOW] YAML parse errors capture a line number but discard it in the user-facing message

`defect` · location `src/core/error.rs:139-147 (From<YamlError>), :100-101 (user_message Yaml arm)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The YamlError type carries a line number, and the From<YamlError> conversion stores it in ArchivaError::Yaml { line, .. }, but user_message renders only `message.clone()` and drops the line. So a structural YAML error in a large .dlog reports e.g. 'Unexpected indentation in mapping' with no position.

**Why it matters:** Combined with the missing file-path context, a YAML error in a hand-edited or merge-conflicted .dlog gives the operator neither file nor line — the two pieces of information most needed to fix it.

**Impact:** Slower manual repair of malformed YAML; the data to pinpoint the error already exists but is thrown away.

**Likelihood:** Medium when .dlog files are edited or merged by hand.

**Evidence (reporter):** Injected a malformed mapping on line 8 of a .dlog; `archiva why` reported 'Unexpected indentation in mapping' with no line. error.rs:100 shows `Self::Yaml { message, .. } => message.clone()` — the `..` discards the captured `line`.

**Independent verification:** Code read (exact): src/core/error.rs:101 `Self::Yaml { message, .. } => message.clone()` — the `..` discards the captured `line`. The From<YamlError> conversion at error.rs:139-145 DOES store the position: `line: error.line()`. The line is genuinely captured by the parser (src/core/yaml.rs:365-372 `error()` helper computes the line from `self.lines[self.index].number` and calls `YamlError::new(message, line)`; struct has `line: usize` at yaml.rs:63). The line is rendered ONLY by YamlError's own Display (yaml.rs:85 `write!(f, "{} at line {}", self.message, self.line)`), but the conversion uses `error.message().to_string()` (error.rs:143), which excludes it; the message string itself carries no position. Top-level rendering uses user_message(): src/main.rs:11 and :30 (`writeln!(stderr, "{}", error.user_message())`) and src/cli.rs:47/:354.

Live reproduction: created /tmp/yamltest, `git init`, `archiva init`, wrote app.js with `function main()`, recorded a valid decision (`{"file":"app.js","anchor":"fn:main","lines":[1,3],"chose":...,"because":...,"rejected":[]}` -> "Recorded dec_001."). Then over-indented the `chose:` line in .decisions/app.js.dlog (captured parser line 10). Running `/home/ubuntu/archaeo/target/release/archiva why app.js fn:main` printed exactly:
  Unexpected indentation in mapping
(exit code 1) — no line number, despite the parser having captured line 10. (I used line 10 rather than the claim's line 8; the structural outcome — message with no position — is identical.)

**Verifier notes / severity correction:** Claim is accurate. Minor location nuance: the Yaml arm is error.rs:101 specifically (error.rs:100 is the Json arm); the claim's ":100-101" range covers it. Severity low is correct — this is an observability/repair-ergonomics gap, not a correctness defect; the positional data exists and is discarded. Trivial fix: render the Yaml arm as `format!("{message} at line {line}")` when line>0 (the YamlError Display already does exactly this — user_message just needs to stop dropping the field). Related, out-of-scope observation: the Json arm (error.rs:100, 130-135) has a mirror weakness — `line` is hardcoded to 0 in From<JsonError> and the carried `column` is also dropped by `..`, so JSON errors likewise report no position. Worth folding into the same fix.

**Recommended resolution:** Render the line (and column where available) in user_message for Yaml/Json/Schema variants, e.g. 'line {line}: {message}'. The fields are already populated.

---

### F76. [INFO] Crash-time lock cleanup relies entirely on stale-lock recovery (no signal handler); first contender after a hard kill waits up to the staleness window

`tradeoff` · location `src/core/fs.rs:256-263 (Drop), :344-364 (recover_stale_lock), :12 (STALE_LOCK_AGE_MILLIS = 120s)` · reporter-confidence high · verification **CONFIRMED**

**Description:** FileLock releases on normal scope exit and on panic-unwind via Drop, but a SIGKILL/SIGSEGV leaves the .lock on disk. Recovery is by the stale-lock path: a dead PID makes the lock recoverable immediately (kill(pid,0) -> ESRCH), but a lock whose PID is reused/live or whose timestamp is fresh is only broken after the 2-minute staleness window (or 1s acquire-timeout error before then). There is no signal handler to proactively clean up.

**Why it matters:** This is a deliberate and largely sound design — PID-liveness check means the common SIGKILL case recovers on the very next run. The residual gap is narrow (fresh-timestamp lock from a process killed seconds ago, or PID reuse) and self-heals within the window. Worth documenting, not fixing.

**Impact:** In a rare window a contender errors with 'lock already exists; retry later' for up to ~2 minutes after a hard kill. Recoverable, non-corrupting.

**Likelihood:** Low.

**Evidence (reporter):** fs.rs Drop calls release_inner only on !released; no libc signal handler anywhere. Empirically: dead-PID lock recovered instantly; dead-PID + fresh-timestamp lock returned 'Archiva lock already exists ... retry later' after a 1.0s timeout (correct, conservative).

**Independent verification:** Code read (src/core/fs.rs):
- Line 12: STALE_LOCK_AGE_MILLIS = 2*60*1000 (120s); line 13: LOCK_RETRY_TIMEOUT_MILLIS = 1000 (1s). Confirmed.
- Drop (256-263): release_inner() runs only when !released. No panic=abort in Cargo.toml [profile.release] (grep found no panic key), so unwind runs Drop on panic. SIGKILL/SIGSEGV bypass Drop. Confirmed.
- No signal handler anywhere: grep -rniE "signal|sigaction|sigterm|atexit|ctrlc|register.*handler" over src/ returns nothing; the only extern "C" blocks are kill() (fs.rs:583) and anchor.rs FFI test stubs (8060/8064). No libc dep in Cargo.toml. Confirmed.
- Recovery logic lock_is_recoverable (426-443): (1) if lock_owner_is_live(pid) -> false; (2) if timestamp expired -> true; (3) if timestamp parses but NOT expired -> false; (4) else mtime fallback. So recovery requires PID dead AND (timestamp expired OR unparseable-with-expired-mtime).

Empirical (target/release/archiva write-decision in /tmp/archtest on lock .decisions/src.ts.lock):
- Dead PID (999999) + expired ts (>2min): recovered in 0.011s, wrote dec_002/dec_004.
- Dead PID + fresh ts (now): 1.007s then "Archiva lock already exists ... retry later".
- Dead PID + 90s-old ts (within window): 1.008s then same error.
- Live PID ($=643051) + ancient ts: 1.008s then same error (live PID blocks recovery regardless of age -> PID-reuse risk).
- Normal exit after clean write: lock absent (Drop released it).

**Verifier notes / severity correction:** Claim CONFIRMED, severity info/tradeoff is correct. Architecture is exactly as described: no signal handler, Drop covers normal exit + panic-unwind only, SIGKILL leaves the .lock on disk, recovery is purely via the stale-lock path, and a contender errors "lock already exists; retry later" (after a 1s acquire timeout) until the 120s window elapses. Non-corrupting and recoverable.

One precision correction to the claim's narrative ("a dead PID makes the lock recoverable immediately (kill(pid,0)->ESRCH)"): a dead PID is necessary but NOT sufficient. lock_is_recoverable short-circuits on live PID, but a dead PID with a FRESH parseable timestamp is still NOT recovered until the 120s timestamp window expires (verified: dead-PID + fresh-ts and dead-PID + 90s-ts both errored after 1s; only dead-PID + >120s-ts recovered instantly). The claim's own empirical section already states this correctly, and the description's clause "or whose timestamp is fresh is only broken after the 2-minute staleness window" captures it, so the finding is internally consistent. The practical consequence is slightly worse than "dead PID recovered instantly" suggests: the liveness check does NOT shorten the wait when the crashed process wrote a fresh timestamp immediately before dying — the full ~2-minute wait applies even when the PID is verifiably dead. Still info-level: bounded, recoverable, no corruption, and acceptable for a repo-local single-writer tool. Worth a one-line note that adding a SIGTERM/SIGINT handler to unlink owned locks would close the common Ctrl-C / OOM-kill case (SIGKILL inherently cannot be trapped).

**Recommended resolution:** Document the recovery model and the 2-minute window in operator docs. Optionally surface lock recovery events through the proposed diagnostic channel so a stuck lock is visible. No code change required for correctness.

---

## DIMENSION: Testing Strategy and Confidence  — score 7/10

> Archiva v2's test suite is large and, for the behaviors it covers, genuinely high-confidence rather than shallow: 301 lib tests + 13 Rust integration tests pass, the differential oracle's 56 scenarios pass byte-strict against the release binary (I ran it to completion), crash-consistency is tested with a real killed-child subprocess at every atomic-write stage, multi-process lock contention is tested with real spawned binaries and a barrier, and the git object reader has ~37 hand-crafted corruption/edge tests covering SHA-1, SHA-256, v1/v2 pack indexes, ofs/ref deltas, and a hand-written zlib inflate. I independently verified the SHA-256 git path and MCP write path end-to-end against the binary. However, confidence is unevenly distributed and the suite has structural blind spots. The single biggest one: the differential oracle (the TypeScript reference implementation) only understands TS/JS via ts-morph, so the Rust and C/C++ native extractors — which are ~12,000 of the 12,341 lines in anchor.rs and the bulk of the system's novel logic — have NO behavioral oracle and are validated only by self-referential unit tests and a coverage-counting corpus harness. C/C++ is not fuzzed at all (the property fuzzer only covers Ts/Tsx/Rust). I also found and reproduced a real, completely untested O(n²) performance defect in the anchor extractors that makes a single large file (or large function body) take tens of seconds. Heavy validation (differential, stress, scale, corpus, property soak) runs only weekly/on-dispatch, not on PRs, so day-to-day regressions are caught only by the much narrower PR gate.

*Score rationale:* The tested surface is tested deeply and adversarially (crash injection, real multi-process contention, corruption corpora, byte-strict differential) — well above typical port-quality. The score is held to 7 by three structural gaps: (1) the most novel/largest code path (Rust + C/C++ extraction) has no differential oracle and C/C++ has no fuzzing; (2) a real O(n^2) defect went undetected because no test exercises large single files; (3) the strongest cross-checks run only weekly, not per-PR. These are coverage-shape problems, not test-quality problems.

**Verified behaviors (checked, not assumed):**

- Ran `cargo test --release`: 301 lib tests pass, 1 ignored (property_extended_serialization_and_diff soak), plus integration suites cli_lock_process (9), mcp_stdio (3), cli_stdin_limits (1) — matches the claimed 301+13.
- Ran the differential to completion: `ARCHIVA_RUST_BIN=target/release/archiva tsx tools/archiva-differential.ts` -> status "passed", 56/56 scenarios ok, 0 failures. The TS oracle source (src/core/anchor.ts etc., bin/archiva.js) is still present and is what the harness drives.
- Confirmed the differential comparison is byte-strict (scenario() does JSON.stringify(left)===JSON.stringify(right)) with only timestamp normalization in normalizeText — so passes are meaningful, not loosely matched.
- Confirmed src/core/anchor.ts (the oracle) extracts anchors ONLY for TS/JS via ts-morph (getFunctions/getClasses/getVariableDeclarations); it has no Rust or C/C++ extraction. anchor.rs is 12,341 lines; anchor.ts is 121 lines.
- Confirmed property_tests.rs fuzzes parsers/diff/anchors but its SourceKind enum is only {Ts, Tsx, Rust} — C/C++ extraction (extract_c_family_anchors, ~230 lines) is never fuzzed.
- Reproduced an O(n^2) blowup in anchor extraction: same project, post-tool-use on a TS file of 10k/20k/40k brace-less lines took 0.53s / 2.29s / 9.87s (4.3x per doubling). A single 20k-line Rust function body (458KB) took 23.8s; the TS equivalent took 2.1s.
- Confirmed the scale/corpus harness (tools/archiva-scale-smoke.ts) skips any file >256KiB (corpusMaxFileBytes, line 1240) and generates only small synthetic files, so the O(n^2) path is never exercised at scale. At exactly the 256KiB cap a Rust file already takes 6.5s.
- Verified the SHA-256 git object reader works end-to-end: created a sha256 repo, recorded a decision, changed+committed the file, and `archiva lint` reported `arc/stale ... code fingerprint differs` by reading HEAD:src/bar.ts via the native reader (no git spawn).
- Verified MCP over stdio live: initialize returns protocolVersion 2024-11-05; tools/call write_decision returns the recorded text envelope.
- Confirmed crash-consistency test atomic_write_killed_child_never_leaves_truncated_target spawns a real child and SIGKILLs it at Create/Write/Sync/Replace stages, asserting the target is always a parseable old-or-new dlog.
- Confirmed JSON/YAML parsers have DEFAULT_MAX_DEPTH=512 and DEFAULT_MAX_BYTES=10MiB guards and recursive descent is depth-bounded; property fuzzer feeds parser-hostile fragment text.
- Confirmed PR CI (.github/workflows/ci.yml) runs cargo test + npm test + differential:release + stress:rust-port + reduced scale smoke, but property:soak and the full corpus/long-horizon matrix run only in validation.yml (weekly cron / workflow_dispatch).

### F77. [HIGH] Rust and C/C++ anchor extractors have no differential oracle (largest novel code path is self-validated only)

`architecture` · location `tools/archiva-differential.ts (oracle scope) vs src/core/anchor.rs:241-249, src/core/anchor.ts:19-68` · reporter-confidence high · verification **CONFIRMED**

**Description:** The differential harness compares the Rust binary against a TypeScript reference implementation, and this is the strongest correctness signal in the project. But the TS oracle (src/core/anchor.ts) only extracts anchors for TS/JS via ts-morph. The Rust (extract_rust_anchors) and C/C++ (extract_c_family_anchors) extractors — together ~12,000 of anchor.rs's 12,341 lines and the bulk of the system's hand-written, non-trivial logic — are dispatched before the TS path (anchor.rs:242-248) and have NO oracle to compare against. Their correctness rests entirely on hand-written unit tests (whose expected values were authored by the same team) and the corpus harness, which only counts anchor-kind coverage (assertRustCorpusCoverage/assertCxxCorpusCoverage in archiva-scale-smoke.ts) rather than checking extracted ranges against any independent ground truth.

**Why it matters:** The whole premise of the differential suite is that an independent implementation catches porting/logic errors. That safety net does not extend to the majority of the code. A wrong line range, missed anchor, or mis-named anchor in the Rust/C++ extractors would not be caught by any cross-check — only by a unit test someone thought to write for that exact case.

**Impact:** Silent extraction errors in Rust/C/C++ files (wrong anchor ranges, missed or phantom anchors) would corrupt decision-to-code mapping for the languages the v2 effort specifically added, with no automated detection.

**Likelihood:** The extractors are large hand-written token scanners; subtle range/edge errors are plausible and have no oracle to surface them.

**Evidence (reporter):** src/core/anchor.ts:19-68 extracts only via sourceFile.getFunctions/getClasses/getVariableDeclarations (TS/JS). anchor.rs:242-248 dispatches is_rust_file -> extract_rust_anchors and is_c_family_file -> extract_c_family_anchors before the TS path. tools/archiva-scale-smoke.ts assertRustCorpusCoverage/assertCxxCorpusCoverage only assert >=2 anchor kinds and presence of a structural kind, not correctness of ranges.

**Independent verification:** Verified by reading source and driving the binary:

1. TS oracle is TS/JS-only. src/core/anchor.ts:19-68 extracts solely via ts-morph (sourceFile.getFunctions/getClasses/getVariableDeclarations/getExportedDeclarations + IfStatement blocks). There is NO language switch and NO Rust/C path anywhere in the TS codebase: `grep -rnl 'extract_rust|extractRustAnchors|extract_c_family|extractCxx' src/ bin/` returns ONLY src/core/anchor.rs. Feeding a .rs file to extractAnchors would have ts-morph parse Rust as TypeScript (garbage), so anchor.ts cannot serve as an oracle for Rust/C.

2. Rust/C dispatched before TS, as claimed. src/core/anchor.rs:241-247 — extract_anchors() returns extract_rust_anchors(source) for is_rust_file and extract_c_family_anchors(source) for is_c_family_file before reaching the TS tokenizer at line 249. Confirmed fn boundaries: extract_rust_anchors @409, extract_c_family_anchors @455.

3. Differential harness exercises only TS/JS. In tools/archiva-differential.ts every source fixture written via writeFile is .ts/.tsx (457 ".ts" + 17 ".tsx") plus one ".js"; ZERO .rs/.c/.cc/.cpp/.h fixtures (`grep -nE 'writeFile\([^)]*\.(rs|cc|cpp|cxx|hpp)'` → no match). The 153 "rust" tokens are the runtime BINARY name in the comparison harness (line 53: { name: "rust", command: rustBin }), not source-language fixtures. The harness compares the Rust binary vs the TS reference on TS inputs only.

4. Corpus harness only counts anchor-KIND coverage, not ranges. tools/archiva-scale-smoke.ts:633-678 assertRustCorpusCoverage/assertCxxCorpusCoverage assert coveredKinds.length>=2 and presence of one structural kind — no comparison of extracted ranges to any independent ground truth. Its "anchors" come from its own hand-written regexes (findRustCorpusAnchors @1568, findCxxCorpusAnchors @1602), which are not a second parser checking ranges. differential.ts references corpus 0 times; corpus logic lives entirely in scale-smoke.

5. Confirmed at runtime that ranges are unchecked. In /tmp/archiva-audit (git-init'd, archiva init'd) I wrote src/lib.rs with a Widget struct/impl and free_function. The Rust extractor IS exercised for anchor-NAME existence at write time (rejecting fn:does_not_exist_anywhere and listing "export:Widget, export:Widget.describe, export:Widget.new, export:free_function, fn:Widget.describe, fn:Widget.new, fn:free_function, impl:Widget, struct:Widget"), but a decision with a deliberately wrong range — fn:free_function lines [1,2] when the function is actually at lines 18-20 — was accepted ("Recorded dec_004"), and `archiva lint` reported "No decision issues found." So extractor output for Rust is never cross-checked against any oracle, and the extracted-vs-claimed range mismatch is silent.

**Verifier notes / severity correction:** Core claim CONFIRMED: the Rust and C/C++ anchor extractors — the largest novel hand-written logic the v2 effort added — have no differential or ground-truth oracle. The only TS-vs-Rust differential harness exercises TS/JS exclusively; the corpus harness checks kind-coverage, not range correctness; remaining confidence rests on same-team unit tests. Silent extraction errors (wrong/missing/phantom ranges) in .rs/.c/.cpp files would corrupt decision-to-code mapping with no automated detection, and I demonstrated lint does not catch a wrong range.

Two corrections to the claim's specifics (do not change the verdict):
- The line-count attribution is inflated. anchor.rs is 12,341 lines, but tests occupy lines 7251-12341 (~5,090 lines) and total non-test code is ~7,250 lines, not 12,000. The dedicated Rust+C extractor region is roughly lines 409-2227 (~1,800 lines); the TS tokenizer/extractor follows. The qualitative point (Rust+C+TS hand-written extraction is the bulk of non-trivial logic, and the Rust/C portions lack an oracle) is correct; the "~12,000 of 12,341 lines" figure is not.
- Slight nuance on "self-validated only": the Rust/C extractors DO get partial implicit exercise via anchor-name existence checks at write-decision time (the binary enumerates extracted anchor names and rejects unknown ones). This validates that names round-trip, but still provides no independent confirmation that the produced name set or ranges are CORRECT, and ranges are explicitly unchecked.

Severity high is appropriate: this is a genuine testing-strategy/confidence gap on the highest-risk, newest code path, not a style preference. It is an architectural/process weakness (absence of independent verification) rather than a proven runtime defect — recommend building a ground-truth oracle for Rust/C (e.g., compare extracted ranges against tree-sitter or rustc/clang span output over a real corpus, or extend the differential harness with a non-ts-morph reference) before relying on these extractors in production.

**Recommended resolution:** Add an independent oracle for at least one structural property of Rust/C/C++ extraction (e.g. cross-check anchor line ranges against tree-sitter or rustc/clang-derived spans in CI), or substantially expand golden-file tests sourced from real third-party files with manually verified expected anchors. At minimum, document that Rust/C/C++ extraction is oracle-free so reviewers weight unit tests accordingly.

---

### F78. [HIGH] O(n^2) anchor extraction is completely untested at scale and degrades to tens of seconds on a single large file

`defect` · location `src/core/anchor.rs (extract_rust_anchors / TS tokenizer+extractor); harness gap in tools/archiva-scale-smoke.ts:1240` · reporter-confidence high · verification **CONFIRMED**

**Description:** Anchor extraction is super-linear in file size. I measured post-tool-use (which extracts anchors) on single files of increasing size with no prior decision (so pure scan+extract, no diff): TS 10k/20k/40k brace-less lines = 0.53s / 2.29s / 9.87s (consistent 4.3x per doubling = O(n^2)); a single 20k-line Rust function body (458KB) = 23.8s vs 2.1s for the TS equivalent. No test or harness exercises this: synthetic scale files are small many-function files, and the corpus harness silently skips any file larger than 256KiB (corpusMaxFileBytes). At exactly 256KiB a Rust file already takes 6.5s. Large generated files, big match/switch bodies, data tables, and vendored sources routinely exceed this in real repos.

**Why it matters:** post-tool-use runs as a Claude/agent hook after edits and lint/status walk the whole project. On a repo containing even one large file, the hook becomes multi-second to multi-minute, which in an agent loop reads as a hang. The tool's own validation can never see this because every harness either uses small files or skips large ones.

**Impact:** Operational hangs on real repos with large files; agent hooks time out; lint/status latency spikes. Not a crash (memory stays low, ~100MB at 200k lines), purely CPU/time.

**Likelihood:** High for any non-trivial repo — large single files are common, and the hook runs on every edit.

**Evidence (reporter):** Measured on /tmp scratch projects with the release binary: TS extraction 10k=0.53s,20k=2.29s,40k=9.87s; single 20k-line Rust fn body=23.8s (TS equiv 2.1s); 256KiB Rust file=6.5s; 100k/200k-line files time out at 30s. tools/archiva-scale-smoke.ts:1240 `if (!stat.isFile() || stat.size > corpusMaxFileBytes) continue;` (corpusMaxFileBytes=256*1024).

**Independent verification:** Reproduced with the release binary via `hooks post-tool-use FILE` in a /tmp scratch project (no prior decision = pure tokenize+extract, no diff), `/usr/bin/time -v`:

TS braceless (`const xN = N + N;`):
- 10k lines (266KB): 0.82s
- 20k lines (566KB): 3.81s  (4.6x)
- 40k lines (1.16MB): 13.98s (3.7x)
Super-linear (~O(n^2)) confirmed. maxRSS 10.6MB / 18.9MB / 35.1MB — CPU-bound, not memory.

Rust single huge fn body vs TS equivalent (both ~20k lines, ~600KB):
- bigfn_20000.rs: 46.88s, maxRSS 14.6MB
- bigfn_20000.ts: 3.32s, maxRSS 14.2MB
~14x Rust/TS asymmetry confirmed (claim cited 23.8s vs 2.1s; this box is slower but the ratio holds and is worse than claimed).

256KiB single-fn Rust file (262150 bytes): 9.15s (claim said 6.5s — worse here).
100k-line TS file (1.98MB): timed out >40s (exit 124), matching the >30s timeout claim.

Root cause (Rust path) pinned in src/core/anchor.rs: the scope walk in collect_rust_item_anchors (line 733; loop at 747-753) calls is_rust_direct_scope_member(tokens, start, index) for every index. is_rust_direct_scope_member (line 1729) calls rust_depths_before(tokens, start, index) (line 1760), which iterates tokens[start..index] — O(index). Inside one large function body the outer loop visits every token while none dispatch to an item, so the per-index O(n) depth scan runs n times = O(n^2). This precisely explains the Rust>>TS asymmetry.

Code path verified: post_tool_use (src/core/project.rs:223) calls extract_anchors even when no .dlog exists for the file (else-branch at lines ~233-237), so the hook does a full scan+extract on every edited file regardless of whether decisions exist — confirming the operational exposure.

Harness gap confirmed: tools/archiva-scale-smoke.ts:1240 `if (!stat.isFile() || stat.size > corpusMaxFileBytes) continue;` with corpusMaxFileBytes = 256*1024 (line 142) silently skips any corpus file >256KiB. Synthetic scale files are small many-function files: renderFunction (line 777) emits ~7-line functions, renderSource (771) joins functionsPerFile of them; syntheticFunctionLines (813) = [slot*7+1, +5]. No test in src/core/anchor.rs or tests/ exercises a large single file or single large function (grep for repeat/10_000/20_000/100000/huge/large_file/perf in anchor.rs and tests/ returned nothing).

**Verifier notes / severity correction:** Claim fully upheld; the only discrepancies are that absolute timings are LARGER than claimed on this (slower) host — Rust 20k fn = 46.9s vs claimed 23.8s, 256KiB Rust = 9.15s vs 6.5s — so the defect is worse, not weaker. Behavior is a CPU/time hang, not a crash (maxRSS stayed 10-35MB, confirming the "memory stays low" sub-claim). Severity high is appropriate: post-tool-use runs extract_anchors on every edited file even with zero decisions (project.rs:223), so a single large generated/vendored/data-table/big-match-body file (common in real repos, and exactly what the 256KiB corpus skip hides) makes agent hooks and lint/status latency spike into tens of seconds. The Rust O(n^2) root cause is precisely located (rust_depths_before called per-index); I did not pin the exact TS quadratic site, but TS super-linearity is empirically solid. Recommended fix: thread running brace/paren/bracket depth counters through the scope walk (or precompute a depth prefix array) instead of recomputing rust_depths_before from `start` on every index, and add a large-single-file / single-large-function perf regression test plus a corpus path that does not skip files above 256KiB.

**Recommended resolution:** Profile and fix the quadratic loop in the extractor(s); add a regression test asserting extraction of a >=256KiB / >=20k-line single file completes within a bound (e.g. <2s); add a large-single-file case to the scale harness and stop silently skipping big files (or at least record how many were skipped).

---

### F79. [MEDIUM] Strongest validations (differential, stress, scale, corpus, property soak) run only weekly/on-dispatch, not on PRs

`operational` · location `.github/workflows/ci.yml vs .github/workflows/validation.yml` · reporter-confidence high · verification **CONFIRMED**

**Description:** The PR CI gate runs cargo test, npm test (TS compat), differential:release, stress:rust-port, and a reduced scale smoke. But the property soak (4096-case extended fuzz, the only deep fuzzing), the full external/seeded scale runs, the external-corpus scale, and the entire long-horizon corpus matrix (rust-lang/rust, llvm, linux, tokio, TypeScript, etc.) run only in validation.yml on a weekly cron or manual dispatch. So the heaviest, most representative validation is not a merge gate.

**Why it matters:** A regression that only manifests at scale, under the soak fuzzer, or on real-world corpora can merge and sit undetected until the next Monday run (or until someone manually dispatches). The day-to-day confidence is the narrower PR gate, which — combined with the oracle and large-file gaps above — is thinner than the raw test count suggests.

**Impact:** Delayed detection (up to a week) of scale, fuzz, and real-corpus regressions; possible release of an affected build if cut between scheduled runs.

**Likelihood:** Medium — most regressions are caught by the PR gate, but the classes the heavy suite is designed to catch are exactly the ones the PR gate cannot.

**Evidence (reporter):** .github/workflows/ci.yml `test` job runs differential:release/stress:rust-port/reduced scale but NOT property:soak (grep: not present). .github/workflows/validation.yml:78 runs property:soak and the long-horizon matrix only under `on: workflow_dispatch` / `schedule: cron "0 7 * * 1"`.

**Independent verification:** Read both workflow files in full and grepped targets.

PR CI gate (.github/workflows/ci.yml): `on: push branches:[main]` + `pull_request` (lines 3-6). The `test` job runs: `npm run check`, `npm run build`, `npm test`, `npm run differential:release`, `npm run stress:rust-port`, and `npm run scale:smoke` with REDUCED env (ARCHIVA_SCALE_FILES=32, DECISIONS=12, PARITY_FILES=16). `grep -n property .github/workflows/ci.yml` => NONE. `grep scale:corpus|long-horizon|stress:soak|SCALE_SEEDED` => NONE in ci.yml. Cross-platform job runs `cargo test --all-targets --locked --quiet` with NO `--ignored` flag.

Heavy validation (.github/workflows/validation.yml): `on: workflow_dispatch` + `schedule: cron "0 7 * * 1"` (Monday 07:00). Runs `npm run property:soak` (line 78), `stress:soak`, `benchmark:compare`, full `scale:smoke`, the Combined seeded scale (ARCHIVA_SCALE_SEEDED=1, SEEDED_FILES=100000, SEEDED_DECISIONS=1000000), External corpus scale, Rust self-corpus scale. The `long-horizon-corpus` matrix (rust-lang/rust, cargo, ripgrep, tokio, torvalds/linux, llvm, TypeScript, node, react, next) is gated `if: github.event_name == 'schedule' || inputs.run_long_horizon == 'true'`.

Soak depth confirmed: package.json `property:soak => cargo test --quiet --lib property_extended_serialization_and_diff -- --ignored`. src/core/property_tests.rs:152-155 — that fn is `#[ignore]` and loops `for _ in 0..4096`. The 7 non-ignored property tests run only DEFAULT_CASES=128 (property_tests.rs:9). So PR CI exercises 128-case property tests but never the 4096-case ignored soak (no `--ignored` in ci.yml), and never the seeded/external/long-horizon scale runs.

**Verifier notes / severity correction:** Claim is accurate in every specific (file:line, triggers, which targets are/aren't on the PR gate). Two refinements, neither weakening it: (1) PR CI does run lighter cousins — 128-case property tests via plain cargo test, stress:rust-port, differential:release, and a reduced scale:smoke — so it is not zero coverage, just shallow; the deep variants (4096-case soak, seeded 1M-decision scale, external/long-horizon corpora) are genuinely absent. (2) Mitigation: .github/workflows/publish.yml (on: release published / workflow_dispatch) runs `property:soak` + `differential` + `smoke:package` before publishing (lines 84-89), so a release cut through the official Publish workflow does gate on the 4096-case soak — but NOT on the seeded/external/long-horizon scale matrix, and only if releases go through that workflow. The core operational risk stands: scale/fuzz/real-corpus regressions can sit undetected up to a week (or until a manual dispatch), and nothing on the per-PR merge gate would catch them. Medium severity is appropriate — this is a defense-in-depth/latency-of-detection gap, not a correctness defect in the shipped binary.

**Recommended resolution:** Promote at least a bounded property-soak and a small real-corpus run into the PR gate (they are fast enough), or gate releases on a fresh heavy-validation run rather than the most recent scheduled one. Document the cadence so release auditors know what was actually run for a given commit.

---

### F80. [MEDIUM] Cross-platform behavior (Windows/macOS/arm/musl) and parent-dir durability fsync are unverified locally; CI-only

`operational` · location `src/core/fs.rs:770-777 (best_effort_flush_parent_dir cfg(unix)/cfg(not(unix))); .github/workflows/ci.yml matrix` · reporter-confidence medium · verification **CONFIRMED**

**Description:** All local tests run on Linux. Platform-divergent code paths exist and are exercised only in CI: the parent-directory durability fsync after atomic rename is implemented for Unix and is a no-op on non-Unix (Windows), so the crash-consistency guarantee proven by atomic_write_killed_child_never_leaves_truncated_target is weaker on Windows and not tested there in the crash-injection form. Windows-reserved path rejection, the Windows raw-access-denied lock classification (classifies_windows_raw_access_denied_as_contention_without_path_probe), and arm64/musl builds are validated only by the CI matrix. The differential and lock-contention multi-process tests, which are the most behavior-revealing, run only on ubuntu in CI's `test` job.

**Why it matters:** The strongest behavioral tests (differential, multi-process lock races, crash injection) effectively have Linux-only confidence. Windows/macOS get build+unit-test coverage but not the cross-process and crash-injection scrutiny, and the durability story differs on Windows.

**Impact:** Platform-specific lock/durability/path bugs could ship to Windows/macOS/arm/musl users with lower test scrutiny than Linux.

**Likelihood:** Medium — the cfg-divergent and OS-error-classification paths are exactly where portability bugs hide.

**Evidence (reporter):** fs.rs:776 `#[cfg(not(unix))] fn best_effort_flush_parent_dir(_path: &Path) {}` (no-op on Windows). ci.yml `test` job (npm test/differential/stress/scale) runs only on ubuntu-latest; the cross-platform job only does cargo build+test. This audit ran exclusively on Linux per the task constraints.

**Independent verification:** Code read + CI inspection on Linux host (uname: Linux mave 6.8.0-1050-oracle x86_64). CONFIRMED items: (1) fs.rs:770-777 — `#[cfg(unix)] best_effort_flush_parent_dir` opens parent and sync_all(); `#[cfg(not(unix))]` variant is a literal no-op `{}`. On Windows durability instead rides on MoveFileExW with MOVEFILE_WRITE_THROUGH (fs.rs:742-745), a different mechanism than the Unix rename+parent-fsync, so the crash-consistency *durability* path is not equivalent and is never exercised on the Linux audit host. (2) fs.rs:931-942 `classifies_windows_raw_access_denied_as_contention_without_path_probe` is `#[cfg(windows)]`-gated → not compiled or run locally; only Windows CI exercises it. (3) .github/workflows/ci.yml: the `rust-cross-platform` matrix (ubuntu/macos/windows-latest) runs ONLY `cargo build --all-targets` + `cargo test --all-targets --locked`. The `test` job carrying `npm test`, `Native differential` (differential:release), `Native stress` (stress:rust-port), and `Reduced scale smoke` runs ONLY on `ubuntu-latest`. So the most behavior-revealing differential/stress/scale suites never run on Windows/macOS, and arm64/musl get only build+stage+package-smoke (native-package job), no functional test suite. (4) Task constraints fixed this audit to Linux; the binary and 301+13 tests were all validated on Linux only. CORRECTION to claim's overstatement: `atomic_write_killed_child_never_leaves_truncated_target` (fs.rs:1303) is NOT cfg-gated and uses cross-platform `std::process::abort()` (fs.rs:201) + `current_exe()` child spawn, so it DOES run on windows-latest and macos-latest inside `cargo test --all-targets`. Thus the no-truncated-target *invariant* IS tested on Windows in crash-injection form; what is weaker on Windows is specifically the parent-dir fsync (no-op), not the truncation test. Also Windows-reserved-name rejection (paths.rs:217,234-243) is platform-independent code with Linux-run unit coverage (paths.rs:415-416), so it is NOT validated "only by the CI matrix."

**Verifier notes / severity correction:** Core operational claim holds: platform-divergent code paths (no-op parent-dir fsync on Windows; cfg(windows) lock-classification) exist and are exercised only in CI, the audit ran exclusively on Linux, and the high-signal differential/stress/scale suites plus multi-process lock contention run only on ubuntu while macOS/Windows/arm/musl get lighter scrutiny (build+test, or build+package-smoke for arm/musl). Severity medium is appropriate — this is a test-coverage/operational-risk gap, not a verified defect; no platform bug was demonstrated, only reduced scrutiny. Two factual corrections reduce the claim's reach: (a) the crash-injection killed-child invariant test is cross-platform and runs on Windows/macOS via cargo test (the claim wrongly states it is not tested there in crash-injection form); the real Windows gap is the parent-dir fsync being a no-op, relying on MOVEFILE_WRITE_THROUGH instead. (b) Windows-reserved-name path rejection is platform-independent and has Linux unit coverage, so it is not CI-matrix-only. Recommend: (1) add a CI step that runs the differential/stress harnesses on windows-latest and macos-latest (or at least a reduced-scale variant) so cross-platform functional behavior is verified, not just compiled; (2) add a Windows-specific durability/atomic-rename test asserting no truncated target after MoveFileExW failure, since the Unix parent-fsync path is bypassed; (3) document explicitly that arm64/musl receive build+package-smoke only.

**Recommended resolution:** Run the multi-process lock and differential suites on the Windows and macOS CI runners (not just cargo test), and add a Windows crash-consistency equivalent or document that parent-dir durability is best-effort/no-op on Windows so the crash-consistency claim is platform-scoped.

---

### F81. [LOW] Anchor fuzzer asserts only non-panic and structural invariants, not extraction correctness

`tradeoff` · location `src/core/property_tests.rs:520-540 (assert_anchor_extraction_invariants)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The property tests for anchor extraction verify that extraction does not panic and that outputs satisfy shape invariants (anchor non-empty, start>=1, end>=start, complete==diagnostics.empty, diagnostic line/column>=1). They do not assert that the extracted anchors are correct (right names, right ranges) for the generated input. This is a reasonable and common fuzzing posture, but it means the property suite provides robustness confidence, not correctness confidence — correctness for Rust/C/C++ rests entirely on hand-authored golden values (see the oracle finding).

**Why it matters:** Reviewers may over-read the 8 property tests as validating extraction logic; they validate that it doesn't crash and produces well-formed output, which is a narrower guarantee.

**Impact:** No direct defect; a documentation/expectation-calibration risk that compounds the oracle gap.

**Likelihood:** n/a (characterization).

**Evidence (reporter):** property_tests.rs assert_anchor_extraction_invariants checks only emptiness/ordering/positivity of fields; the JSON/YAML/dmap property tests do assert real round-trip equality, but the anchor property tests do not assert any reference output.

**Independent verification:** Read /home/ubuntu/archaeo/src/core/property_tests.rs directly.

assert_anchor_extraction_invariants (lines 528-540) asserts ONLY structural/shape invariants:
- line 529: `assert_eq!(extraction.complete, extraction.diagnostics.is_empty())`
- lines 530-534: per-anchor `!anchor.anchor.is_empty()`, `anchor.start >= 1`, `anchor.end >= anchor.start`
- lines 535-539: per-diagnostic `line >= 1`, `column >= 1`, `!message.is_empty()`
No reference name/range is compared.

Both anchor property tests delegate to this and nothing else:
- property_parser_malformed_sources_do_not_panic (lines 138-149) feeds `random_malformed_source(...)` for Ts/Tsx/Rust and calls only `assert_anchor_extraction_invariants(&extraction)`. The test name and the `random_malformed_source` generator (fragments_for, lines 489-525, e.g. "return `unterminated ${", "/* open", "let value = r#\"") confirm inputs are intentionally malformed, so a golden reference would not even be well-defined.
- ignored property_extended_serialization_and_diff (line 180) likewise calls only the invariant check for anchors.

By contrast, the serialization property tests DO assert real round-trip equality:
- JSON: lines 50, 53 (`parse_json(&compact/&pretty).unwrap() == value`), line 98, 157
- YAML: lines 63, 108, 160 (`parse_yaml(&rendered).unwrap() == value`)
- dmap: lines 74, 85, 164 (`render_dmap(&parse_dmap(&rendered).unwrap()) == rendered`)
And diff tests assert count equality (lines 125-126, 170-171).

So the asymmetry described in the claim is exactly as stated: serialization/diff property tests assert correctness via round-trip/count equality; anchor property tests assert only non-panic + structural shape, never extracted-anchor correctness against a reference.

**Verifier notes / severity correction:** Claim is accurate as written. The location is precise (528-540) and the description matches the code. One nuance that arguably softens it further toward "info": the anchor property inputs are deliberately MALFORMED random sources (random_malformed_source / fragments_for), for which a correct golden anchor set is generally undefined — so structural-invariant-only checking is the only well-posed assertion for these particular generators. A correctness-oriented property test would require a separate generator that produces well-formed-by-construction code paired with a known-correct anchor set, which does not exist here. The characterization as a tradeoff/low (robustness confidence, not correctness confidence; correctness for Rust/C/C++ rests on hand-authored goldens) is correct. Not a defect. severity low/info is appropriate; I keep low to match the claim.

**Recommended resolution:** Keep the non-panic fuzzer, but add a small number of property checks with computable ground truth (e.g. inject N well-formed functions and assert N function anchors are recovered) to turn some of this into correctness signal.

---

### F82. [LOW] audit-v2-completion is a string-presence checker, not a behavioral gate, but is wired into `npm run check`

`tradeoff` · location `tools/audit-v2-completion.mjs:201-265 (auditBehaviorSurface/auditLanguageAndGitCoverage/auditWorkflowEvidenceProducers)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The v2 completion audit (run via `npm run check` -> audit:v2) verifies readiness almost entirely by grepping source/workflow files for expected substrings: e.g. CLI dispatch 'covers required commands' checks that src/cli.rs contains the literal strings like "init"/"why"; MCP support checks src/mcp.rs includes "initialize"/"tools/list"/"tools/call"; SHA-256 support checks git.rs includes "Sha1"/"Sha256"; workflow checks grep YAML for needles. These pass regardless of whether the code behaves correctly. To its credit the tool is honest that external evidence is still required and only does deeper JSON validation when --evidence-dir artifacts are provided.

**Why it matters:** It can give a green 'completion audit OK' on a build whose behavior is broken, as long as the identifiers are present. It should not be mistaken for a behavioral acceptance test; the real behavioral signal is cargo test + differential.

**Impact:** Risk of false confidence if the audit is treated as a release gate on its own; low because it sits alongside real tests in `check`.

**Likelihood:** Low (it complements, not replaces, the test suites).

**Evidence (reporter):** audit-v2-completion.mjs uses includesAll/missingValues string matching against file contents for behavior/language/git/workflow checks; deeper assertions (status==="passed", C/C++ anchorKinds) only run when evidenceDir artifacts from a real heavy-validation run are passed in.

**Independent verification:** Read tools/audit-v2-completion.mjs in full. The behavior/language/git/workflow auditors use pure substring matching, not execution:
- includesAll (line 182-184) = values.every(v => text.includes(v)); missingValues (197-199) = values.filter(v => !text.includes(v)).
- auditBehaviorSurface (219-227): reads src/cli.rs/src/mcp.rs as text; CLI check (225) just asserts missingValues(cli, [\"init\"...]).length===0; MCP check (226) asserts includesAll(mcp, [\"initialize\",\"tools/list\",\"tools/call\"]). No binary is invoked.
- auditLanguageAndGitCoverage (229-241): git SHA-256 check (238) is includesAll(git,[\"GitObjectFormat\",\"Sha1\",\"Sha256\"]); C/C++ checks grep anchor.rs/project.rs for symbol/extension literals.
- auditWorkflowEvidenceProducers (243-256): greps YAML for needles (target names, corpus names, tee pipelines).
Deeper behavioral assertions exist ONLY under --evidence-dir: auditEvidenceArtifacts (270-316) parses real-run JSON and asserts parsed.status===\"passed\" (296) and C/C++ anchorKinds.function>0 / structural kinds (299-313). main() only calls it when evidenceDir is set (343-345).

Wiring confirmed: package.json scripts.check = \"npm run check:ts && npm run check:rust && npm run check:package && npm run audit:v2\"; scripts[\"audit:v2\"] = \"node tools/audit-v2-completion.mjs\" (no --evidence-dir, no --strict-complete). So `npm run check` runs the string-presence audit with zero artifacts.

False-positive proof (ran in /tmp clone via ARCHIVA_AUDIT_REPO_ROOT): replaced src/cli.rs and src/mcp.rs with files whose only body is `panic!(\"dispatch removed\")` / `panic!(\"mcp removed\")` but kept the literal command/method strings in comments. Output: \"ok CLI command dispatch covers required commands\", \"ok MCP supports initialize, tools/list, and tools/call\", \"audit OK (36 local evidence checks)\", exit 0. A totally non-functional CLI/MCP passes.

Mitigation confirmed real: check:rust = \"cargo fmt --check && cargo clippy ... && cargo test --quiet\" runs the actual Rust test suite in the same `check` pipeline, and the audit prints \"External completion evidence is still required before marking v2 complete.\" (line 330) so it does not self-declare release readiness.

**Verifier notes / severity correction:** Claim is accurate in every particular, including the to-its-credit caveats: the tool is honest about needing external evidence and only does behavioral JSON validation (status===\"passed\", C/C++ anchorKinds) when --evidence-dir artifacts from a real heavy-validation run are passed. Severity correctly low/tradeoff because (1) the substring checks function as cheap structural regression guards (catching accidental deletion of a command arm or workflow step), not as the sole correctness gate, and (2) they run alongside the genuine `cargo test --quiet` suite inside the same `npm run check`. The only real exposure is if someone treats `audit:v2` in isolation as a release gate; the script's own closing message argues against that. No correction to scope or recommendation needed. Recommended hardening (optional): for the highest-value claims, replace string greps with light behavioral probes (e.g. run `archiva --help`/`archiva mcp` initialize round-trip) so deleted dispatch logic fails the audit even when literals survive in comments.

**Recommended resolution:** Rename/document it as a 'presence and evidence-artifact audit' to avoid conflating it with behavioral verification; keep the strong gate on cargo test + differential + evidence-dir artifact validation.

---

## DIMENSION: Release Engineering and Packaging  — score 8/10

> Archiva v2 ships as an npm meta-package (@jalkarna/archiva) that selects one of 7 platform-specific native binaries via optionalDependencies + a postinstall (install-native.mjs) that copies the locally-resolved native binary into dist-native/archiva.exe, the path the bin key points at. I verified the binary (0.2.0), the committed placeholder shim, the install decision logic, the staging/validation/publish tooling, and all three workflows. The packaging is mature: no network downloads in the install path (supply chain is just npm registry + lockfile integrity), exact-pinned optionalDeps, magic-byte + --version verification at install, a source-checkout guard, idempotent publish, and broad post-publish smoke including musl-in-Alpine. The .exe filename on all platforms is cosmetic and functionally correct (bin key is "archiva"; POSIX runs the ELF fine; Windows shims are rewritten). The notable gaps are operational, not structural: a cryptic postinstall crash on Node <20.11 because engines is advisory while tooling hard-depends on import.meta.dirname, and a few doc/enforcement accuracy issues. Overall this is release-ready with one Medium operational risk worth addressing.

*Score rationale:* The packaging is well-engineered: 7 exact-pinned platform targets, a postinstall that copies a locally-resolved native binary (no network downloads, so supply chain reduces to npm registry + lockfile integrity), magic-byte + --version verification at install, a committed placeholder shim giving graceful --ignore-scripts UX, a source-checkout guard distinguishing dev from real installs, idempotent publish with retry-on-visibility, a strict metadata validator, and broad post-publish smoke (all platforms plus musl-in-Alpine, exercising real write/why/status/lint/MCP rather than version-only). The .exe-on-all-platforms naming is cosmetic and verified correct. Points off for the one Medium operational risk (cryptic postinstall failure below the Node engine floor since engines is advisory but tooling hard-depends on import.meta.dirname) and minor accuracy gaps (Cargo version parity only enforced at publish, loose substring version match, README wording). No structural defects; release-ready once the Node-floor crash is hardened.

**Verified behaviors (checked, not assumed):**

- Ran /home/ubuntu/archaeo/target/release/archiva --version => prints bare '0.2.0', exit 0; matches Cargo package version and package.json version.
- Confirmed package.json bin = {archiva: dist-native/archiva.exe} on all platforms; the .exe is cosmetic because the npm bin KEY is 'archiva'. `file dist-native/archiva.exe` => Node script (placeholder shim), `file dist-native/archiva` => ELF x86-64. Running the shim prints the clear 'native binary was not installed' message with exit 1 (graceful --ignore-scripts UX).
- Traced install-native.mjs end to end: resolves target via detectHostTarget/requireTarget, resolveNativePackage throws a clear 'Reinstall ... without --omit=optional/--ignore-scripts' error for an absent optional dep; isSourceCheckout() suppresses it only in a Cargo.toml-bearing non-node_modules tree. Verified the skip fires locally for an unselected target (darwin-arm64) and that linux-x64-gnu resolves because node_modules/@jalkarna/archiva-linux-x64-gnu is present.
- Confirmed no network downloads anywhere in the install path: grep over install-native.mjs / stage-native-package.mjs / smoke-native-package.mjs shows only spawnSync of the local binary and github URLs in metadata strings; integrity is npm-registry + lockfile based.
- Ran `node tools/validate-native-package-metadata.mjs` => 'Native package metadata OK (2 staged packages checked)', exit 0. Read the validator: it enforces optionalDependencies==exact version map, lockfile parity, bin/files arrays, postinstall/prepack scripts, Node engine string, Cargo rust-version and rust-toolchain channel, workflow action pins (checkout@v7, setup-node@v6, upload-artifact@v6) and rustup version pins.
- Read all 3 workflows: ci.yml builds all 7 native targets and smoke-tests each; publish.yml gates on a version-parity check (Cargo==package.json==tag v-prefix), runs heavy validation + long-horizon corpus, publishes native packages idempotently, then meta, then post-publish smoke across 5 platforms plus a musl-in-node:20-alpine job that exercises init/write-decision/why/status/lint/mcp on the published binary.
- Confirmed musl targets need no musl-tools: Cargo.toml [dependencies] is empty (std-only), so rustc self-links bundled musl; arm64 targets build on native arm runners (ubuntu-24.04-arm), darwin-x64 on macos-15-intel — all 7 targets are buildable as configured.
- Verified import.meta.dirname dependency is pervasive across all tools and is the basis for the >=20.11 engine floor, which npm does not enforce by default.

### F83. [MEDIUM] Postinstall crashes cryptically on Node <20.11 because engines is advisory but tooling hard-requires import.meta.dirname

`operational` · location `tools/install-native.mjs:10 (also native-targets-consuming tools); package.json engines.node ">=20.11"` · reporter-confidence high · verification **CONFIRMED**

**Description:** Every tool resolves its root via `path.resolve(import.meta.dirname, "..")`. `import.meta.dirname` only exists on Node >=20.11.0 (and >=21.2.0). package.json declares engines.node ">=20.11", but npm does not enforce engines by default (engine-strict is off), so a user on Node 18.x or Node 20.0–20.10 can run `npm i -g @jalkarna/archiva`; the postinstall then evaluates `path.resolve(undefined, "..")`, which throws a bare TypeError with no friendly guidance, failing the whole global install.

**Why it matters:** Node 18 LTS is still widely installed. The failure is during postinstall with a raw stack trace, not the clear messaging the rest of the install path is careful to provide, so users cannot tell that the fix is simply upgrading Node.

**Impact:** Hard `npm i -g` failure with an opaque TypeError for users below the engine floor; support burden and a poor first-run impression.

**Likelihood:** Medium — Node 18/early-20 are common in the wild and engines is not enforced unless the user sets engine-strict.

**Evidence (reporter):** grep shows `import.meta.dirname` used in install-native.mjs:10 and every other tool. validate-native-package-metadata.mjs:325 asserts the engines string verbatim ("...for import.meta.dirname tooling"), confirming the dependency is known. Node docs: import.meta.dirname added in 20.11.0/21.2.0. No runtime Node-version guard exists in install-native.mjs before the import.meta.dirname use.

**Independent verification:** package.json:64 sets "postinstall":"node tools/install-native.mjs". tools/install-native.mjs:10 evaluates `path.resolve(import.meta.dirname, "..")` at module top level (imports on lines 1-8 plus native-targets.mjs use no import.meta.dirname — confirmed by grep, so line 10 faults first). No runtime Node-version guard exists before line 10 (grep for process.version/nodeVersion/20.11/engines in install-native.mjs finds nothing). On Node <20.11 import.meta.dirname is undefined; reproduced the exact fault: `node -e 'path.resolve(undefined,"..")'` -> "TypeError - The \"paths[0]\" argument must be of type string. Received undefined" (bare TypeError, no guidance). engines is advisory: no .npmrc and no engine-strict anywhere in the repo (grep empty), so npm does not block install on engines.node ">=20.11". validate-native-package-metadata.mjs:325 asserts engines === ">=20.11" "for import.meta.dirname tooling", confirming the dependency is a deliberate, known floor. Caveat: live `npm i -g` on a sub-20.11 Node was not run (env is Node v24.8.0); the crash is reproduced at the expression level, but the path is unconditional/unguarded so the outcome is deterministic.

**Verifier notes / severity correction:** Claim is accurate as written, including the file:line locations and the mechanism (advisory engines + unguarded import.meta.dirname use). Severity medium stands: it is a first-run global-install failure with an opaque TypeError for users below the engine floor when engine-strict is off (npm default), not a security or data-integrity defect. Recommended fix: add an explicit Node-version check at the very top of install-native.mjs (read process.versions.node, compare >=20.11.0) that exits with a clear message, OR replace import.meta.dirname with fileURLToPath(new URL("..", import.meta.url)) which works on all Node 18+. The fileURLToPath approach is preferable since it removes the dependency entirely and could let the engine floor drop. The same import.meta.dirname pattern appears in 6 other tools (npm-publish-idempotent, audit-v2-completion, stage-native-package, write-dist-bin-shim, smoke-native-package, validate-native-package-metadata), but those are build/CI tools not run during end-user install, so only install-native.mjs is on the end-user crash path.

**Recommended resolution:** Add an explicit Node-version preflight at the top of install-native.mjs (check process.versions.node and print a clear 'Archiva requires Node >=20.11; you have X' message + exit), or replace `import.meta.dirname` with `path.dirname(fileURLToPath(import.meta.url))` (works on all Node 18+). The latter removes the floor entirely for the install path.

---

### F84. [LOW] README undersells --omit=optional as 'prevents selection' when it is actually a hard install failure

`operational` · location `README.md:75-77; tools/install-native.mjs:90-110 (resolveNativePackage throws, rethrown when not a source checkout)` · reporter-confidence high · verification **CONFIRMED**

**Description:** README says `--omit=optional` or `--ignore-scripts` 'prevent the native binary from being selected', implying a soft/no-op. Reality differs by flag: with `--ignore-scripts`, postinstall is skipped and the committed placeholder shim (dist-native/archiva.exe) runs and prints a clear error with exit 1 (graceful). With `--omit=optional`, postinstall DOES run, resolveNativePackage throws because the optional package is absent, and (outside a source checkout) it rethrows, so the entire `npm i -g` aborts non-zero with the 'Reinstall ... without --omit=optional' message.

**Why it matters:** The two flags produce materially different UX (one yields a usable-but-erroring command, the other aborts the install). Both are clearly messaged, but the README conflates them.

**Impact:** Low — both failure modes are clear and actionable; this is documentation accuracy, not broken behavior.

**Likelihood:** Low.

**Evidence (reporter):** Ran the committed shim: prints 'Archiva native binary was not installed. Reinstall without --ignore-scripts and with optional dependencies enabled.' exit=1. install-native.mjs:104-110 throws with the --omit=optional guidance; isSourceCheckout() (lines 113-120) only suppresses it in a Cargo.toml-bearing non-node_modules tree. Confirmed the source-checkout skip fires locally for an unselected target (darwin-arm64).

**Independent verification:** Verified all three legs of the claim by reading source and running install-native.mjs in scratch dirs.

1) README wording (README.md:75-77, read directly): "Install with optional dependencies and lifecycle scripts enabled. npm options such as `--omit=optional` or `--ignore-scripts` prevent the native binary from being selected." This frames both flags identically as soft "not selected", with no mention that one aborts the install.

2) --ignore-scripts path (graceful, exit 1): bin maps to dist-native/archiva.exe (package.json bin.archiva), and stage-native-package.mjs:186-192 writes a committed Node placeholder shim that does `console.error(placeholderMessage); process.exit(1)`. Ran the equivalent shim: prints "Archiva native binary was not installed. Reinstall without --ignore-scripts and with optional dependencies enabled." exit=1. So with --ignore-scripts, postinstall is skipped, the shim runs on `archiva` invocation, clear error, no install abort.

3) --omit=optional path (hard install failure). install-native.mjs:106-118 always runs at postinstall; resolveNativePackage (85-95) throws "Missing optional native package ... Reinstall @jalkarna/archiva without --omit=optional and without --ignore-scripts." when the optional package is absent. isSourceCheckout (97-104) only suppresses it when Cargo.toml exists AND path is not under node_modules.
- Source-checkout skip: in repo root (has Cargo.toml), `ARCHIVA_NATIVE_TARGET=darwin-arm64 node tools/install-native.mjs` -> "Skipping Archiva native package selection in source checkout: ..." exit=0. Confirms the local suppression fires for an unselected target.
- Installed-tree rethrow: copied install-native.mjs + native-targets.mjs into /tmp/fakeinstall/node_modules/@jalkarna/archiva (no Cargo.toml). `ARCHIVA_NATIVE_TARGET=darwin-arm64 node tools/install-native.mjs` -> uncaught Error rethrown, "Node.js v24.8.0" crash, exit=1. This is exactly what `npm i -g` would surface: a non-zero postinstall aborting the whole global install.

So the two flags behave differently: --ignore-scripts is graceful (shim at runtime), --omit=optional aborts the install at postinstall. README's "prevent the native binary from being selected" undersells the --omit=optional case.

**Verifier notes / severity correction:** Claim is accurate in mechanism, location, and severity. One nuance worth recording: both failure messages are clear and actionable (the --omit=optional message even names the exact remediation), so this is genuinely a documentation-accuracy issue, not a functional defect — low severity is correct. The --omit=optional abort is also arguably the *safer* failure mode (fails loudly at install rather than leaving a non-functional binary on PATH), so the README inaccuracy is the only real issue: it implies a benign no-op when one flag actually hard-fails the global install. Recommended fix matches the claim: split the sentence so --ignore-scripts is described as "leaves a placeholder that errors at runtime" and --omit=optional as "causes the install to fail".

**Recommended resolution:** Reword the README to state the two outcomes explicitly: `--ignore-scripts` leaves a command that errors with instructions; `--omit=optional` fails the install. Both are intentional.

---

### F85. [LOW] macOS native binaries are unsigned and un-notarized

`operational` · location `.github/workflows/publish.yml publish-native (darwin-x64/darwin-arm64 jobs); tools/stage-native-package.mjs (no codesign/notarize step)` · reporter-confidence medium · verification **CONFIRMED**

**Description:** The darwin packages ship a raw `cargo build --release` binary with no `codesign` (beyond rustc's default ad-hoc arm64 signature) and no notarization step. Distribution via npm avoids the com.apple.quarantine xattr (which only browser-downloaded files get), so the CLI does execute, but the binaries carry no Developer ID identity or notarization ticket.

**Why it matters:** Acceptable for an npm-delivered CLI today, but if a user ever obtains the binary outside npm (manual download of the tarball), Gatekeeper will quarantine it, and some enterprise security tooling flags unsigned Mach-O.

**Impact:** Low — npm install path works as-is.

**Likelihood:** Low.

**Evidence (reporter):** No `codesign`/`notarytool`/`xcrun` references in any tool or workflow (grep over tools/*.mjs and .github/workflows). The darwin jobs build and stage with the same generic path as Linux.

**Independent verification:** grep -rniE "codesign|notarytool|notarize|xcrun|com.apple.quarantine|developer id" over /home/ubuntu/archaeo/tools/ and /home/ubuntu/archaeo/.github/workflows/ returns NO matches (EXIT 1, zero hits). publish.yml publish-native matrix defines darwin-x64 (macos-15-intel, x86_64-apple-darwin, line 353-356) and darwin-arm64 (macos-15, aarch64-apple-darwin, line 357-360). All targets — including darwin — run the identical generic pipeline: "cargo build --release --locked --target ..." (line 386) then "node tools/stage-native-package.mjs" (line 389). No platform-specific signing/notarization branch exists. tools/stage-native-package.mjs copyExecutable() (lines 51-57) only does fs.copyFile + fs.chmod(0o755 for non-win32); grep across the whole file shows zero sign/notar/xcrun references. So darwin binaries ship as raw cargo output (arm64 gets rustc's default ad-hoc signature; x64 unsigned), with no Developer ID identity and no notarization ticket.

**Verifier notes / severity correction:** Claim is accurate in every particular: no codesign/notarytool/xcrun anywhere, darwin jobs use the same generic build+stage path as Linux, and stage-native-package.mjs does only copy+chmod. Severity low is correct: npm-distributed binaries do not receive the com.apple.quarantine xattr (that is applied by Gatekeeper only to files marked as downloaded via LSFileQuarantineEnabled apps like browsers), so npm-installed binaries execute without a Gatekeeper prompt. The residual operational risk is real but minor: (1) any user who downloads a darwin binary outside npm (e.g. a GitHub release artifact, if ever published) would hit Gatekeeper; (2) absence of a notarization ticket / Developer ID means no hardened-runtime guarantees and reduced supply-chain provenance. Note the claim's parenthetical about arm64 ad-hoc signing is correct — rustc/ld default-codesigns aarch64-apple-darwin binaries ad-hoc, which is required for them to run at all on Apple Silicon, but ad-hoc is not a Developer ID identity. Verdict CONFIRMED.

**Recommended resolution:** Document that the binary is unsigned, or add optional Developer ID signing + notarization for the darwin targets if non-npm distribution is anticipated.

---

## DIMENSION: API/CLI/Behavioral Consistency and Developer Experience  — score 8/10

> Archiva exposes the same core operations via three entry points (CLI, MCP stdio, Claude Code hooks). The core orchestration in src/core/project.rs is shared, so write/why/ghost_check produce byte-identical .dlog/.dmap output and identical validation error text regardless of entry point — I verified this directly by running both paths in isolated /tmp projects. Path normalization (./, .//, backslash) is consistent across CLI and MCP. The main weaknesses are not divergences within an operation but (a) a complete absence of any schema-evolution/migration story (schema:1 is hardcoded; a single forward-version file hard-fails every whole-project command), (b) silent dropping of unknown fields on every rewrite (forward-compat data loss), (c) the auto-wired PostToolUse re-anchor hook being a no-op under real Claude Code, and (d) a feature asymmetry where CLI why supports line lookup but MCP why does not. Overall the per-operation consistency is genuinely strong; the gaps are in format-stability planning and the hook onboarding path. Note: an early apparent "disappearing dlog" was traced to a second audit agent sharing /tmp/archiva-audit, not an archiva bug — all findings below were re-verified in unique scratch dirs.

*Score rationale:* Per-operation consistency across CLI, MCP, and hooks is genuinely strong: shared core (project.rs) yields byte-identical .dlog/.dmap output, identical validation error text, and identical path normalization across entry points, all verified by running both paths. Help text, exit-code conventions, and flag handling are coherent. Points are deducted for the absence of any schema-evolution story (one forward-version file blinds whole-project commands), silent unknown-field data loss on rewrite, and the auto-wired PostToolUse hook being a no-op under real Claude Code — the last is a real onboarding defect for the tool's headline workflow. The why line-lookup asymmetry between CLI and MCP and several smaller inconsistencies round out a solid-but-not-release-perfect picture.

**Verified behaviors (checked, not assumed):**

- Wrote the same decision shape via CLI write-decision and via MCP write_decision in isolated /tmp repos; resulting .dlog and .dmap files are structurally identical (same field order, same fingerprint format, same 'L-L:anchor' .dmap line).
- write_decision validation errors are byte-identical across CLI and MCP: missing 'because' -> 'because: missing required field'; '../x.ts' -> 'Invalid project-relative path "../x.ts": parent path segments are not allowed'; non-existent anchor -> identical 'Anchor ... does not exist ... Available anchors: ...' message.
- Path normalization parity: './/src/...' on write and 'src\\path.ts'/'.\\src\\path.ts' on read/why/ghost_check all resolve to the same decision identity via both CLI and MCP.
- arc/supersede only fires on the SECOND lint/ghost_check after STALE is persisted; verified this is identical for CLI lint and MCP ghost_check (first run: arc/stale only; second run: arc/stale + arc/supersede) — the earlier apparent divergence was first-vs-second-run ordering, not an entry-point difference.
- schema:2 .dlog causes 'schema: expected schema version 1' and exit 1 for why AND for whole-project status/lint/session-start, hiding 5 valid sibling decisions; removing the file restored all commands.
- post-tool-use rewrite dropped injected unknown fields future_field/confidence from the .dlog (silent data loss).
- MCP why ignores a 'line' field and treats numeric 'anchor' as a literal name; CLI why src/g.ts 2 performs line lookup — confirmed asymmetry.
- Claude-Code-style PostToolUse stdin payload ('{tool_input:{file_path:...}}') is ignored by `archiva hooks post-tool-use`; it exits 1 with 'Missing file path' unless ARCHIVA_FILE is set or a positional arg is passed.
- CLI why regenerates no .dmap after `rm *.dmap`; MCP ghost_check (and status/session-start) do regenerate it.
- Onboarding loop (init -> empty status/session-start -> first write-decision -> why -> status) works cleanly with sensible empty-state messages and exit 0; init is idempotent. lint exits 1 on error issues (arc/stale) and 0 on warning-only (arc/orphan); unknown command/option errors go to stderr with exit 1.
- --version prints 0.2.0; MCP initialize reports protocolVersion 2024-11-05 and serverInfo name 'archiva'; tools/list = [write_decision, why, ghost_check] (no history/status/lint over MCP).

### F86. [HIGH] Auto-wired PostToolUse re-anchor hook is a no-op (and errors) under real Claude Code

`operational` · location `src/core/settings.rs:5 & init template; src/cli.rs:234-258; src/main.rs:46-51 (should_read_stdin)` · reporter-confidence high · verification **CONFIRMED**

**Description:** `archiva init` wires PostToolUse to `archiva hooks post-tool-use` with no file argument, relying on the ARCHIVA_FILE env var. But Claude Code delivers the edited file via a JSON payload on stdin (tool_input.file_path), and main.rs only reads stdin for write-decision — never for post-tool-use. ARCHIVA_FILE is not set by Claude Code, so the hook fails on every edit.

**Why it matters:** Re-anchoring after edits (keeping lines_hint/fingerprint current, marking drift) is the core automation the quick-start sells. As wired by init, it never runs in the documented environment; it emits an error on every Write/Edit/MultiEdit.

**Impact:** Following the README quick-start verbatim yields a PostToolUse hook that prints 'Missing file path. Pass one or set ARCHIVA_FILE.' and exits 1 on every edit, surfacing a recurring error to the user while silently never re-anchoring. Decisions drift undetected until a manual lint.

**Likelihood:** High — affects the default, documented Claude Code setup the tool is built for.

**Evidence (reporter):** Scratch repo: `printf '{"tool_name":"Edit","tool_input":{"file_path":"src/st.ts"}}' | archiva hooks post-tool-use` -> 'Missing file path. Pass one or set ARCHIVA_FILE.' exit 1. Bare `archiva hooks post-tool-use` also exits 1. Only `ARCHIVA_FILE=src/st.ts archiva hooks post-tool-use` succeeded. main.rs:47 restricts stdin reading to write-decision.

**Independent verification:** Read src/core/settings.rs:4-6 — init wires PostToolUse to the bare command "archiva hooks post-tool-use" with matcher "Write|Edit|MultiEdit", NO file argument and NO env templating. Generated /tmp/archiva_audit/.claude/settings.json confirms: PostToolUse hook command is literally "archiva hooks post-tool-use" with no arg.

Read src/main.rs:46-51 — should_read_stdin() returns true ONLY when args[0]=="write-decision". post-tool-use never reads stdin, so a Claude Code JSON payload on stdin is ignored entirely.

Read src/cli.rs:234-258 — run_post_tool_use takes the file from the first positional arg, else falls back to env::var("ARCHIVA_FILE"), else errors "Missing file path. Pass one or set ARCHIVA_FILE."

Reproduction in /tmp scratch repo (git init + archiva init, exit 0):
- printf '{"tool_name":"Edit","tool_input":{"file_path":"src/st.ts"}}' | archiva hooks post-tool-use -> "Missing file path. Pass one or set ARCHIVA_FILE." exit=1
- bare `archiva hooks post-tool-use </dev/null` -> same message, exit=1
- ARCHIVA_FILE=st.ts archiva hooks post-tool-use -> "No decisions for st.ts; nothing to re-anchor." exit=0

Every claimed observation matches. The stdin payload that Claude Code sends (tool_input.file_path) is silently discarded; the only working invocations are the env var or an explicit positional arg, neither of which the auto-wired settings.json or Claude Code supplies.

**Verifier notes / severity correction:** Claim is accurate on all technical points. Two refinements, neither of which lowers the verdict:

1) The phrase "surfacing a recurring error to the user" depends on Claude Code's exit-code semantics. The hook exits 1 (not 2). In Claude Code, a non-zero PostToolUse exit other than 2 is a non-blocking error: stderr is shown to the user but is NOT fed back to the model and does not block the edit. So the user does see a recurring "Missing file path..." error after every Write/Edit/MultiEdit, but the agent's workflow is not interrupted. The core functional defect — the re-anchor/drift-detection hook never actually runs against the edited file — is fully confirmed: decisions silently drift until a manual `archiva lint`.

2) The defect is in the auto-wiring path (settings.rs template + main.rs stdin gating), not in the hook logic itself, which works correctly when given a path. README:202-204 documents the two working manual forms (ARCHIVA_FILE=... or positional path), but the auto-wired hook uses neither, and Claude Code provides neither automatically.

Recommended resolution: parse the PostToolUse JSON payload from stdin (extract tool_input.file_path) when running `hooks post-tool-use` — i.e. extend should_read_stdin to include post-tool-use and have run_post_tool_use fall back to the stdin payload before erroring. Keep the positional/env paths for manual use. This is the only change that makes the documented, auto-wired drift-detection feature actually function under real Claude Code.

**Recommended resolution:** Make post-tool-use parse the Claude Code hook stdin JSON (tool_input.file_path) as a fallback when no positional/env arg is present, or change the init template to a wrapper that maps the payload to ARCHIVA_FILE. Add an integration test that feeds a realistic hook payload.

---

### F87. [MEDIUM] No schema migration story: a single forward-version .dlog hard-fails every whole-project command

`architecture` · location `src/core/dlog.rs:66-71; src/core/version.rs:8` · reporter-confidence high · verification **CONFIRMED**

**Description:** parse_dlog_value rejects any schema != DLOG_SCHEMA_VERSION (1) with a hard error. Because lint/status/session-start iterate all .dlog files and propagate the first parse error, one file written by a future archiva (schema:2) aborts the entire command and hides every valid decision in the repo. There is no migration path, no schema:2 reader, and no forward/backward negotiation.

**Why it matters:** This is the public format-stability question directly in scope. Decision files are committed to the repo and shared across a team. As soon as anyone upgrades archiva and it bumps the schema, every teammate on the old binary gets total command failure, not graceful degradation. There is no documented or coded plan for schema:2.

**Impact:** Mixed-version teams or CI on an older pinned binary see 'schema: expected schema version 1' and exit 1 for status/lint/session-start across the whole project, masking all healthy decisions. Forward rollout of any data-model change is effectively a breaking change with no upgrade ramp.

**Likelihood:** Certain the moment the schema is ever bumped; the codebase already contains the constant that will trigger it.

**Evidence (reporter):** In a unique scratch repo I placed a valid schema:2 .dlog alongside 5 valid schema:1 files. `archiva why src/v2.ts fn:v2` -> 'schema: expected schema version 1' exit 1. `archiva status`, `archiva lint`, `archiva hooks session-start` ALL printed the same error and exited 1, even though a.ts/d.ts/e.ts/g.ts/o.ts decisions were valid. Removing the one schema:2 file restored all commands.

**Independent verification:** MECHANISM CONFIRMED by reading source and reproducing at runtime.

Code: src/core/dlog.rs:66-71 — parse_dlog_value hard-rejects any schema != 1: `if schema != DLOG_SCHEMA_VERSION { return Err(ArchivaError::schema("schema", format!("expected schema version {DLOG_SCHEMA_VERSION}"))); }`. src/core/version.rs:8 defines `DLOG_SCHEMA_VERSION: u32 = 1`. grep for migrat|upgrade|forward.?compat|schema_version found ZERO migration logic; the constant has 13 non-test references, all either equality-check or write-time stamping. No schema:2 reader, no negotiation, no skip/warn path.

Propagation: all whole-project commands funnel each file through load_dlog -> parse_dlog_yaml and propagate the first error via `?`:
- session_start (project.rs:69-85) loops list_dlog_files and calls load_dlog(...)? per file.
- lint_project_inner (project.rs:123-162, used by both `lint` and `status` via lint_project_issue_count) loops dlog_files and calls lint_dlog_locked(...)? per file.
- why/history (project.rs:38-52) load the single named file.

RUNTIME REPRO in /tmp/archiva-schematest (git-init'd, `archiva init`): wrote 5 valid schema:1 decisions (src/a,d,e,g,o.ts via `write-decision --json` with lines as a 2-element array [1,3]). Baseline status/lint/session-start all exit 0 and list all 5. Then hand-created .decisions/src/v2.ts.dlog with schema:2 (otherwise valid) plus matching .dmap. Result, each command printed exactly `schema: expected schema version 1`:
- `archiva why src/v2.ts fn:v2` -> exit 1
- `archiva status` -> exit 1 (masked all 5 healthy files)
- `archiva lint` -> exit 1
- `archiva hooks session-start` -> exit 1
- `archiva why src/a.ts fn:a` (single healthy file, not iterating) -> exit 0, still works.
Deleting the schema:2 file restored status/lint/session-start to exit 0 listing all 5 decisions. This matches the claimed evidence exactly.

ADDITIONAL DEFECT not in the claim: the error string contains no file path. For status/lint/session-start the user sees only `schema: expected schema version 1` with no indication of WHICH .dlog caused it, making diagnosis materially harder.

**Verifier notes / severity correction:** The mechanism, blast radius (one forward-version file aborts every whole-project command and masks all healthy decisions), and the absence of any migration/negotiation path are all CONFIRMED — the claim's technical description and reproduction are accurate.

I am adjusting severity from high to medium, and the distinction matters: this is a forward-compatibility DESIGN GAP, not an active defect that current users can hit through normal use. There is no schema:2 producer in this release (DLOG_SCHEMA_VERSION = 1, and only archiva writes .dlog files), so today the only way to create a schema:2 file is hand-editing or a future binary that does not yet exist. The risk is conditional on a future schema bump that the team itself controls the rollout of. That keeps it below high.

It is clearly above low, for three reasons: (1) it is cross-cutting — it disables status, lint, and session-start simultaneously, not one command; (2) session-start is the core agent-hook value path, and an all-or-nothing failure there silently strips ALL decision memory the moment one forward file appears; (3) forward-compat tolerance cannot be retrofitted into already-deployed v1 binaries — once v1 ships without a skip-with-warning path, any future schema:2 rollout is a hard breaking change for every pinned/older client and for mixed-version teams (one dev upgrades and commits schema:2, CI on the old pinned binary breaks repo-wide).

Recommended resolution: in the whole-project iteration paths (session_start, lint_project_inner, load_project_status_summaries), treat an unknown/newer schema as a skippable per-file diagnostic (warn + continue, surfacing healthy files) rather than a propagated hard error; at minimum, include the offending file path in the schema error message so single-file commands and any future failures are diagnosable. Decide a forward-compat policy (newer-than-known => skip-with-warning; older-than-known => migrate or read-compat) before the first schema bump ships.

One design-tradeoff caveat worth stating honestly: an old binary REFUSING to parse newer data is partly correct — silently misreading future fields would be worse. The defect is not the refusal itself but (a) the all-or-nothing propagation that hides unrelated healthy files, and (b) the file-less error message.

**Recommended resolution:** Define a versioning policy before 1.0: (1) accept schema <= current and migrate-on-read, or (2) at minimum skip-with-warning unknown future-schema files in whole-project commands instead of aborting, so one forward file cannot blind the entire project. Document the schema:2 evolution plan.

---

### F88. [MEDIUM] Unknown/forward-compatibility fields are silently dropped on every .dlog rewrite (data loss)

`defect` · location `src/core/dlog.rs:88-127 (parse_decision_record) and 214-253 (decision_to_yaml)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The YAML reader only extracts known fields into DecisionRecord, and the writer re-emits only those known fields. Any additional keys present in a decision record (e.g. fields a future archiva or an external tool added) are lost the next time the file is rewritten by post-tool-use, lint --fix, or ghost_check.

**Why it matters:** Combined with the hardcoded schema check, this removes the usual safety valve for forward compatibility: even if the schema gate were relaxed, round-tripping a file would still erase newer data. Decision files are the authoritative store; silent loss of fields is a correctness/durability issue.

**Impact:** A newer client's extra metadata (or a hand-added annotation) vanishes after any older-or-equal client performs a routine re-anchor, with no warning. Loss is irreversible once the file is committed.

**Likelihood:** High whenever files are touched by more than one version or any external augmentation exists; routine post-tool-use rewrites trigger it.

**Evidence (reporter):** In a scratch repo I added `future_field: keep_me` and `confidence: 0.9` to a valid record, then ran `archiva hooks post-tool-use src/o.ts`. The rewritten .dlog dropped both fields (only status/stale_since were added). Reproduced verbatim.

**Independent verification:** Read src/core/dlog.rs:8-51 (struct defs), :88-127 (parse_decision_record), :198-253 (dlog_to_yaml/decision_to_yaml). Neither DecisionRecord nor DlogFile carries any catch-all/extra-fields map; the parser extracts only the ~13 named keys, and the writer rebuilds a fresh YamlObject from only those named fields. There is no path that could round-trip unknown keys.

Reproduced against the release binary in /tmp/archiva_audit (git-init + `archiva init`):
1. Wrote a valid decision (`write-decision --json` with anchor export:order) → .decisions/src/o.ts.dlog created.
2. Hand-edited the dlog to add a top-level key `vendor_extension: top_level_keep` and two per-decision keys `future_field: keep_me` and `confidence: 0.9` (verified present via cat).
3. Ran `archiva hooks post-tool-use src/o.ts` → output "Re-anchored src/o.ts: 0 stale, 0 orphan."
4. cat of the rewritten dlog: ALL three unknown keys gone (vendor_extension, future_field, confidence). Only the known schema remained.

Crucially, src/core/project.rs:266-330 shows post_tool_use ALWAYS calls write_dlog + write_dmap (line 328-329) after load_dlog succeeds, even on a pure no-op pass (my repro: 0 stale / 0 orphan, no field mutated). So the rewrite — and thus the field drop — is unconditional on any tracked file touched by the hook, not gated on an actual change. Same applies to write_decision (project.rs:545) and any other load→write path. The drop is silent (no warning emitted) and the file is overwritten in place, so it is irreversible once committed.

**Verifier notes / severity correction:** The claim is accurate in mechanism, evidence, and impact. One nuance worth recording for ranking: today no producer (this binary, schema v1) ever emits extra fields, so the only sources of loss are (a) a hypothetical newer/forward-compat client and (b) hand-added human annotations in the .dlog — both real but not exercised by the current shipping system. The drop is unconditional (fires even on no-op re-anchors with 0 stale/0 orphan), silent, and irreversible, which keeps it at medium rather than low. The claim's specific phrasing "only status/stale_since were added" is incidental to its run; in my run nothing was added yet the fields were still dropped — same root cause, the unconditional rewrite. Recommended resolution: capture unrecognized keys into an `extra: BTreeMap<String, YamlValue>` on DecisionRecord/DlogFile during parse and re-emit them in decision_to_yaml/dlog_to_yaml (round-trip preservation), or at minimum emit a warning when unknown keys are encountered so loss is not silent.

**Recommended resolution:** Preserve unrecognized keys (capture into a passthrough map and re-emit on render) or explicitly version-gate and refuse to rewrite files containing unknown fields rather than silently discarding them.

---

### F89. [MEDIUM] MCP why cannot do line-based lookup that CLI why supports; line argument silently ignored

`defect` · location `src/cli.rs:147-159 vs src/mcp.rs:118-123 & 172-181 (why_tool_arguments_from_json)` · reporter-confidence high · verification **CONFIRMED**

**Description:** CLI why treats an all-digit second positional as a line number and calls why_for_line. The MCP why tool only accepts file+anchor; a numeric value passed as `anchor` is treated as a literal anchor name (miss), and an extra `line` field is silently dropped, returning whole-file results instead of erroring.

**Why it matters:** The README documents line lookup (`archiva why src/auth/session.ts 52`) as a first-class read mode. Agents calling the MCP why tool — the primary agent interface per the README — cannot reach the same behavior, and get no error signaling the unsupported mode.

**Impact:** An agent that asks 'why does line 52 exist' via MCP either gets a misleading 'No decision found ... at 52' or silently gets every decision in the file, with no indication line lookup is unavailable over MCP.

**Likelihood:** Medium; line-oriented questions are natural for agents editing specific spans.

**Evidence (reporter):** Scratch repo with src/g.ts (fn:gamma lines 1-3). CLI `why src/g.ts 2` returned the decision. MCP why arguments {file,anchor:'2'} -> 'No decision found for src/g.ts at 2.'; MCP why {file,line:2} and {file,line:99999} BOTH returned the full fn:gamma decision (line field ignored).

**Independent verification:** Code read + binary reproduction in /tmp/archtest (git repo, src/g.ts with fn:gamma lines 1-3, dec_001 written for anchor fn:gamma lines [1,3]).

CODE:
- CLI (src/cli.rs:147-159): why's second positional, when non-empty and all-ASCII-digits, parses to u32 and calls project::why_for_line(); otherwise treats it as an anchor. So a numeric second arg = line lookup.
- MCP schema (src/mcp.rs:542-563): why_tool() inputSchema declares only properties {file, anchor}, required [file]. No `line` property exists.
- MCP parser (src/mcp.rs:172-181): why_tool_arguments_from_json reads ONLY required `file` and optional `anchor`; any `line` field is never read. Handler (src/mcp.rs:118-123) calls project::why(.., input.anchor.as_deref()) — never why_for_line. There is no line code path in MCP at all.
- project::why_for_line exists (src/core/project.rs:44) but is reachable only from the CLI.

CLI RUNS:
- `archiva why src/g.ts 2` -> returns dec_001 (line lookup works).
- `archiva why src/g.ts 99999` -> "No decision found for src/g.ts at line 99999."

MCP RUNS (echoed JSON-RPC into `archiva mcp`):
- arguments {file:"src/g.ts", anchor:"2"} (id=2) -> "No decision found for src/g.ts at 2." (numeric value treated as literal anchor name -> miss, misleading message).
- arguments {file:"src/g.ts", line:2} (id=3) -> FULL fn:gamma dec_001 decision (line silently dropped, whole-file result).
- arguments {file:"src/g.ts", line:99999} (id=4) -> IDENTICAL full fn:gamma dec_001 decision (line silently dropped; CLI would say "No decision at line 99999").
- arguments {file:"src/g.ts"} (id=5) -> identical full result, confirming id=3/id=4 are just the whole-file path regardless of line.

**Verifier notes / severity correction:** Claim is accurate in every particular: numeric `anchor` over MCP yields a misleading "No decision found ... at 2"; `line` is silently dropped so MCP why with line=2, line=99999, and no line all return identical whole-file results. why_for_line is CLI-only. This is a genuine capability/DX asymmetry between the two front-ends, and the primary MCP consumer (an AI agent reasoning about "why does line N exist") is exactly the use case the CLI supports but MCP cannot. Medium severity is right: it is a missing-feature/inconsistency, not data corruption or a crash, and there is a usable workaround (pass the anchor name over MCP, or get whole-file results). One nuance worth noting for the report: the MCP schema never advertises a `line` field, so a strictly schema-following agent would not pass one — the misleading-miss path (numeric anchor) is the more realistic trigger than the silently-dropped-line path, though both are confirmed reproducible. Recommended resolution: either (a) add a `line` property to the why inputSchema and route it to why_for_line in the handler, or (b) reject unknown/numeric arguments with an explicit error rather than silently returning whole-file results, so behavior is consistent and self-documenting across CLI and MCP.

**Recommended resolution:** Either add a documented `line` field to the MCP why inputSchema wired to why_for_line, or document the asymmetry explicitly. At minimum, reject unknown lookup intents rather than silently returning whole-file output.

---

### F90. [LOW] .dmap repair is asymmetric: ghost_check/status/session-start regenerate a missing/corrupt .dmap, CLI why does not

`tradeoff` · location `src/core/project.rs:39-42 (why) vs 79, 405, 425 (ensure_dmap_current callers)` · reporter-confidence high · verification **CONFIRMED**

**Description:** session-start, status, and MCP ghost_check call ensure_dmap_current and rebuild a missing or corrupt .dmap as a side effect. CLI why (and history) read the .dlog directly and do not touch the .dmap, so a missing/corrupt .dmap is left unrepaired after a why.

**Why it matters:** The same conceptual 'read the decisions for this file' operation has different repository side effects depending on which command/entry point is used, which is mildly surprising and makes .dmap freshness depend on command history.

**Impact:** Low: why still returns correct answers from the authoritative .dlog; only the derivative index repair timing differs. No incorrect output observed.

**Likelihood:** Low; only matters when a .dmap is missing/corrupt and the user happens to run why rather than status/session-start.

**Evidence (reporter):** Scratch repo: after `rm src/r.ts.dmap`, `archiva why src/r.ts fn:r` did NOT recreate the .dmap; a subsequent MCP ghost_check did recreate r.ts.dmap.

**Independent verification:** Code path: project.rs:39-41 `why()` (and :49-51 `history()`) call only `load_dlog`, which (storage.rs:26-33) reads `.dlog`, parses YAML, and returns — never touching `.dmap` or calling ensure_dmap_current. By contrast ensure_dmap_current is invoked by session-start (project.rs:79), status (:405), write path (:425), load_dmap (storage.rs:64,71,83,91), and MCP ghost_check repairs the dmap for the requested file.

Empirical reproduction in /tmp/dmaptest (git-init'd, `archiva init`, one decision written for src/r.ts fn:r):
1. Missing-dmap case: `rm r.ts.dmap` then `archiva why src/r.ts fn:r` -> printed full correct decision (exit 0), `.dmap` NOT recreated (ls shows only r.ts.dlog). A subsequent MCP ghost_check on src/r.ts recreated r.ts.dmap (9 bytes "1-3:fn:r").
2. Corrupt-dmap case: wrote "GARBAGE NOT VALID DMAP @@@" into r.ts.dmap. `archiva history src/r.ts fn:r` (exit 0) and `archiva why src/r.ts fn:r` (exit 0) both left the garbage file unchanged. `archiva status` (exit 0) repaired it to "1-3:fn:r".

Both why and history return correct output from the authoritative .dlog regardless of dmap state; only the derivative index repair timing differs. The claim's location refs, mechanism, and impact assessment are all accurate.

**Verifier notes / severity correction:** Claim is accurate in every detail. Severity low/tradeoff is correct: the .dmap is an explicitly-derivative index and the .dlog is authoritative, so why/history never produce wrong output — they simply do not opportunistically repair the index the way the status/session-start/ghost_check/write paths do. One small clarification to the claim: `history` shares the identical non-repairing path as `why` (both go straight through load_dlog), so the asymmetry covers both read-only CLI lookup commands, not why alone. Reasonable design rationale exists for the asymmetry: why/history are pure read paths that intentionally avoid acquiring the decision-file lock and writing to disk, which keeps them side-effect-free and safe to run concurrently. If symmetric repair is desired, it could be added as a best-effort non-fatal repair, but this is a minor DX nicety, not a release blocker.

**Recommended resolution:** Either document why as a pure read (no repair) consistently, or have why opportunistically repair .dmap like the other readers. Consistency of side effects across read paths is the goal.

---

### F91. [LOW] Missing-source-file error leaks an absolute filesystem path, unlike all other repo-relative messages

`defect` · location `src/core/project.rs:211 (read_source_text) / fs read error formatting` · reporter-confidence high · verification **CONFIRMED**

**Description:** write_decision against a path whose source file does not exist returns 'Failed to read source file /tmp/.../src/nope.ts: No such file or directory', exposing the absolute host path. Every other user-facing message (path validation, anchor errors, why/lint output) uses repo-relative paths.

**Why it matters:** Inconsistent error vocabulary across the same command surface; absolute paths are noise for agents and leak environment details into committed logs/CI output.

**Impact:** Low: cosmetic/consistency. Could surface a user's home/CI directory layout in logs.

**Likelihood:** Medium; agents commonly attempt to record a decision before the file exists or with a typo'd path.

**Evidence (reporter):** Scratch repo: `archiva write-decision --json '{"file":"src/nope.ts",...}'` -> 'Failed to read source file /tmp/dx-305804-1782852046/src/nope.ts: No such file or directory (os error 2)'. Contrast: traversal error reports the relative "../x.ts".

**Independent verification:** Reproduced with the release binary. Scratch repo /tmp/dx-verify (git init + `archiva init`):

  $ archiva write-decision --json '{"file":"src/nope.ts","anchor":"fn:foo","lines":[1,3],"chose":"x","because":"y","rejected":[]}'
  Failed to read source file /tmp/dx-verify/src/nope.ts: No such file or directory (os error 2)   (exit 1)

The leaked path is the absolute canonicalized host path, not repo-relative. Confirmed it tracks the real filesystem layout, not cwd-relative: invoking from a subdir (/tmp/dx-abs/sub) still emits the absolute root-resolved path `/tmp/dx-abs/sub/src/nope.ts`.

Contrast — every other user-facing message I exercised stays repo-relative:
- Traversal: `Invalid project-relative path "../x.ts": parent path segments are not allowed`
- Missing anchor (existing file): `Anchor "fn:nonexistent" does not exist in src/real.ts. ... Available anchors in src/real.ts: export:foo, fn:foo.`
- why on missing file: `No decisions found for src/nope.ts.`

Code path traced: src/core/project.rs:224 write_decision_with_context -> :211/:622 read_source_text -> src/core/paths.rs:142 canonical_source_path_if_exists, which does `project_root.canonicalize()` then `root.join(relative)` and returns that absolute PathBuf when the file does not exist (paths.rs:154 early return before the escape check). read_text_file_with_limit (src/core/fs.rs:71) opens it, File::open fails NotFound, and the error is built via ArchivaError::io(Some(path), "read source file", ...). The Io variant is formatted at src/core/error.rs:89: `format!("Failed to {action} {}: {source}", path.display())` — the only user-facing variant that prints a raw absolute PathBuf. InvalidPath/Anchor/Cli variants (error.rs:96-107) print the relative input or message text.

**Verifier notes / severity correction:** Claim is accurate in substance and severity. Two precision notes:
1) Attribution: project.rs:211 is the call site, but the actual leak is structural in the `Self::Io` Display arm at src/core/error.rs:89 (uses `path.display()` on the absolute PathBuf produced by canonical_source_path_if_exists at paths.rs:142-155). The same Io variant would leak an absolute path from ANY filesystem read error (e.g. permission denied, too-many-open-files) on a source/dlog/git path, not just the missing-source-file case — so the fix should normalize at the Io formatting boundary (or carry a repo-relative display path), not just patch read_source_text.
2) Impact is correctly low: no privilege/data exposure, just host directory layout (home/CI paths) surfacing in stderr/logs, plus a UX inconsistency with the otherwise-uniform relative-path messaging. Recommended resolution: when an Io error originates from a project-relative operation, format with the RelativePath (or strip project_root before display) so the message reads `Failed to read source file src/nope.ts: ...`.

**Recommended resolution:** Format source-read failures with the repo-relative path (input.file.as_str()) to match the rest of the surface; drop the absolute path and raw os-error noise.

---

## DIMENSION: Overall Architecture and Maintainability  — score 8/10

> Archiva v2 is a std-only Rust re-engineering of a TS "decision memory" tool: one binary (init/why/history/lint/status/hooks/mcp/write-decision), repo-local .decisions/ storage with an authoritative YAML .dlog and a derivative .dmap index, a hand-rolled native git object reader (loose + packfile + DEFLATE, SHA-1/SHA-256), hand-rolled JSON and YAML parsers, and a 12k-line multi-language anchor parser, with ZERO Cargo dependencies. The architecture is genuinely well-layered: a clean dependency DAG rooted at error/paths/ordered_map, a single orchestration module (project.rs) that composes the leaf modules, a RelativePath newtype that centralizes path-safety validation at construction, and a dlog-authoritative/dmap-derivative model that self-heals on read. I verified the full lifecycle end to end (init, write-decision, why, status, MCP stdio initialize/tools-list/tools-call, hooks post-tool-use through both loose and packed git objects) and confirmed 301 lib tests pass and clippy is clean. The dominant architectural fact is the deliberate trade of dependency risk for maintenance burden: ~7k lines of hand-written TS/JS anchor extraction reimplementing ts-morph, ~2.6k lines of git plumbing including a from-scratch DEFLATE/Huffman inflater, and two bespoke parsers. This is defensible because TS extraction is continuously differentially tested against the retained TS oracle (src/*.ts + tools/archiva-differential.ts), but it concentrates almost all maintenance cost, bug surface, and contributor-onboarding friction in three large modules, and leaves several evolution paths (schema version, language extractors, MCP tools) as hardcoded multi-site edits rather than extension points. The system is solid and shippable for 0.2; the risks below are about the cost of evolving it to 1.0, not correctness defects.

*Score rationale:* The architecture is genuinely strong: a clean acyclic module graph rooted at error/paths/ordered_map, a single well-isolated orchestration hub (project.rs), a RelativePath newtype that centralizes path-safety at construction, a sound dlog-authoritative/dmap-derivative model that self-heals on read, disciplined production code (no user-reachable panics; 3 invariant-expects in git.rs), and a differential-oracle strategy that legitimately justifies the largest hand-rolled subsystem. Verified working end-to-end (CLI lifecycle, MCP stdio, packed+loose git reads) with 301 passing tests and clean clippy. Points off for evolution-readiness, not correctness: a hard-pinned single schema version with no migration path (the top barrier to 1.0), no registry abstraction for the two axes the tool is most likely to grow along (MCP tools, languages), MCP exposing under half the CLI's capabilities despite the logic already existing, and JS-parity quirks baked into production with no documented sunset. The std-only/zero-dep git+parser reimplementation is a defensible deliberate tradeoff, but it concentrates almost all maintenance cost and bug surface in three large modules and is the place where the purity buys the least. Solid 0.2 foundation; the named items are what stand between it and a maintainable 1.0.

**Verified behaviors (checked, not assumed):**

- Ran full decision lifecycle in /tmp/archiva_audit: init -> write-decision --json (fields chose/because/rejected/lines[start,end] required) -> .dlog (YAML schema:1, authoritative) + .dmap (compact '1-3:fn:toolTarget' derivative) written -> why returns the decision. Confirmed dlog is authoritative and dmap derivative.
- Drove MCP over stdio: initialize returns protocolVersion 2024-11-05; tools/list returns exactly 3 tools (write_decision, why, ghost_check); tools/call why returned the recorded decision text. Confirmed CLI exposes 8 commands but MCP only 3.
- Exercised the native git reader end to end: hooks post-tool-use on a committed file returned 'Re-anchored: 1 stale'; after `git gc` (forcing packfile path) it still worked, confirming both loose-object and packfile/DEFLATE read paths function.
- Measured scale empirically in /tmp/scale_test: per-write held ~46ms steady from 0..199 decisions on a 1000-function file (anchor-extraction dominated, not OrderedMap-dominated); status and lint across 200 dlog files each ran in ~0.43s. Confirmed O(n^2) OrderedMap is not a practical bottleneck at realistic scale.
- Confirmed Cargo.toml [dependencies] is empty (true zero-dep std-only); npm package.json ships only the native binary (files[]=dist-native/archiva.exe + docs/scripts), with ts-morph/typescript as devDependencies only.
- Confirmed the TS oracle is live infra, not dead code: tools/archiva-ts-baseline.ts imports extractAnchors/why/etc from src/core/*.js and npm 'differential' script compares the Rust binary against it; Rust/C extraction is native-only per README:437.
- Verified module dependency graph via import scan: leaf modules depend downward toward error/paths/ordered_map; project.rs is the sole hub importing ~15 modules. No cyclic cross-coupling among leaves.
- Confirmed production panic discipline: 0 production unwrap/expect/panic in anchor.rs, project.rs, storage.rs impl sections; only 3 invariant-assertion .expect() in git.rs (offset/length overflow guards), none user-input-reachable. unsafe is confined to fs.rs platform process-existence checks (kill/OpenProcess).
- Confirmed schema rigidity: dlog.rs:65-71 rejects any schema != DLOG_SCHEMA_VERSION (const 1) with no migration branch; verified 301 lib tests pass and clippy --release is clean.

### F92. [MEDIUM] Hand-rolled git plumbing with a from-scratch DEFLATE inflater is the highest-concentration bug/maintenance surface in the codebase

`tradeoff` · location `src/core/git.rs:1875 (zlib_inflate), :1324 (read_pack_object_at), :607-760 (packfile index), exposing only 2 public fns (find_git_root:134, read_git_head_file:153)` · reporter-confidence high · verification **CONFIRMED**

**Description:** git.rs is ~2.6k lines of implementation that reimplements: loose-object reading, full packfile support (v1/v2 index, fanout, OFS_DELTA/REF_DELTA resolution, pack trailer/index checksum validation), dual SHA-1/SHA-256 object formats, and a complete zlib/DEFLATE decompressor with stored/fixed/dynamic Huffman block decoding and adler32 verification. All of this exists to serve exactly one product need: reading 'HEAD:path' to get the pre-edit content of a file for the post-tool-use diff. I verified it works on both loose and packed objects (git gc then hooks post-tool-use succeeded). The concern is not current correctness but that DEFLATE and packfile delta resolution are notoriously edge-case-heavy (large objects, malformed streams, delta chains, format quirks across git versions), and this is the one subsystem where the std-only purity buys the least: gix/git2 are vetted, fuzzed, and track upstream git format evolution for free.

**Why it matters:** This module carries the highest ratio of complexity-to-product-value in the codebase. Any git format edge case the hand-rolled reader gets wrong manifests as a silently wrong diff baseline, which then produces wrong stale/orphan classifications in post-tool-use — the core value proposition. There is no fuzzing harness and no external oracle for the inflater (unlike the TS anchor path).

**Impact:** A malformed or unusual packfile/delta could produce a wrong HEAD baseline (post_tool_use falls back to new_content on error, masking failures as 'no change'), or a panic on an unhandled format. Maintenance cost is concentrated and requires deep git-internals knowledge to safely modify.

**Likelihood:** Low for common git repos (verified working); moderate over time as repos accumulate unusual pack states or as git format details drift.

**Evidence (reporter):** grep shows only find_git_root and read_git_head_file are pub in 4329-line file; zlib_inflate at git.rs:1875 implements inflate_stored_block/inflate_huffman_block/dynamic_huffman/adler32 by hand; Cargo.toml [dependencies] is empty; ran `git gc` in /tmp/scale_test then `archiva hooks post-tool-use src/f0.ts` -> 'Re-anchored ... 1 stale' (packfile path exercised, exit 0).

**Independent verification:** Every factual claim holds.

LINE NUMBERS (all exact): `wc -l src/core/git.rs` = 4329 (claim's "2.6k lines" refers to implementation only — first `#[cfg(test)]` test region starts at line 552 and the main `mod tests` is at 2618, so ~2617 lines of impl + ~1700 lines of tests; "~2.6k lines of implementation" is accurate). `grep "pub fn"` returns EXACTLY two public items in the whole file: find_git_root:134 and read_git_head_file:153 — no other pub fn/struct/enum. zlib_inflate at 1875; inflate_stored_block:1932; inflate_huffman_block:1949; dynamic_huffman:1978; adler32:2225; read_pack_object_at:1324; packfile index region (fanout read/validate, binary search) at 689-808 (claim cited 607-760; the index machinery is in that neighborhood — read_packed_git_object_from_dir:607, find_packed_object:631, fanout handling 689+). Cargo.toml `[dependencies]` section is EMPTY.

SUBSYSTEM SCOPE confirmed by reading source: dual SHA-1/SHA-256 formats (GitObjectFormat enum, GIT_SHA256_* consts, detection at 240-248); full packfile support incl. OFS_DELTA + REF_DELTA with apply_git_delta:1576 implementing copy/insert opcodes, varint sizes, delta-chain depth limit (1334), base-offset underflow check (1392); complete DEFLATE in zlib_inflate:1895 dispatching stored(0)/fixed-Huffman(1)/dynamic-Huffman(2)/reserved-error, with adler32 verification (1925-1930) and trailing-byte rejection (1917).

CURRENT CORRECTNESS reproduced: /tmp/gitscale — init git repo, archiva init, wrote a.ts + decision (dec_001, anchor fn:foo), committed, `git gc -q` (confirmed pack-*.pack created, loose objects gone), edited a.ts, `archiva hooks post-tool-use a.ts` -> "Re-anchored a.ts: 1 stale, 0 orphan." exit=0. The packfile inflate+baseline-read path is exercised and produces correct staleness detection.

MASKING-ON-ERROR claim CONFIRMED at src/core/project.rs:273: `let old_content = read_git_head_file(project_root, old_git_file).unwrap_or_else(|_| new_content.clone());` — any git read error (malformed pack, unhandled format, panic-free Err) silently substitutes new_content. Then line 274 `using_current_content_fallback = old_content == new_content` and line 275 `diff_lines(&old_content, &new_content)` yields an empty diff, so line-shift detection is suppressed. No logging/warning is emitted on the error path. (Note: fingerprint-based staleness at is_fingerprint_stale:318 still fires independently of the diff, so a git-read failure degrades line-range tracking but does not blind ALL detection — a mitigating nuance the claim slightly understates.)

**Verifier notes / severity correction:** CONFIRMED as stated; severity medium and category "tradeoff" are correct. This is an architectural/maintenance tradeoff, NOT a verified defect — current behavior is correct on both loose and packed objects (reproduced), with disciplined bounds checks throughout (delta range overflow, copy-range vs base, output-vs-target-size, fanout sorted/monotonic validation, adler32, depth limits). The std-only purity genuinely buys the least here: this one subsystem reimplements format-evolving, edge-case-heavy logic (DEFLATE, delta chains, dual hash formats) that vetted/fuzzed crates (gix/git2) maintain for free, all to serve a single product need (HEAD:path baseline for post-tool-use diffs). Two refinements to the claim: (1) the file is 4329 lines total, not 2.6k — the 2.6k figure is implementation-only and is accurate as such, worth stating precisely. (2) The error-masking impact is partially mitigated: a wrong/failed HEAD baseline silently disables LINE-SHIFT tracking (empty diff) but fingerprint staleness (project.rs:318) and orphan detection still run off new_content, so failures degrade rather than fully blind detection. Recommended resolution: keep the implementation (rewrite risk is high, current code is careful), but (a) log a diagnostic when read_git_head_file errors instead of silently falling back at project.rs:273, so masked git-read failures are observable rather than presenting as "no change"; (b) ensure a fuzz/corpus harness over the inflater and delta resolver exists to guard the concentrated edge-case surface.

**Recommended resolution:** Keep std-only if zero-dep distribution is a hard product requirement, but (a) add a fuzz/property harness for zlib_inflate and pack delta resolution against the system `git` as oracle, mirroring the TS differential strategy; (b) document in the architecture doc the explicit decision to forgo gix/git2 and the maintenance contract for git.rs; (c) consider gating git.rs behind a feature so a future build can swap in git2 if the maintenance cost proves too high. Treat as a deliberate tradeoff, not a defect.

---

### F93. [MEDIUM] Schema version is a single hardcoded constant with no migration or forward-compatibility path

`architecture` · location `src/core/version.rs:9 (DLOG_SCHEMA_VERSION=1); enforced at src/core/dlog.rs:65-71` · reporter-confidence high · verification **CONFIRMED**

**Description:** DLOG_SCHEMA_VERSION is a compile-time constant pinned to 1, and parse_dlog_value rejects any dlog whose schema field != that exact value ('expected schema version 1'). There is no version-dispatch in the parser, no migration step, and no notion of a forward-compatible minor change. The dmap has no version field at all (it is purely derivative and regenerated). I verified the error path: storage tests assert 'schema: expected schema version 1'.

**Why it matters:** The .dlog is the authoritative, version-controlled, user-committed artifact. The moment v3 needs to add or change a dlog field, every existing .dlog in every repo becomes unreadable by the new binary AND new .dlog files become unreadable by old binaries, with a hard error and no upgrade path. For a tool whose entire premise is durable decision memory committed alongside code, schema evolution is a near-certain future need (e.g. richer rejected-alternatives, decision links, confidence). The current design makes the first schema change a breaking, all-or-nothing migration.

**Impact:** A future schema bump forces a flag-day migration across all consumers; mixed-version teams (some on old binary) get hard parse failures rather than graceful degradation. This is the single biggest barrier to evolving toward 1.0.

**Likelihood:** High that a schema change will be wanted before 1.0; the cost lands entirely at that point.

**Evidence (reporter):** version.rs:9 `pub const DLOG_SCHEMA_VERSION: u32 = 1;`; dlog.rs:65-71 `if schema != DLOG_SCHEMA_VERSION { return Err(... format!("expected schema version {DLOG_SCHEMA_VERSION}")) }`; no other schema branch exists (grep for schema in dlog.rs shows only equality check).

**Independent verification:** CODE: src/core/version.rs:8 `pub const DLOG_SCHEMA_VERSION: u32 = 1;` (claim said :9 — off by one, the constant is at line 8 of the file as the package-name const occupies the lines above). src/core/dlog.rs:65-70 is a strict equality gate inside parse_dlog_value: `let schema = expect_u32(...); if schema != DLOG_SCHEMA_VERSION { return Err(ArchivaError::schema("schema", format!("expected schema version {DLOG_SCHEMA_VERSION}"))); }`. grep for `migrat|forward.compat|backward.compat|version dispatch` across src/*.rs returns zero matches outside unrelated hits — there is no version-branching, no migration step, no minor/major distinction. Only one `schema` comparison exists in dlog.rs (equality). storage.rs:479 asserts the exact error string `schema: expected schema version 1`.

DMAP: confirmed no version field. Init+write produced `.decisions/app.js.dmap` whose entire content is `1-3:fn:foo` — purely derivative, regenerated from the dlog.

RUNTIME REPRO: in /tmp/schematest (git-init'd, app.js committed), wrote a valid decision (dlog shows `schema: 1`). I then edited the dlog `schema: 1` -> `schema: 2`. Every read path hard-fails: `archiva why app.js` prints `schema: expected schema version 1` and exits 1; `archiva status` prints the same and exits 1; `archiva lint` exits 1. There is no graceful degradation, no warning-and-continue, no auto-migrate — a single unrecognized schema integer takes the whole repo's decision memory offline for that file.

**Verifier notes / severity correction:** Claim is accurate on substance and impact. One minor correction: the constant is at src/core/version.rs:8, not :9 (the claim's dlog.rs:65-71 range is right, the gate is lines 65-70). Everything else holds exactly: single hardcoded compile-time constant, strict equality enforcement, no migration/dispatch path, dmap carries no version. Severity medium is appropriate — this is a forward-compatibility/architectural weakness, NOT a present-day defect (schema 1 is the only version that exists, so nothing is broken today). The "biggest barrier to 1.0" framing is a fair characterization but is forward-looking: the cost is a future flag-day migration and hard failures for mixed-version teams, not a current bug. Worth noting the failure mode is at least loud and safe (clean error + exit 1, no data corruption or silent misbehavior), and the dlog being authoritative + dmap regenerated means a migration tool could be written without data loss. Recommended resolution: accept-major / reject-unknown-minor policy (treat schema as major.minor, tolerate unknown-but-compatible minors with a warning), plus a documented `archiva migrate` path before the first schema bump.

**Recommended resolution:** Before 1.0, replace the equality check with a range/dispatch: accept schema <= CURRENT, branch parsing by version, and write the newest. Define and document a forward-compat policy (e.g. unknown fields preserved/ignored on read so older binaries degrade gracefully). Add a migration test fixture per version. This is cheap to design now and expensive to retrofit after the first real schema change ships.

---

### F94. [LOW] MCP exposes only 3 of the 8 CLI capabilities, creating a capability asymmetry between the two primary interfaces

`architecture` · location `src/mcp.rs:402-404 (tool_definitions: write_decision, why, ghost_check) vs src/cli.rs:60-67 (init/why/history/hooks/write-decision/status/lint/mcp)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The MCP server — the interface AI agents actually drive — exposes write_decision, why, and ghost_check. The CLI additionally offers history, status, lint, and the session-start/post-tool-use hooks. An agent over MCP cannot retrieve a decision's history chain, get a repo-wide status report, or run a full lint, even though project.rs already implements all of these as reusable functions (project::history, project::status, project::lint_project). I verified tools/list returns exactly 3 tools.

**Why it matters:** For an agent-memory product, the MCP surface is the primary product surface; the CLI is largely for humans and hook wiring. Leaving history/status on the floor means agents can record and look up single decisions but cannot reason over decision health or evolution — arguably the differentiated value. Because the orchestration logic already exists in project.rs, this is a thin wiring gap, not a missing capability, which makes the asymmetry look like incomplete surfacing rather than a deliberate scope cut.

**Impact:** Agents get a reduced feature set vs humans; the richest analysis capabilities (history, status) are unreachable over the agent-facing protocol despite being implemented.

**Likelihood:** N/A (current designed state) — affects every MCP consumer.

**Evidence (reporter):** mcp.rs:402 tool_definitions has 3 entries; live `tools/list` over stdio returned write_decision/why/ghost_check only; project.rs:49 `pub fn history`, :87 `pub fn status`, :93 `pub fn lint_project` all exist and are exercised by the CLI.

**Independent verification:** MCP tool set (live): drove `archiva mcp` over stdio with initialize + tools/list; response id=2 returned exactly TOOLS: ['write_decision', 'why', 'ghost_check']. Confirmed in source at src/mcp.rs:402 `pub fn tool_definitions() -> Vec<JsonValue> { vec![write_decision_tool(), why_tool(), ghost_check_tool()] }` (single line, not 402-404, but content matches).

CLI command set: src/cli.rs match arms (lines 57-67) dispatch init/why/history/hooks/write-decision/status/lint/mcp (plus help/-V). So the CLI offers history, status, and lint that MCP does not. (Claim cited cli.rs:60-67; actual range is ~57-67 — minor line drift, substance correct.)

Reusable functions exist exactly as claimed: src/core/project.rs:49 `pub fn history`, :87 `pub fn status`, :93 `pub fn lint_project`. Note: the claim cites these as `src/project.rs` — the real path is `src/core/project.rs`, but line numbers match exactly.

ghost_check is NOT a substitute for history/status/full lint: src/mcp.rs:124-129 maps ghost_check to `format_ghost_check_result(...)`, and src/core/lint.rs `format_ghost_check_result` only formats LintIssues for a single file. It is a file-scoped lint subset; there is no MCP path to repo-wide status, the history chain, or a full project lint. So the asymmetry stands: an agent over MCP cannot retrieve history, repo-wide status, or full lint.

**Verifier notes / severity correction:** CONFIRMED with two citation corrections: (1) functions live in src/core/project.rs, not src/project.rs (line numbers 49/87/93 correct); (2) tool_definitions is a single line at mcp.rs:402, and the CLI match arms span ~57-67 not 60-67. None of these affect the substance. The asymmetry is real and verified end-to-end: MCP exposes 3 capabilities (write_decision, why, ghost_check=file-scoped lint), CLI exposes 8, and history/status/lint_project are already implemented as reusable functions exercised by the CLI but unreachable over MCP. Characterization is fair: this is a design tradeoff / architectural observation, not a defect — the MCP surface is plausibly kept minimal intentionally for agent ergonomics (write + read + drift-check covers the core decision-memory loop). low severity is appropriate; an argument for info could be made. Not critical/high: no incorrect behavior, no data loss, no security impact — purely a feature-coverage gap on the agent-facing interface.

**Recommended resolution:** Decide explicitly whether the 3-tool surface is intentional minimalism or interim scope, and document it. If agents are the primary audience, surface history and status as MCP tools (low effort given project.rs already implements them). At minimum, note the asymmetry in the architecture doc so it is a choice, not an accident.

---

### F95. [LOW] OrderedMap is O(n) per insert/lookup, making dlog/anchor assembly O(n^2); fine at current scale but an unbounded-by-design hot path

`tradeoff` · location `src/core/ordered_map.rs:30-48 (insert/get linear scan); used in 7 modules incl. anchor.rs (AnchorBuilder), dlog.rs, storage.rs, decision.rs` · reporter-confidence high · verification **CONFIRMED**

**Description:** OrderedMap backs every order-preserving collection (decisions in a dlog, anchors in an extraction) to guarantee deterministic output. Its insert() linearly scans for an existing key before pushing, and get/get_str/remove_str are linear. Building a map of N items is therefore O(N^2). This is chosen deliberately for deterministic iteration order (critical for stable .dlog/.dmap rendering and TS parity) and zero dependencies. I measured real behavior: status/lint across 200 dlog files ran in ~0.43s; writing decisions to a 1000-function file held steady at ~46ms/write (dominated by re-extracting anchors, not map ops). So the quadratic is real but the per-collection N (decisions-per-file, anchors-per-file) is naturally small in practice.

**Why it matters:** The complexity is hidden inside an abstraction used pervasively, so it is invisible at call sites. It is bounded today only by the empirical smallness of per-file decision/anchor counts, not by any design constraint. A pathological file (thousands of anchors, or a long-lived hot file accumulating hundreds of decisions) would degrade super-linearly, and because the abstraction is shared, a fix must preserve insertion order everywhere.

**Impact:** No user-visible impact at realistic scale (verified). Latent risk for outlier files; the cost is structural, not local.

**Likelihood:** Low — requires unusually large single-file decision/anchor counts to matter.

**Evidence (reporter):** ordered_map.rs:30-48 linear insert/get; measured /tmp/scale_test: 200-file status 0.427s, 200-file lint 0.427s; per-write on 1000-fn file steady ~0.046s across 0..199 decisions (no super-linear growth observed at that scale).

**Independent verification:** STRUCTURAL (read exact code):
- src/core/ordered_map.rs:30-48 — insert() does `entries.iter_mut().find(...)` linear scan before push; get/get_str/remove_str (lines 42-68) all linear scans over a Vec. Confirmed.
- src/core/anchor.rs:388-406 — AnchorBuilder::add calls self.anchors.insert(...) per anchor; dedup counts use a real HashMap but the order-preserving anchor map is OrderedMap, so building N anchors is N linear scans = O(N^2). Confirmed.
- src/core/dlog.rs:74-80 — dlog parse rebuilds decisions via OrderedMap::insert per entry (also O(N^2) in decisions-per-file). Confirmed.
- 7 modules use OrderedMap (anchor, decision, dlog, project, status, storage + def): grep confirmed.
- Hot path is real: write_decision_with_context (project.rs:212) and assert_anchor_exists (anchor.rs:347-348) both call extract_anchors, which builds the full N-anchor OrderedMap. post_tool_use (project.rs:235,270) does the same — and that hook is auto-fired after every agent edit.

EMPIRICAL (binary v0.2.0, /tmp/scale_v, generated Rust files of N top-level fns; verified each produces N distinct `fn:func_*` anchors via the "Available anchors" error listing):
Successful `write-decision` (full extraction) avg time:
  N=250  0.0119s | N=500 0.0212s | N=1000 0.0606s | N=2000 0.2525s | N=4000 1.0117s | N=8000 4.3165s
Ratio per doubling converges to ~4.0x (0.06->0.25->1.01->4.32) = textbook O(N^2).
post-tool-use hook (auto-fired in agent loop) on same files: N=1000 0.056s, N=4000 1.098s, N=8000 4.261s — same quadratic.
Realistic scale: this repo's own src/core/anchor.rs (12,341 lines, 321 extracted anchors) -> post-tool-use 0.094s (acceptable).

NOTE: the claim's own data capped at N<=1000 (~46-60ms), where absolute time masks the curve; my N>=2000 runs make the quadratic unmistakable. The claimant's sub-statement "no super-linear growth observed at that scale" is an artifact of too-small N, not absence of the quadratic.

**Verifier notes / severity correction:** Core finding CONFIRMED: OrderedMap is O(n) per op and anchor/dlog assembly is genuinely O(n^2), verified both by code and by clean 4x-per-doubling timing. The claimant's conclusion (deliberate tradeoff for deterministic ordering + zero deps; structural/latent rather than locally fixable) is accurate and severity low is defensible for typical files.

Two corrections to the claim's framing:
1. "no super-linear growth observed at that scale" is REFUTED — growth is clearly super-linear; the claimant simply didn't test past N=1000. From N=1000 (0.06s) to N=8000 (4.3s) the ratio is a clean ~4x per doubling.
2. "No user-visible impact at realistic scale" is true for typical hand-written files but overstated for the tail. The cost lands in post_tool_use, which is auto-fired after every agent edit, so a large single file (bundled/minified JS, generated code, large C/C++ translation units, vendored single-file libs) drives multi-second pauses into the live agent loop (4000 fns ~1.1s, 8000 fns ~4.3s) and risks hook timeouts. I'd treat that as a low/medium operational risk rather than purely latent. The fix is cheap and localized despite being "structural": back OrderedMap with a parallel HashMap<K,usize> index (or have AnchorBuilder/dlog parse use a side HashMap for existence + a Vec for order), preserving deterministic output while making insert/lookup O(1). Recommend doing it before any feature that ingests large/generated files.

**Recommended resolution:** Leave as-is for now (correctly prioritizes determinism + zero deps and is empirically adequate). If profiling ever flags it, add a side HashMap<K, usize> index inside OrderedMap to make insert/get O(1) while keeping the Vec for ordered iteration — a localized change behind the existing API. Document the O(n^2) property on the type so call sites are aware.

---

### F96. [INFO] project.rs is a single-module orchestration hub coupling nearly every leaf module; cohesive today but a growth pressure point

`architecture` · location `src/core/project.rs (imports anchor, decision, decision_status, diff, dlog, dmap, fs, git, gitignore, lint, status, storage, paths, time, version)` · reporter-confidence high · verification **CONFIRMED**

**Description:** project.rs is the composition root for all business workflows (why/history/status/lint/write_decision/post_tool_use/session_start). It depends on essentially every other core module, while the leaf modules stay nicely decoupled from each other (verified the import graph: leaves depend down toward error/paths/ordered_map, project depends across all of them). post_tool_use (project.rs:223-345) in particular interleaves anchor extraction, moved-file detection, git HEAD diffing, line-range shifting, fingerprint staleness, locking, and dual-file persistence in one function. This is a deliberate and currently-healthy hub-and-spoke design — the coupling lives in one place by intent — but it is where every new cross-cutting feature will land.

**Why it matters:** As the system grows toward 1.0, project.rs is the module most likely to accrete complexity and become hard to reason about, because every workflow that touches multiple subsystems must be wired here. The post_tool_use flow is already the most intricate control flow in the codebase and the hardest to verify by reading. This is normal for a composition root, but worth naming as the spot to watch for cohesion erosion.

**Impact:** No current defect; it is the predictable future maintenance focal point and the function most in need of careful test coverage on edge interleavings (move + fallback + incomplete extraction).

**Likelihood:** N/A — structural observation.

**Evidence (reporter):** Import scan shows project.rs pulling from 15 modules while leaf modules import only downward; post_tool_use spans project.rs:223-345 with nested locking and branch logic; project.rs has 1575 lines of tests guarding it (impl ~685).

**Independent verification:** Read src/core/project.rs (2261 lines) and ran import scans across all core modules.

(1) IMPORT HUB: `grep -oE "use crate::core::[a-z_]+" project.rs | sort -u` yields 18 distinct core modules: anchor, decision, decision_status, diff, dlog, dmap, error, fingerprint, fs, git, gitignore, lint, ordered_map, paths, status, storage, time, version. The claim said ~15; actual is broader. Next-most-coupled module is storage.rs (11 distinct), confirming project.rs is the singular hub.

(2) LEAF DECOUPLING: per-module cross-core import counts (sorted): storage 18 raw / 11 distinct, decision 11, dlog 9, decision_status 7, status 6, error 6, git 5, anchor 4, fs 3, lint 2, diff 2, version 2, paths 1, fingerprint 1, time 0, ordered_map 0, gitignore 0, dmap 0. Distinct-import direction is strictly downward: anchor→{error,paths,ordered_map}; lint→{paths}; diff→{dlog}; status→{dlog,dmap,ordered_map,paths}; decision_status→{dlog,dmap,fingerprint,time}. Clean low(error/paths/ordered_map/time)→data(dlog/dmap/fingerprint)→mid→storage→project layering; no leaf-to-leaf tangle.

(3) post_tool_use: `awk` confirms function body is exactly project.rs:223-345 (next fn moved_dlog_candidate at 346). Read body — interleaves all seven concerns: extract_anchors (236), moved_dlog_candidate/move_dlog_and_dmap_locked (237-247), read_git_head_file + diff_lines (272-276), apply_line_changes_to_range (290), is_fingerprint_stale/mark_stale_now (319-322), with_decision_file_lock (282), write_dlog+write_dmap (330-331), with nested move/fallback branching.

(4) TEST RATIO: `mod tests` at line 687; file 2261 lines → ~1574 test lines vs ~686 impl lines. Matches claimed 1575/685.

**Verifier notes / severity correction:** Claim fully confirmed and, if anything, understated: project.rs couples to 18 core modules, not ~15. All structural assertions (hub-and-spoke, downward-only leaf imports, post_tool_use:223-345 interleaving seven concerns, ~1575-test/~685-impl ratio) verified against source. This is a legitimate info-level architectural observation, not an inflated style preference — there is no current defect; it is correctly characterized as the intentional composition root and the predictable focal point for future cross-cutting features. The recommendation to keep careful edge-interleaving coverage (move + fallback + incomplete-extraction) on post_tool_use is well-founded given that function concentrates the most branch complexity. No correction to scope or severity needed; severity stays info.

**Recommended resolution:** No action needed now — the design is sound and well-tested. As features are added, resist growing post_tool_use further; extract named sub-steps (re-anchor decision, classify staleness/orphan) into testable helpers so the orchestration reads as a sequence of intent-named operations. Keep project.rs as a thin composition layer rather than letting business logic migrate into it.

---

### F97. [INFO] Retained TS oracle (src/*.ts) is active differential infrastructure, not dead weight — but its lifecycle is undocumented

`operational` · location `src/core/*.ts (881 LOC, uses ts-morph), tools/archiva-differential.ts, tools/archiva-ts-baseline.ts:7 (imports extractAnchors from src/core/anchor.js)` · reporter-confidence high · verification **CONFIRMED**

**Description:** The TS files coexisting with Rust in src/ are NOT dead code: tools/archiva-ts-baseline.ts and the differential harness import them (extractAnchors, writeDecision, why, postToolUse, lintProject) and the npm scripts run `differential`/`benchmark:compare` comparing the Rust binary against the TS implementation. The shipped npm package excludes all of it (package.json files[] ships only dist-native/archiva.exe + docs + install scripts; dependencies is empty, ts-morph is a devDependency). This is exactly why the 7k-line hand-rolled TS/JS anchor extractor in Rust is defensible — its correctness is continuously gated against a real TS-compiler-based oracle. By contrast, Rust and C/C++ extraction are native-only with no oracle (README:437).

**Why it matters:** This is a genuine architectural strength that an outside auditor could easily mistake for cruft. It is the mechanism that justifies the std-only purity for the TS path. However, the oracle's purpose, scope (TS/JS only), and intended retirement are not surfaced where a contributor would see them, risking either accidental deletion or indefinite drag.

**Impact:** Positive: de-risks the largest hand-rolled subsystem. Risk is only that its role is implicit and its sunset undefined.

**Likelihood:** N/A — observation.

**Evidence (reporter):** tools/archiva-ts-baseline.ts:7-10 imports from ../src/core/*.js; package.json scripts include differential/baseline:ts using tsx on src; package.json files[] excludes src/*.ts and dependencies:[] (ts-morph is devDependency); README:436-437 states TS/JS corpora compare TS baseline vs Rust while Rust/C corpora run Rust-only.

**Independent verification:** All claimed facts independently verified by reading source:

1. TS oracle size: `wc -l src/core/*.ts` = 881 total (exact match to claim).

2. Imports are real and invoked (not vestigial). tools/archiva-ts-baseline.ts:7-10 import `extractAnchors` (../src/core/anchor.js), `writeDecision`,`why` (decision.js), `applyDiffToRange`,`postToolUse` (reanchor.js), `lintProject` (../src/lint/rules.js). grep shows them actually called: writeDecision (53,89,152), why (98), extractAnchors (130), applyDiffToRange (141), postToolUse (166,193), lintProject (211).

3. The oracle is a real TS-compiler-based parser, not a hand-rolled stand-in. src/core/anchor.ts:1 `import { Node, Project, SourceFile, SyntaxKind } from "ts-morph";`. ts-morph appears only here in src/ and is declared a devDependency (package.json:86).

4. Differential harness drives BOTH runtimes. tools/archiva-differential.ts:51-54 defines `runtimes` = [{name:"typescript", command:process.execPath, prefixArgs:[bin/archiva.js]}, {name:"rust", command:rustBin}]. Both entrypoints exist on disk (bin/archiva.js, src/cli/main.ts present). Scenarios compare TS vs Rust on anchor extraction (anchor:"fn:..." cases), path validation, MCP. npm scripts confirm wiring: `differential`, `differential:release` (ARCHIVA_RUST_BIN=target/release/archiva), `baseline:ts` (tsx tools/archiva-ts-baseline.ts), `benchmark:compare`.

5. Shipped package excludes the oracle. package.json files[] = dist-native/archiva.exe, dist-native/package.json, package-manifest.json, schema, tools/install-native.mjs, tools/native-targets.mjs, docs, README.md, LICENSE — no src/*.ts, no tools harnesses. There is NO `dependencies` field at all (grep: "NO dependencies field"); ts-morph/tsx/typescript/vitest/zod are all devDependencies. Runtime install is via optionalDependencies (platform native binaries) + postinstall install-native.mjs.

6. README confirms native-only for Rust/C with no oracle. README:436-438: "TypeScript/JavaScript corpora compare the TypeScript baseline against Rust. Rust and C/C++ corpora run the native Rust binary only, because the TypeScript baseline cannot parse those source languages." (Claim cited README:437; actual span 436-438 — immaterial offset.)

7. Lifecycle/sunset is undocumented. grep -rin for sunset/deprecat/retire/remove-TS-oracle across docs/ and README.md returned ZERO matches, confirming the "lifecycle undocumented / sunset undefined" sub-claim.

**Verifier notes / severity correction:** Claim is accurate in full, including the negative sub-claim (no sunset documentation). Severity info is correct: this is a positive operational finding — the differential oracle genuinely de-risks the largest hand-rolled subsystem (TS/JS anchor extraction) by gating it against a ts-morph (real TS compiler) baseline, and it is correctly excluded from the shipped artifact (zero runtime deps). Two minor, non-load-bearing corrections to the claim's citations: (a) the native-only statement is at README lines 436-438, not exactly 437; (b) the claim's parenthetical "(881 LOC, uses ts-morph)" is accurate but worth noting ts-morph is imported in exactly one file (src/core/anchor.ts:1) — the rest of the 881 LOC is plain TS port logic, so the oracle's TS-compiler dependency is concentrated in the anchor extractor, which is precisely the subsystem the claim argues is de-risked. The only genuine gap (the basis for the operational flag) is the absence of any documented decommission criteria: nothing states under what conditions src/*.ts and the differential harness get removed, so the dual-maintenance burden is open-ended. Recommend adding a short lifecycle note (e.g., in docs/ or README) defining when the TS oracle is retired — e.g. after N releases of differential parity, or upon adding a second non-TS oracle. No defect; confirmed as info.

**Recommended resolution:** Document the oracle explicitly (in the architecture doc and a src/ README): it exists to differentially validate the Rust TS/JS extractor, ships to no one, and covers only the TS/JS path. Define the retirement trigger (e.g. 'remove after N releases of zero differential diffs') so it is a managed asset with a sunset, not permanent ambiguous baggage. Note that Rust/C extraction has NO oracle — consider building one (compare against rustc/clang AST dumps) to extend the same safety net to native-only languages.

---

## Completeness Critic — audit-coverage gaps  — score 7/10

> The 97-finding panel is broad and, on the high-severity items I spot-checked, accurate: I independently reproduced the lone-single-quote YAML panic (yaml.rs:700), the Rust anchor stack overflow on deep nesting, the no-HEAD-baseline false-STALE behavior (decision correctly shifts to lines [2,5] with a committed baseline but is frozen and marked STALE without one), the silent supersession data loss (re-deciding an anchor without `supersedes` erases the prior decision and its history), the MCP `why` line-argument being ignored (line:99 still returns the only decision), the .dmap-never-read claim (garbage .dmap, line lookup still works), and the lint/status exit-code conflation (issues present, exit 0). The audit is well-founded. However, it has real coverage gaps. The most important: (1) the panic surface is under-enumerated — I found a THIRD, un-listed stack overflow (empty block scalar `chose: |` or `chose: >` followed by a lower-indent key crashes status/lint), meaning the YAML hardening problem is a class, not two isolated bugs; (2) the Rust-extractor DoS blast radius is larger than stated — a single pathological source file anywhere in the repo tree crashes whole-project `lint`/`status` even when that file has NO decision, because the orphan/move scan re-extracts it; (3) no finding establishes whether ANY whole-repo command is panic-safe against an adversarial/corrupt .decisions tree, which is the central robustness question given .decisions/ is committed and shared. The single most important un-performed check before 1.0 is an end-to-end panic-safety harness over corrupt .dlog and pathological source inputs across every command (especially the long-lived MCP server, where one crafted decision kills the session). The second is a real Claude Code hook integration test, since the auto-wired PostToolUse hook is a confirmed silent no-op under the actual stdin contract.

*Score rationale:* The 97-finding panel is thorough and, on every high-severity claim I independently reproduced (YAML lone-quote panic, Rust extractor overflow, no-HEAD false-STALE, supersession data loss, MCP why line-ignore, dmap-never-read, exit-code conflation, hook no-op), it is accurate and well-located to file:line. That earns a high base. It loses points for three coverage gaps that matter at 1.0: (1) the panic surface is treated as a finite list of two YAML panics when it is a class — I found a third (empty block scalar) in minutes; (2) the Rust-extractor DoS blast radius is understated (any source file in the tree, decided or not, crashes whole-project commands); (3) no end-to-end panic-safety or real-Claude-Code hook integration harness exists, so the product's core automatic workflow and its robustness against its own committed/shared data store are unproven. It also has zero 'critical' findings despite trivially reachable repo-wide process aborts, and several atomicity/locking verdicts rest on code reading rather than fault injection.

### G1. [HIGH] Panic surface is under-enumerated: a third, unlisted stack overflow exists (empty block scalar in .dlog)

`defect` · src/core/yaml.rs (block-scalar collection ~300-320 and fold/literal render 1046-1075); reachable from status/lint over .decisions/*.dlog

**Gap:** The panel lists exactly two YAML panics (lone single-quote at yaml.rs:700; multibyte slice at yaml.rs:310). I found a third: a .dlog whose scalar is an empty block-scalar header `chose: |` (or `chose: >`) immediately followed by a lower-indent key (`because: y`) causes a stack overflow (SIGABRT, exit 134) in `status` and `lint`, while `why` survives. This is a distinct code path from the two listed panics.

**Why it matters:** It demonstrates the YAML parser panics are a CLASS of bug, not two isolated cases. An audit that enumerates two specific panics implies a finite, patchable list; the real risk is that the hand-rolled YAML parser has not been panic-fuzzed and likely harbors more. .decisions/ is committed and shared, so any of these can be planted by a teammate or a malicious PR and will crash the long-lived MCP server and every whole-repo command.

**Recommendation:** Do not ship until the YAML parser is fuzzed (cargo-fuzz or an in-repo property soak over arbitrary byte inputs) and every recursive/slicing path is bounded. Convert the recursion in the block-scalar/render path to iteration or add a depth guard mirroring the JSON parser's guard. Treat 'no panic on any .dlog input' as a release gate.

---

### G2. [HIGH] Rust-extractor DoS blast radius is larger than the panel states: any pathological source file in the repo crashes whole-project lint/status, even files with no decision

`defect` · src/core/project.rs orphan/move scan (lint_project_inner ~133-157, moved_dlog_candidate ~346-375) calling anchor extraction on discovered source files; src/core/anchor.rs collect_rust_item_anchors:733

**Gap:** The panel scopes the Rust stack-overflow finding to status/write-decision/MCP triggered via a file under analysis. I observed that a deep.rs with ~50k nested blocks and NO associated decision still crashes `lint` and `status` (SIGABRT 134); deleting deep.* makes both commands succeed. This means whole-project commands re-extract anchors from source files discovered by the orphan/move-candidate scan, so the DoS does not require the malicious file to be decided — merely present in the tree.

**Why it matters:** It changes the threat model: an attacker (or an unlucky generated/minified file) does not need write access to .decisions/ to brick the tool repo-wide; dropping one source file into the repo is enough. The panel's separate 'O(decisions) stat scan' and 'stack overflow' findings are correct individually but the combination (project scan x recursive extractor) is the actual release-blocking system behavior.

**Recommendation:** Bound extraction depth/work per file (token or recursion cap) AND make the project scan skip-and-report files that fail extraction rather than aborting the command (this also resolves the panel's separate 'one corrupt file aborts the whole command' finding). Add a scale test with a single pathological file to the PR gate.

---

### G3. [HIGH] No end-to-end panic-safety / corrupt-input harness across commands — the central robustness question for a committed, shared .decisions/ is never tested

`enhancement` · Test strategy gap: src/core/property_tests.rs asserts anchor invariants only; no command-level fuzz over corrupt .dlog/.dmap or pathological source; tools/archiva-scale-smoke.ts exercises best case

**Gap:** Three independent crash classes (lone-quote YAML panic, empty-block-scalar overflow, Rust-extractor overflow) are all reachable from committed, peer-shared, version-controlled inputs (.dlog files and source files). The audit catalogs them individually but never asks the consolidating question: is there ANY whole-repo command that is panic-safe against an adversarial or merely corrupt .decisions tree? The answer, from my probing, is no — status/lint/session-start/MCP all abort. No harness drives each command against a corpus of malformed inputs.

**Why it matters:** This is the single most important missing check for 1.0. The product's data store is committed to git and shared across a team and across the TS->Rust migration; corrupt or hostile files WILL occur. A process abort (SIGABRT) in the MCP server kills the agent's whole session, not just one request.

**Recommendation:** Before 1.0: add a panic-safety gate that runs every command (init/why/history/lint/status/hooks/mcp/write-decision) against a generated corpus of malformed .dlog/.dmap and pathological source files, asserting graceful non-zero exit (never SIGABRT). Wire it into PR CI, not the weekly job.

---

### G4. [HIGH] Real Claude Code hook integration was never executed; the confirmed PostToolUse no-op means the product's core promise is unverified end-to-end

`enhancement` · src/core/settings.rs (init template wiring `archiva hooks post-tool-use` with no arg) vs src/cli.rs:243-253 (reads path from arg or ARCHIVA_FILE only); .claude/settings.json generated by init

**Gap:** The panel correctly flags (operational/high) that the auto-wired PostToolUse hook is a no-op under Claude Code's stdin contract. But no finding closes the loop: nobody ran the tool inside (or simulating) Claude Code to confirm decisions actually get re-anchored in the intended workflow. I confirmed the hook receives `{"tool_input":{"file_path":...}}` on stdin and the binary ignores stdin entirely, printing 'Missing file path' and exiting 0. Combined with the no-HEAD-baseline false-STALE bug, the headline workflow (agent edits -> decisions auto-shift) is broken in two independent ways and was validated only via direct CLI args that the real harness does not pass.

**Why it matters:** The product is 'decision memory for AI coding agents'; its primary automatic integration is the PostToolUse hook. If that path has never been exercised as the agent actually invokes it, the tool's main value proposition is unproven for 1.0.

**Recommendation:** Add an integration test that feeds the exact Claude Code PostToolUse/SessionStart stdin JSON shape to the wired commands and asserts re-anchoring occurs; fix the hook to parse stdin JSON (file_path) before 1.0.

---

### G5. [HIGH] Process-level robustness of the long-lived MCP server is never probed: a panic aborts the whole stdio session, not one request

`operational` · src/mcp.rs serve_reader_writer (206-267); no catch_unwind / panic=abort interaction documented

**Gap:** Every panic finding is framed per-command, but the MCP server is the one component that persists across many requests in a single process. None of the 97 findings tests whether a panic in handling request N is isolated from request N+1. Given Rust's default panic behavior aborts the process (and the binary visibly SIGABRTs on the overflows), one malformed decision or pathological file referenced in any tools/call terminates the entire agent's MCP session, dropping all in-flight context.

**Why it matters:** For an agent-facing server, request isolation is a baseline expectation. A crash that takes down the session (vs returning an error) is a qualitatively worse failure than the same crash in a one-shot CLI invocation, yet the audit assigns the same severities to both.

**Recommendation:** Wrap per-request handling in std::panic::catch_unwind and return a JSON-RPC error / isError result on panic, OR (preferable) remove the panics. Add an MCP soak test that streams malformed and pathological requests and asserts the server stays alive.

---

### G6. [MEDIUM] MCP `why` line handling is mis-categorized as 'silently ignored' — it returns affirmatively wrong results, warranting higher severity

`defect` · src/mcp.rs:118-123 & 172-181 (why_tool_arguments_from_json drops line); inputSchema also omits `line` entirely

**Gap:** The panel rates 'MCP why cannot do line-based lookup; line argument silently ignored' as medium and frames it as a missing capability. In practice it is worse: I sent {file:foo.rs, line:99} (a line in no decision's range) and MCP returned the file's only decision as if it matched, whereas CLI `why foo.rs 99` correctly returns 'No decision found ... at line 99'. The MCP tool also advertises no `line` property in its inputSchema, so an agent passing a line gets a confidently wrong answer rather than a not-found.

**Why it matters:** Agents are the primary MCP consumers; returning an unrelated decision for an arbitrary line actively misinforms the agent (it may attribute a rationale to code it does not cover), which is more harmful than a missing feature.

**Recommendation:** Either implement line resolution in MCP why to match CLI semantics, or reject `line` explicitly; do not return file-wide results for a line query. Re-rank closer to the panel's other 'wrong result' defects.

---

### G7. [MEDIUM] Contradiction in the panel: MCP ghost_check is rated both 'result-equivalent, no defect' yet depends on the Rust extractor that the panel separately proves crashes

`operational` · src/mcp.rs:124-133 (ghost_check -> project::lint_file) cross-referenced with anchor.rs:733 overflow and the empty-block-scalar overflow

**Gap:** Finding 'ghost_check reimplemented ... verified result-equivalent, no defect' (info) coexists with the high-severity extractor stack overflow and the YAML panics, all reachable through the same MCP server process. The 'no defect' verdict is true only for well-formed inputs; ghost_check on a file that triggers the extractor overflow (or whose .dlog hits a YAML panic) aborts the entire MCP server process. The panel never tests ghost_check against the very inputs its own other findings prove are fatal.

**Why it matters:** An 'info / no defect' label on an MCP-exposed path that can SIGABRT the shared server understates risk to a reader triaging the 97 findings. The MCP server is long-lived: one crafted ghost_check kills the agent session.

**Recommendation:** Re-test ghost_check specifically against pathological source and corrupt .dlog; make the MCP request handler catch panics (or, better, eliminate them) so one bad request returns isError:true rather than aborting the process.

---

### G8. [MEDIUM] Concurrency and crash-during-write claims (torn-write durability, lock wedging) are asserted from code reading but never reproduced under real contention/crash

`enhancement` · src/core/storage.rs:256-258 (non-atomic dlog+dmap), src/core/fs.rs lock recovery 426-468/563-594; panel findings on atomicity and lock wedging

**Gap:** Several medium/high findings (non-atomic dlog/dmap commit, PID-liveness lock wedge, read-only commands taking write locks) are derived from reading the code, with verdicts CONFIRMED/PLAUSIBLE. I did not find evidence anyone reproduced these dynamically (e.g., killing the process between the .dlog and .dmap writes, or running two writers concurrently on the same anchor, or pointing at a read-only .decisions/). For a tool whose store is shared and concurrently hit by hook + MCP + manual CLI, the absence of an actual concurrency/crash-injection test is itself a gap.

**Why it matters:** Atomicity and locking bugs are exactly the class that code-reading over-or-under-estimates; only fault injection settles them. The panel's confidence on these should be tempered until reproduced.

**Recommendation:** Add a crash-injection test (fault point between dlog and dmap writes) and a concurrency test (N processes writing distinct anchors in one file, plus a reader under a read-only mount) before relying on the atomicity/locking verdicts for 1.0 sign-off.

---

### G9. [MEDIUM] Severity floor mismatch: multiple confirmed process-abort (SIGABRT) crashes are rated 'high', but a tool whose shared data store can crash the whole repo's tooling has at least one release-blocking 'critical' among them

`operational` · Panel severity assignments for yaml.rs:700 panic, anchor.rs:733 overflow, and the corrupt-file-aborts-command findings

**Gap:** The audit caps these at 'high'. Given (a) .decisions/ is committed and team-shared, (b) the crashes are trivially triggered and abort the process, (c) they take down the long-lived MCP server, and (d) I found the blast radius extends to any source file in the tree, at least the combined 'planted input crashes all whole-repo commands + MCP session' scenario meets a 'critical / release-blocking' bar. No single finding is labeled critical, which risks a reader concluding nothing is a hard blocker.

**Why it matters:** Severity drives go/no-go. A panel with zero 'critical' findings can be read as 'ship with known-highs', but the panic class is a genuine blocker for a 1.0 that stores shared, untrusted-by-default data.

**Recommendation:** Elevate the consolidated 'panic on committed/shared input' theme to critical and treat panic-safety as a 1.0 gate.

---

### G10. [LOW] init transactional-ity and re-run behavior under partial/dirty state under-examined

`enhancement` · src/core/init.rs (init_project); panel finding 'init is not transactional' (low)

**Gap:** The panel notes init is not transactional but does not probe re-run idempotency against a partially-initialized or pre-existing .claude/settings.json containing user hooks/mcpServers. I confirmed a clean re-run is idempotent ('Archiva initialized.' twice, settings.json well-formed), but the merge behavior into a settings.json that already has unrelated SessionStart/PostToolUse hooks or mcpServers was not tested by the panel and is the realistic case (users already use Claude Code hooks).

**Why it matters:** init clobbering or duplicating a user's existing hook config would be a visible, trust-damaging bug on first contact.

**Recommendation:** Add tests for init merging into a settings.json with pre-existing, unrelated hooks and mcpServers (preserve user entries, no duplication on re-run).

---
