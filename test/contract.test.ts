import { spawnSync, execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { initProject } from "../src/cli/init.js";
import { status } from "../src/cli/status.js";
import { extractAnchors } from "../src/core/anchor.js";
import { loadDlog, writeDlog } from "../src/core/dlog.js";
import { parseDmap, renderDmap } from "../src/core/dmap.js";
import { applyDiffToRange, postToolUse } from "../src/core/reanchor.js";
import { history, why, whyForLine, writeDecision } from "../src/core/decision.js";
import { fingerprint, normalizeCode } from "../src/core/fingerprint.js";
import { normalizeRelativePath, sourcePath } from "../src/core/paths.js";
import { sessionStart } from "../src/hooks/session-start.js";
import { handleRequest } from "../src/mcp/server.js";
import { loadGitignoreMatcher } from "../src/core/gitignore.js";
import { absoluteToRelative, listLintSourceFiles } from "../src/core/scan.js";
import type { DlogFile } from "../src/core/types.js";

const repoRoot = findRepoRoot(path.dirname(fileURLToPath(import.meta.url)));
const archivaBin = path.join(repoRoot, "bin", "archiva.js");

const ARCHIVA_AGENTS_BLOCK = `## Decision Logging (Archiva)

This project uses Archiva for decision tracking.

### Before modifying any file
- Read the decision map injected at session start (prefixed \`[Archiva]\`)
- Or call the \`why\` MCP tool: \`why(file, anchor)\`
- Do NOT modify code marked with a decision without reading it first

### After any non-trivial implementation choice
Call \`write_decision\` with:
- \`file\` and \`anchor\` (function or block name)
- \`chose\` - what approach you selected
- \`because\` - the specific reason, not a generic description
- \`rejected\` - every alternative you considered, with specific disqualifying reasons

Required for: algorithm choices, concurrency patterns, error handling strategies,
any point where you weighed 2+ approaches.

Not required for: imports, type declarations, formatting, variable names.

If changing code that has an existing decision:
- If your change preserves the reasoning -> keep the decision, update \`lines_hint\`
- If your change invalidates the reasoning -> call \`write_decision\` with \`supersedes: <id>\`
`;

describe("phase 0 contract fixtures", () => {
  it("captures version reporting through CLI and MCP initialize", async () => {
    const root = await tempProject();
    const packageJson = JSON.parse(readFileSync(path.join(repoRoot, "package.json"), "utf8")) as { version: string };

    expect(runArchiva(["--version"], "", root)).toMatchObject({
      status: 0,
      stdout: `${packageJson.version}\n`,
      stderr: ""
    });
    await expect(handleRequest(root, "initialize", {})).resolves.toEqual({
      protocolVersion: "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "archiva", version: packageJson.version }
    });
  });

  it("captures exact fingerprint normalization and SHA-256 truncation", () => {
    expect(normalizeCode("  const   x   =   1;\n\n")).toBe("const x = 1;");
    expect(fingerprint("const x = 1;\n")).toBe("3f41cbb3");
    expect(fingerprint("  const   x   =   1;\n\n")).toBe("3f41cbb3");
    expect(normalizeCode("function kept() {\n  return 1;\n}\n")).toBe("function kept() {\nreturn 1;\n}");
    expect(fingerprint("function kept() {\n  return 1;\n}\n")).toBe("479b6cd1");
  });

  it("captures current js-yaml dlog writer shape for wrapping and ambiguous scalars", async () => {
    const root = await tempProject();
    const dlog: DlogFile = {
      file: "src/wrap.ts",
      schema: 1,
      decisions: {
        "fn:wrap": {
          id: "dec_010",
          lines_hint: [2, 8],
          fingerprint: "abc123ef",
          chose: "plain string with colon: and hash # kept as scalar",
          because:
            "This reason intentionally crosses the one hundred character js-yaml wrapping boundary so Rust can verify plain scalar wrapping parity later.",
          rejected: [
            {
              approach: "flow array [x, y]",
              reason:
                "A long rejected reason also crosses the wrapping boundary so formatting remains visible in golden tests."
            }
          ],
          expires_if: "2026-01-02T03:04:05.000Z",
          session: "sess_contract",
          timestamp: "2026-06-26T20:31:18.340Z",
          history: []
        }
      }
    };

    await writeDlog(root, dlog);
    await expect(fs.readFile(path.join(root, ".decisions/src/wrap.ts.dlog"), "utf8")).resolves.toBe(`file: src/wrap.ts
schema: 1
decisions:
  fn:wrap:
    id: dec_010
    lines_hint:
      - 2
      - 8
    fingerprint: abc123ef
    chose: 'plain string with colon: and hash # kept as scalar'
    because: >-
      This reason intentionally crosses the one hundred character js-yaml wrapping boundary so Rust
      can verify plain scalar wrapping parity later.
    rejected:
      - approach: flow array [x, y]
        reason: >-
          A long rejected reason also crosses the wrapping boundary so formatting remains visible in
          golden tests.
    expires_if: '2026-01-02T03:04:05.000Z'
    session: sess_contract
    timestamp: '2026-06-26T20:31:18.340Z'
    history: []
`);
  });

  it("captures current dmap sorting for same-line case, punctuation, and unicode anchors", () => {
    expect(
      renderDmap([
        { startLine: 1, endLine: 1, anchor: "fn:z" },
        { startLine: 1, endLine: 1, anchor: "fn:A" },
        { startLine: 1, endLine: 1, anchor: "fn:a" },
        { startLine: 1, endLine: 1, anchor: "fn:_" },
        { startLine: 1, endLine: 1, anchor: "fn:á" },
        { startLine: 1, endLine: 1, anchor: "fn:Z" }
      ])
    ).toBe(`1-1:fn:_
1-1:fn:a
1-1:fn:A
1-1:fn:á
1-1:fn:z
1-1:fn:Z
`);
  });

  it("captures current dmap parser permissiveness and status suffix handling", () => {
    expect(parseDmap("  1-2:fn:trim  \r\n\r\n 3-4:fn:colon:ORPHAN \n")).toEqual([
      { startLine: 1, endLine: 2, anchor: "fn:trim", status: undefined },
      { startLine: 3, endLine: 4, anchor: "fn:colon", status: "ORPHAN" }
    ]);
    expect(parseDmap("0-1:fn:x\n")).toEqual([{ startLine: 0, endLine: 1, anchor: "fn:x", status: undefined }]);
    expect(parseDmap("5-2:fn:x\n")).toEqual([{ startLine: 5, endLine: 2, anchor: "fn:x", status: undefined }]);
    expect(parseDmap("-2:fn:x\n")).toEqual([{ startLine: 0, endLine: 2, anchor: "fn:x", status: undefined }]);
    expect(parseDmap("1-2-3:fn:x\n")).toEqual([{ startLine: 1, endLine: 2, anchor: "fn:x", status: undefined }]);
    expect(parseDmap("2-3:fn:x:NOT_STATUS\n")).toEqual([
      { startLine: 2, endLine: 3, anchor: "fn:x:NOT_STATUS", status: undefined }
    ]);
    expect(parseDmap("2-3:fn:x:STALE:ignored\n")).toEqual([
      { startLine: 2, endLine: 3, anchor: "fn:x:STALE:ignored", status: undefined }
    ]);
    expect(parseDmap("2-3:block:if_x:STALE\n")).toEqual([
      { startLine: 2, endLine: 3, anchor: "block:if_x", status: "STALE" }
    ]);
    expect(() => parseDmap("1-2:\n")).toThrow("Invalid .dmap line: 1-2:");
    expect(() => parseDmap("a-2:fn:x\n")).toThrow("Invalid .dmap range: a-2:fn:x");
    expect(() => parseDmap("2:fn:x\n")).toThrow("Invalid .dmap range: 2:fn:x");
  });

  it("captures dlog load defaults and schema-1 unknown field stripping on rewrite", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
    await fs.writeFile(
      path.join(root, ".decisions/src/legacy.ts.dlog"),
      `file: src/legacy.ts
schema: 1
extra_top_level: gone
decisions:
  fn:legacy:
    id: dec_007
    lines_hint:
      - 1
      - 2
    fingerprint: abc123ef
    chose: keep legacy parser
    because: fixture
    rejected: []
    timestamp: '2026-06-26T20:31:18.340Z'
    extra_record_field: gone
`,
      "utf8"
    );

    const loaded = await loadDlog(root, "src/legacy.ts");
    expect(loaded).toEqual({
      file: "src/legacy.ts",
      schema: 1,
      decisions: {
        "fn:legacy": {
          id: "dec_007",
          lines_hint: [1, 2],
          fingerprint: "abc123ef",
          chose: "keep legacy parser",
          because: "fixture",
          rejected: [],
          timestamp: "2026-06-26T20:31:18.340Z",
          history: []
        }
      }
    });

    await writeDlog(root, loaded!);
    await expect(fs.readFile(path.join(root, ".decisions/src/legacy.ts.dlog"), "utf8")).resolves.toBe(`file: src/legacy.ts
schema: 1
decisions:
  fn:legacy:
    id: dec_007
    lines_hint:
      - 1
      - 2
    fingerprint: abc123ef
    chose: keep legacy parser
    because: fixture
    rejected: []
    timestamp: '2026-06-26T20:31:18.340Z'
    history: []
`);
  });

  it("captures current js-yaml dlog reader forms Rust must accept", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
    await fs.writeFile(
      path.join(root, ".decisions/src/yaml.ts.dlog"),
      `# top-level comments are ignored
file: src/yaml.ts
schema: 1
decisions:
  fn:yaml:
    id: "dec_011"
    lines_hint: [1, 3]
    fingerprint: abc123ef
    chose: "double quoted choice"
    because: >-
      folded line one
      folded line two
    rejected:
      - approach: 'single quoted: option # literal'
        reason: |-
          literal first
          literal second
    timestamp: '2026-06-26T20:31:18.340Z'
`,
      "utf8"
    );

    await expect(loadDlog(root, "src/yaml.ts")).resolves.toEqual({
      file: "src/yaml.ts",
      schema: 1,
      decisions: {
        "fn:yaml": {
          id: "dec_011",
          lines_hint: [1, 3],
          fingerprint: "abc123ef",
          chose: "double quoted choice",
          because: "folded line one folded line two",
          rejected: [
            {
              approach: "single quoted: option # literal",
              reason: "literal first\nliteral second"
            }
          ],
          timestamp: "2026-06-26T20:31:18.340Z",
          history: []
        }
      }
    });
  });

  it("captures init settings merge including the archiva MCP server entry", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, ".claude"), { recursive: true });
    await fs.writeFile(
      path.join(root, ".claude", "settings.json"),
      JSON.stringify(
        {
          mcpServers: {
            other: { command: "other-tool", args: ["serve"] },
            archiva: { command: "old-archiva", args: ["old"] }
          },
          hooks: {
            SessionStart: [{ hooks: [{ type: "command", command: "echo existing" }] }]
          }
        },
        null,
        2
      ),
      "utf8"
    );

    await initProject(root);
    const settings = JSON.parse(await fs.readFile(path.join(root, ".claude/settings.json"), "utf8")) as {
      mcpServers: Record<string, { command: string; args: string[] }>;
      hooks: { SessionStart: Array<{ hooks: Array<{ command: string }> }> };
    };

    expect(settings.mcpServers.other).toEqual({ command: "other-tool", args: ["serve"] });
    expect(settings.mcpServers.archiva).toEqual({ command: "archiva", args: ["mcp"] });
    expect(settings.hooks.SessionStart.flatMap((group) => group.hooks.map((hook) => hook.command))).toEqual([
      "echo existing",
      "archiva hooks session-start"
    ]);
  });

  it("captures init AGENTS.md and gitignore text behavior", async () => {
    const root = await tempProject();
    await fs.writeFile(path.join(root, "AGENTS.md"), "Existing notes\n\n", "utf8");
    await fs.writeFile(path.join(root, ".gitignore"), "dist/\n\n", "utf8");

    await initProject(root, { gitignoreDecisions: true });
    await expect(fs.readFile(path.join(root, "AGENTS.md"), "utf8")).resolves.toBe(`Existing notes\n\n${ARCHIVA_AGENTS_BLOCK}`);
    await expect(fs.readFile(path.join(root, ".gitignore"), "utf8")).resolves.toBe("dist/\n.decisions/\n");

    await initProject(root, { gitignoreDecisions: true });
    await expect(fs.readFile(path.join(root, "AGENTS.md"), "utf8")).resolves.toBe(`Existing notes\n\n${ARCHIVA_AGENTS_BLOCK}`);
    await expect(fs.readFile(path.join(root, ".gitignore"), "utf8")).resolves.toBe("dist/\n.decisions/\n");

    const markerRoot = await tempProject();
    const existingAgents = "custom\n## Decision Logging (Archiva)\nkeep\n";
    await fs.writeFile(path.join(markerRoot, "AGENTS.md"), existingAgents, "utf8");
    await initProject(markerRoot);
    await expect(fs.readFile(path.join(markerRoot, "AGENTS.md"), "utf8")).resolves.toBe(existingAgents);
  });

  it("captures JSON parse and stringify behavior used by CLI, MCP, and settings", () => {
    const parsed = JSON.parse(
      '{"z":0,"dup":"first","a":{"escape":"line\\n\\uD83D\\uDE00","quote":"\\""},"dup":"last","arr":[true,false,null,-0,1.25,1e2]}'
    ) as Record<string, unknown>;

    expect(Object.keys(parsed)).toEqual(["z", "dup", "a", "arr"]);
    expect(JSON.stringify(parsed)).toBe(
      `{"z":0,"dup":"last","a":{"escape":"line\\n${"\u{1F600}"}","quote":"\\""},"arr":[true,false,null,0,1.25,100]}`
    );
    expect(JSON.stringify({ jsonrpc: "2.0", id: undefined, result: { ok: true } })).toBe(
      '{"jsonrpc":"2.0","result":{"ok":true}}'
    );
    expect(`${JSON.stringify({ b: 1, a: { c: 2 } }, null, 2)}\n`).toBe(`{
  "b": 1,
  "a": {
    "c": 2
  }
}
`);
    expect(JSON.stringify(JSON.parse("1e21"))).toBe("1e+21");
    expect(JSON.stringify(JSON.parse("1e-7"))).toBe("1e-7");
  });

  it("captures exact why, whyForLine, history, and session-start text", async () => {
    const root = await tempProject();
    const dlog: DlogFile = {
      file: "src/explain.ts",
      schema: 1,
      decisions: {
        "fn:first": {
          id: "dec_001",
          lines_hint: [1, 3],
          fingerprint: "11111111",
          chose: "first approach with extra whitespace",
          because: "first reason",
          rejected: [
            { approach: "class wrapper", reason: "adds no behavior" },
            { approach: "global helper", reason: "hides coupling" },
            { approach: "third hidden", reason: "not shown in session map" }
          ],
          expires_if: "api changes",
          session: "sess_a",
          timestamp: "2026-06-26T20:31:18.340Z",
          history: [
            {
              id: "dec_000",
              chose: "older approach",
              because: "older reason",
              timestamp: "2026-06-25T10:00:00.000Z",
              superseded_reason: "first reason"
            }
          ],
          status: "STALE",
          stale_since: "2026-06-26T21:00:00.000Z"
        },
        "fn:second": {
          id: "dec_002",
          lines_hint: [5, 8],
          fingerprint: "22222222",
          chose: "second approach\nwith newlines and      spaces",
          because: "second reason",
          rejected: [],
          timestamp: "2026-06-26T20:32:18.340Z",
          history: []
        }
      }
    };
    await writeDlog(root, dlog);

    await expect(why(root, "src/explain.ts", "fn:first")).resolves.toBe(`fn:first dec_001 (lines 1-3) [STALE]
Chose: first approach with extra whitespace
Because: first reason
Rejected:
  - class wrapper -> adds no behavior
  - global helper -> hides coupling
  - third hidden -> not shown in session map
Recorded: 2026-06-26T20:31:18.340Z  Session: sess_a
Expires if: api changes`);
    await expect(whyForLine(root, "src/explain.ts", 6)).resolves.toBe(`fn:second dec_002 (lines 5-8)
Chose: second approach
with newlines and      spaces
Because: second reason
Recorded: 2026-06-26T20:32:18.340Z`);
    await expect(history(root, "src/explain.ts", "fn:first")).resolves.toBe(`dec_000 2026-06-25T10:00:00.000Z
  Chose: older approach
  Because: older reason

dec_001 2026-06-26T20:31:18.340Z
  Chose: first approach with extra whitespace
  Because: first reason`);
    await expect(why(root, "src/explain.ts")).resolves.toBe(`fn:first dec_001 (lines 1-3) [STALE]
Chose: first approach with extra whitespace
Because: first reason
Rejected:
  - class wrapper -> adds no behavior
  - global helper -> hides coupling
  - third hidden -> not shown in session map
Recorded: 2026-06-26T20:31:18.340Z  Session: sess_a
Expires if: api changes

fn:second dec_002 (lines 5-8)
Chose: second approach
with newlines and      spaces
Because: second reason
Recorded: 2026-06-26T20:32:18.340Z`);
    await expect(sessionStart(root)).resolves.toBe(`[Archiva] Decision map loaded for 1 files:

src/explain.ts
  1-3 fn:first STALE -> first approach with extra whitespace | x class wrapper(adds no behavior), global helper(hides coupling)
  5-8 fn:second -> second approach with newlines and spaces`);
  });

  it("captures MCP tools/list schema including current session omission", async () => {
    const root = await tempProject();
    await expect(handleRequest(root, "tools/list", {})).resolves.toEqual({
      tools: [
        {
          name: "write_decision",
          description: "Log a decision you just made: what you chose, why, and what you rejected.",
          inputSchema: {
            type: "object",
            required: ["file", "anchor", "lines", "chose", "because", "rejected"],
            properties: {
              file: { type: "string" },
              anchor: { type: "string" },
              lines: { type: "array", items: { type: "number" }, minItems: 2, maxItems: 2 },
              chose: { type: "string" },
              because: { type: "string" },
              rejected: {
                type: "array",
                items: {
                  type: "object",
                  required: ["approach", "reason"],
                  properties: {
                    approach: { type: "string" },
                    reason: { type: "string" }
                  }
                }
              },
              expires_if: { type: "string" },
              supersedes: { type: "string" }
            }
          }
        },
        {
          name: "why",
          description: "Look up the decision log for a file and anchor before modifying it.",
          inputSchema: {
            type: "object",
            required: ["file"],
            properties: {
              file: { type: "string" },
              anchor: { type: "string" }
            }
          }
        },
        {
          name: "ghost_check",
          description: "Check for stale or orphaned decisions in a file.",
          inputSchema: {
            type: "object",
            required: ["file"],
            properties: {
              file: { type: "string" }
            }
          }
        }
      ]
    });
  });

  it("captures MCP stdio edge cases that direct handler tests cannot see", async () => {
    const root = await tempProject();
    const result = runArchiva(
      ["mcp"],
      [
        "{\"jsonrpc\":\"2.0\",\"id\":1}",
        "{\"jsonrpc\":\"2.0\",\"id\":null}",
        "{\"jsonrpc\":\"2.0\"}",
        "{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"id\":99}",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"does/not/exist\"}",
        "{bad"
      ].join("\n"),
      root
    );

    expect(result.status).toBe(0);
    const responses = result.stdout
      .trim()
      .split(/\r?\n/)
      .map((line) => JSON.parse(line) as Record<string, unknown>);

    expect(responses).toHaveLength(4);
    expect(responses[0]).toMatchObject({ jsonrpc: "2.0", id: 1, error: { code: -32600, message: "Missing method" } });
    expect(Object.prototype.hasOwnProperty.call(responses[1], "id")).toBe(false);
    expect(responses[1]).toMatchObject({
      jsonrpc: "2.0",
      result: { protocolVersion: "2024-11-05", serverInfo: { name: "archiva" } }
    });
    expect(responses[2]).toMatchObject({
      jsonrpc: "2.0",
      id: 2,
      error: { code: -32000, message: "Unsupported MCP method: does/not/exist" }
    });
    expect(responses[3]).toMatchObject({ jsonrpc: "2.0", id: null, error: { code: -32700 } });
  });

  it("captures CLI negative behavior for missing post-tool-use target", async () => {
    const root = await tempProject();
    const result = runArchiva(["hooks", "post-tool-use"], "", root, { ARCHIVA_FILE: "" });

    expect(result.status).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr.trim()).toBe("Missing file path. Pass one or set ARCHIVA_FILE.");
  });

  it("captures CLI help and Commander-style negative surfaces", async () => {
    const root = await tempProject();

    expect(runArchiva(["nope"], "", root)).toMatchObject({
      status: 1,
      stdout: "",
      stderr: "error: unknown command 'nope'\n"
    });
    expect(runArchiva(["init", "--bad"], "", root)).toMatchObject({
      status: 1,
      stdout: "",
      stderr: "error: unknown option '--bad'\n"
    });
    expect(runArchiva(["why"], "", root)).toMatchObject({
      status: 1,
      stdout: "",
      stderr: "error: missing required argument 'file'\n"
    });

    expect(runArchiva(["--help"], "", root)).toMatchObject({
      status: 0,
      stderr: "",
      stdout: `Usage: archiva [options] [command]

Decision layer for agentic codebases.

Options:
  -V, --version              output the version number
  -h, --help                 display help for command

Commands:
  init [options]             Set up Archiva in the current project
  status                     Show decision health across the repo
  why <file> [lineOrAnchor]  Explain why code was written
  history <file> <anchor>    Show the decision history chain for an anchor
  lint [options]             Run decision lint rules
  hooks                      Run Archiva hook commands
  mcp                        Start the Archiva MCP server over stdio
  write-decision [options]   Record a decision from JSON on stdin or --json
  help [command]             display help for command
`
    });

    expect(runArchiva(["hooks", "--help"], "", root)).toMatchObject({
      status: 0,
      stderr: "",
      stdout: `Usage: archiva hooks [options] [command]

Run Archiva hook commands

Options:
  -h, --help            display help for command

Commands:
  session-start         Print compact decision context
  post-tool-use [file]  Re-anchor decisions after a file edit
  help [command]        display help for command
`
    });
  });

  it("captures write-decision JSON parse and validation errors", async () => {
    const root = await tempProject();
    const invalidFromOption = runArchiva(["write-decision", "--json", "{bad"], "", root);
    const invalidFromStdin = runArchiva(["write-decision"], "{bad", root);
    const invalidSchema = runArchiva(
      ["write-decision"],
      JSON.stringify({
        file: "src/x.ts",
        anchor: "fn:x",
        lines: [1, 1],
        chose: "",
        because: "b",
        rejected: []
      }),
      root
    );

    expect(invalidFromOption.status).toBe(1);
    expect(invalidFromOption.stdout).toBe("");
    expect(invalidFromOption.stderr.trim()).toBe(
      "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
    );
    expect(invalidFromStdin.status).toBe(1);
    expect(invalidFromStdin.stdout).toBe("");
    expect(invalidFromStdin.stderr.trim()).toBe(
      "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
    );
    expect(invalidSchema.status).toBe(1);
    expect(invalidSchema.stdout).toBe("");
    expect(JSON.parse(invalidSchema.stderr)).toEqual([
      {
        code: "too_small",
        minimum: 1,
        type: "string",
        inclusive: true,
        exact: false,
        message: "String must contain at least 1 character(s)",
        path: ["chose"]
      }
    ]);
  });

  it("documents current path normalization quirks that Rust v2 intends to harden", async () => {
    const root = await tempProject();

    expect(normalizeRelativePath("./src/a.ts")).toBe("src/a.ts");
    expect(normalizeRelativePath(".//src/a.ts")).toBe("src/a.ts");
    expect(normalizeRelativePath("src\\a.ts")).toBe("src/a.ts");
    expect(normalizeRelativePath("src//a.ts")).toBe("src//a.ts");
    expect(normalizeRelativePath("src/./a.ts")).toBe("src/./a.ts");
    expect(normalizeRelativePath("src/../a.ts")).toBe("src/../a.ts");
    expect(normalizeRelativePath("src/../../escape.ts")).toBe("src/../../escape.ts");
    expect(normalizeRelativePath("C:\\repo\\file.ts")).toBe("C:/repo/file.ts");
    expect(normalizeRelativePath("CON.ts")).toBe("CON.ts");
    expect(normalizeRelativePath("src/NUL.txt")).toBe("src/NUL.txt");
    expect(normalizeRelativePath(".")).toBe(".");
    expect(normalizeRelativePath("a\0b.ts")).toBe("a\0b.ts");
    expect(sourcePath(root, "src/../../escape.ts")).toBe(path.join(root, "src/../../escape.ts"));
    expect(() => normalizeRelativePath("")).toThrow('Expected a project-relative path, got ""');
    expect(() => normalizeRelativePath("./")).toThrow('Expected a project-relative path, got "./"');
    expect(() => normalizeRelativePath("../outside.ts")).toThrow('Expected a project-relative path, got "../outside.ts"');
    expect(() => normalizeRelativePath("/tmp/outside.ts")).toThrow('Expected a project-relative path, got "/tmp/outside.ts"');
    expect(() => normalizeRelativePath("\\\\server\\share\\file.ts")).toThrow(
      'Expected a project-relative path, got "\\\\server\\share\\file.ts"'
    );
  });

  it("captures current gitignore matcher quirks used by lint scans", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src/generated"), { recursive: true });
    await fs.writeFile(path.join(root, ".gitignore"), "ignored.ts\n!rescue.ts\nsrc/generated/\n*.test.ts\n", "utf8");
    for (const file of ["ignored.ts", "rescue.ts", "src/generated/a.ts", "src/a.test.ts", "src/a.ts"]) {
      await fs.writeFile(path.join(root, file), "function x() {}\n", "utf8");
    }

    const isIgnored = await loadGitignoreMatcher(root);
    expect(isIgnored("ignored.ts")).toBe(true);
    expect(isIgnored("rescue.ts")).toBe(false);
    expect(isIgnored("src/generated/a.ts")).toBe(false);
    expect(isIgnored("src/a.test.ts")).toBe(true);
    expect(isIgnored("src/a.ts")).toBe(false);
    await expect(listLintSourceFiles(root).then((files) => files.map((file) => absoluteToRelative(root, file)))).resolves.toEqual([
      "rescue.ts",
      "src/a.ts",
      "src/generated/a.ts"
    ]);
  });

  it("captures current line range shifting semantics across edit positions", () => {
    expect(applyDiffToRange("a\nb\nc\n", "x\na\nb\nc\n", [2, 3])).toEqual([3, 4]);
    expect(applyDiffToRange("a\nb\nc\n", "a\nx\nb\nc\n", [2, 3])).toEqual([3, 4]);
    expect(applyDiffToRange("a\nb\nc\n", "a\nb\nx\nc\n", [2, 3])).toEqual([2, 3]);
    expect(applyDiffToRange("a\nb\nc\nd\n", "b\nc\nd\n", [3, 4])).toEqual([2, 3]);
    expect(applyDiffToRange("a\nb\nc\nd\n", "a\nc\nd\n", [2, 4])).toEqual([2, 4]);
    expect(applyDiffToRange("a\nb\nc\nd\n", "a\nb\nd\n", [2, 4])).toEqual([2, 4]);
    expect(applyDiffToRange("a\nb\nc\n", "x\nb\nc\n", [2, 3])).toEqual([2, 3]);
    expect(applyDiffToRange("a\nb\nc\n", "a\nx\nc\n", [2, 3])).toEqual([2, 3]);
    expect(applyDiffToRange("a\nb\nc", "a\nx\nb\nc", [2, 3])).toEqual([3, 4]);
    expect(applyDiffToRange("a\r\nb\r\nc\r\n", "a\r\nx\r\nb\r\nc\r\n", [2, 3])).toEqual([3, 4]);
  });

  it("captures TypeScript anchor edge cases for future parser parity", () => {
    const anchors = extractAnchors(
      "src/edge.ts",
      `
export default function namedDefault() { return 1; }
export function overloaded(x: string): string;
export function overloaded(x: number): number;
export function overloaded(x) { return x; }
class Store {
  constructor() {}
  private save() { return true; }
  static load() { return true; }
  get value() { return 1; }
  field = () => true;
}
const obj = { method() { return true; } };
function outer() { function inner() { return true; } return inner(); }
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:namedDefault",
      "fn:overloaded",
      "fn:outer",
      "class:Store",
      "fn:Store.save",
      "fn:Store.load",
      "export:default",
      "export:overloaded"
    ]);
    expect(anchors["export:overloaded"]?.start).toBe(3);
    expect(anchors["fn:Store.value"]).toBeUndefined();
    expect(anchors["fn:inner"]).toBeUndefined();
  });

  it("captures modern TypeScript export and method anchor edges", () => {
    const anchors = extractAnchors(
      "src/modern.ts",
      `
export async function fetchData() { await run(); }
export default function() { return 1; }
export default () => true;
export const value = 1;
const local = () => true;
export { local as renamed };
class C {
  async save() { return true; }
  *items() { yield 1; }
  [computed]() { return 1; }
  #secret() { return 2; }
  declare only(): void;
}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:fetchData",
      "class:C",
      "fn:C.save",
      "fn:C.items",
      "fn:C.[computed]",
      "fn:C.#secret",
      "fn:local",
      "export:fetchData",
      "export:default",
      "export:value",
      "export:renamed"
    ]);
    expect(anchors["export:default"]?.start).toBe(3);
    expect(anchors["export:value"]?.start).toBe(5);
    expect(anchors["export:renamed"]?.start).toBe(6);
    expect(anchors["fn:C.only"]).toBeUndefined();
  });

  it("captures typed variable initializer anchor edges", () => {
    const topLevel = extractAnchors(
      "src/typed-initializers.ts",
      `const objectTyped: { a: number, b: number } = () => true;
const tupleTyped: [number, string] = function hiddenTuple() { return [1, "x"]; };
export const genericTyped: Promise<string | number> = async () => "x";
const unionTyped: (() => number) | null = () => 1;
const plain = () => true;
function after() {}
`
    );

    expect(Object.keys(topLevel)).toEqual([
      "fn:after",
      "fn:objectTyped",
      "fn:tupleTyped",
      "fn:genericTyped",
      "fn:unionTyped",
      "fn:plain",
      "export:genericTyped"
    ]);
    expect([
      topLevel["fn:objectTyped"]?.start,
      topLevel["fn:tupleTyped"]?.start,
      topLevel["fn:genericTyped"]?.start,
      topLevel["export:genericTyped"]?.start,
      topLevel["fn:unionTyped"]?.start,
      topLevel["fn:plain"]?.start,
      topLevel["fn:after"]?.start
    ]).toEqual([1, 2, 3, 3, 4, 5, 6]);
    for (const ghost of ["fn:b", "fn:string", "fn:hiddenTuple"]) {
      expect(topLevel[ghost]).toBeUndefined();
    }

    const namespace = extractAnchors(
      "src/ns-typed-initializers.ts",
      `namespace Local {
  const objectTyped: { a: number, b: number } = () => true;
  const tupleTyped: [number, string] = function hiddenTuple() { return [1, "x"]; };
  export { objectTyped, tupleTyped };
}
export = Local;
function after() {}
`
    );

    expect(Object.keys(namespace)).toEqual(["fn:after", "export:objectTyped", "export:tupleTyped"]);
    expect([
      namespace["fn:after"]?.start,
      namespace["export:objectTyped"]?.start,
      namespace["export:tupleTyped"]?.start
    ]).toEqual([7, 2, 3]);
    for (const ghost of ["export:b", "export:string", "fn:hiddenTuple"]) {
      expect(namespace[ghost]).toBeUndefined();
    }
  });

  it("captures type-only question token complexity edges", () => {
    const anchors = extractAnchors(
      "src/type-question-complexity.ts",
      `function optionalParam(x?: string) { return x; }
const optionalArrow = (x?: string) => x;
function conditionalType(x: T extends string ? A : B) { return x; }
const conditionalArrow = (x: T extends string ? A : B) => x;
function defaultParam(x = flag ? 1 : 2) { return x; }
const defaultArrow = (x = flag ? 1 : 2) => x;
class C { optional?() { return 1; } method(x?: T, y: T extends string ? A : B) { return y; } }
class Computed { [bad ? { x: 1 } : key]() {} }
function optionalChain(x: any) { return x?.value; }
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:optionalParam",
      "fn:conditionalType",
      "fn:defaultParam",
      "fn:optionalChain",
      "class:C",
      "fn:C.optional",
      "fn:C.method",
      "class:Computed",
      "fn:Computed.[bad ? { x: 1 } : key]",
      "fn:optionalArrow",
      "fn:conditionalArrow",
      "fn:defaultArrow"
    ]);
    expect([
      anchors["fn:optionalParam"]?.complexity,
      anchors["fn:optionalArrow"]?.complexity,
      anchors["fn:conditionalType"]?.complexity,
      anchors["fn:conditionalArrow"]?.complexity,
      anchors["class:C"]?.complexity,
      anchors["fn:C.method"]?.complexity,
      anchors["fn:defaultParam"]?.complexity,
      anchors["fn:defaultArrow"]?.complexity,
      anchors["fn:Computed.[bad ? { x: 1 } : key]"]?.complexity,
      anchors["fn:optionalChain"]?.complexity
    ]).toEqual([1, 1, 1, 1, 1, 1, 2, 2, 2, 1]);
  });

  it("captures type-like, declare, namespace, and export ordering anchor edges", () => {
    const anchors = extractAnchors(
      "src/exports.ts",
      `
export const run = () => true;
export function handle() {}
export class Box { open() {} }
export interface I {}
export type T = string;
export enum E { A }
export const enum CE { A }
export namespace N { export function inside() {} }
export declare function declared(x: string): string;
export declare const declaredValue: number;
export declare class Declared { run(): void; }
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:handle",
      "fn:declared",
      "class:Box",
      "fn:Box.open",
      "class:Declared",
      "fn:Declared.run",
      "fn:run",
      "export:handle",
      "export:declared",
      "export:run",
      "export:Box",
      "export:I",
      "export:T",
      "export:E",
      "export:CE",
      "export:N",
      "export:declaredValue",
      "export:Declared"
    ]);
    expect(anchors["export:I"]?.start).toBe(5);
    expect(anchors["export:T"]?.start).toBe(6);
    expect(anchors["export:E"]?.start).toBe(7);
    expect(anchors["export:CE"]?.start).toBe(8);
    expect(anchors["export:N"]?.start).toBe(9);
    expect(anchors["fn:Declared.run"]?.start).toBe(12);
    expect(anchors["fn:inside"]).toBeUndefined();
  });

  it("captures default type-like export edges", () => {
    const anchors = extractAnchors(
      "src/default-type-like.ts",
      `export default interface I { x: number }
export { I as NamedI };
export default enum E { A }
export { E as NamedE };
export default type T = string;
export { T as NamedT };
export default namespace N { export const x = 1; }
export { N as NamedN };
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual(["fn:after", "export:default", "export:NamedI", "export:NamedE"]);
    expect([
      anchors["export:default"]?.start,
      anchors["export:NamedI"]?.start,
      anchors["export:NamedE"]?.start,
      anchors["fn:after"]?.start
    ]).toEqual([1, 1, 3, 9]);
    for (const ghost of ["export:I", "export:E", "export:T", "export:N", "export:NamedT", "export:NamedN"]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("captures decorator line ranges for class, export, and method anchors", () => {
    const anchors = extractAnchors(
      "src/decorators.ts",
      `@sealed({
  role: "api"
})
export class Service {
  @logged({
    level: "info"
  })
  run() {}
}
class Local {
  @logged
  [compute]() {}
  @secret
  #run() {}
}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "class:Service",
      "fn:Service.run",
      "class:Local",
      "fn:Local.[compute]",
      "fn:Local.#run",
      "export:Service"
    ]);
    expect([anchors["class:Service"]?.start, anchors["class:Service"]?.end]).toEqual([1, 9]);
    expect([anchors["fn:Service.run"]?.start, anchors["fn:Service.run"]?.end]).toEqual([5, 8]);
    expect(anchors["export:Service"]?.start).toBe(1);
    expect(anchors["fn:Local.[compute]"]?.start).toBe(11);
    expect(anchors["fn:Local.#run"]?.start).toBe(13);
  });

  it("suppresses class field initializer function anchors", () => {
    const anchors = extractAnchors(
      "src/class-fields.ts",
      `class C {
  field = () => true;
  static staticField = function hidden() { return 1; };
  #privateField = function privateHidden() { return 2; };
  [fieldName] = function hiddenComputed() { return 3; };
  "quotedField" = function hiddenQuoted() { return 4; };
  accessor auto = 1;
  method() {}
  #method() {}
  [methodName]() {}
  "quotedMethod"() {}
}
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:after",
      "class:C",
      "fn:C.method",
      "fn:C.#method",
      "fn:C.[methodName]",
      'fn:C."quotedMethod"'
    ]);
    expect([
      anchors["class:C"]?.start,
      anchors["class:C"]?.end,
      anchors["fn:C.method"]?.start,
      anchors["fn:C.#method"]?.start,
      anchors["fn:C.[methodName]"]?.start,
      anchors['fn:C."quotedMethod"']?.start,
      anchors["fn:after"]?.start
    ]).toEqual([1, 12, 8, 9, 10, 11, 13]);
    for (const ghost of [
      "fn:C.field",
      "fn:C.staticField",
      "fn:C.hidden",
      "fn:C.privateHidden",
      "fn:C.hiddenComputed",
      "fn:C.hiddenQuoted",
      "fn:C.auto"
    ]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("captures semicolonless class field recovery edges", () => {
    const semicolonless = extractAnchors(
      "src/semicolonless-class-fields.ts",
      `class C {
  field = () => true
  method() { return 1; }
  other = function hidden() { return 2; }
  #privateField = () => true
  #method() { return 3; }
  [computed] = function hiddenComputed() { return 4; }
  [methodName]() { return 5; }
}
function after() {}
`
    );

    expect(Object.keys(semicolonless)).toEqual(["fn:after", "class:C", "fn:C.method", "fn:C.#method"]);
    expect([
      semicolonless["class:C"]?.start,
      semicolonless["class:C"]?.end,
      semicolonless["fn:C.method"]?.start,
      semicolonless["fn:C.#method"]?.start,
      semicolonless["fn:after"]?.start
    ]).toEqual([1, 8, 3, 6, 10]);
    for (const ghost of ["fn:C.field", "fn:C.hidden", "fn:C.privateField", "fn:C.hiddenComputed", "fn:C.[methodName]"]) {
      expect(semicolonless[ghost]).toBeUndefined();
    }

    const multiline = extractAnchors(
      "src/multiline-class-fields.ts",
      `class C {
  field =
    () => true
  method() { return 1; }
  data = {
    run() { return 2; }
  }
  afterData() { return 3; }
}
function after() {}
`
    );

    expect(Object.keys(multiline)).toEqual(["fn:after", "class:C", "fn:C.method", "fn:C.afterData"]);
    expect([
      multiline["class:C"]?.start,
      multiline["class:C"]?.end,
      multiline["fn:C.method"]?.start,
      multiline["fn:C.afterData"]?.start,
      multiline["fn:after"]?.start
    ]).toEqual([1, 9, 4, 8, 10]);
    expect(multiline["fn:C.run"]).toBeUndefined();
    expect(multiline["fn:C.field"]).toBeUndefined();
  });

  it("captures semicolonless field before generator recovery", () => {
    const semicolonless = extractAnchors(
      "src/field-followed-by-generator.ts",
      `class C {
  field = 1
  *items() { yield 1; }
  other = 2
  async save() { return 1; }
}
function after() {}
`
    );

    expect(Object.keys(semicolonless)).toEqual(["fn:after", "class:C"]);
    expect([
      semicolonless["class:C"]?.start,
      semicolonless["class:C"]?.end,
      semicolonless["fn:after"]?.start
    ]).toEqual([1, 3, 7]);
    expect(semicolonless["fn:C.items"]).toBeUndefined();
    expect(semicolonless["fn:C.save"]).toBeUndefined();

    const semicolon = extractAnchors(
      "src/field-followed-by-generator-semicolon.ts",
      `class C {
  field = 1;
  *items() { yield 1; }
}
function after() {}
`
    );

    expect(Object.keys(semicolon)).toEqual(["fn:after", "class:C", "fn:C.items"]);
    expect([
      semicolon["class:C"]?.start,
      semicolon["class:C"]?.end,
      semicolon["fn:C.items"]?.start,
      semicolon["fn:after"]?.start
    ]).toEqual([1, 4, 3, 5]);
  });

  it("captures anonymous default class method anchors", () => {
    const anchors = extractAnchors(
      "src/anon-default.ts",
      `@sealed
export default class {
  @logged
  open() {}
  *items() { yield 1; }
  [compute]() {}
  #secret() {}
  get value() { return 1; }
  field = () => true;
}
class Later { m() {} }
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:open",
      "fn:items",
      "fn:[compute]",
      "fn:#secret",
      "class:Later",
      "fn:Later.m",
      "export:default"
    ]);
    expect([anchors["fn:open"]?.start, anchors["fn:open"]?.end]).toEqual([3, 4]);
    expect([anchors["export:default"]?.start, anchors["export:default"]?.end]).toEqual([1, 10]);
    expect(anchors["fn:value"]).toBeUndefined();
    expect(anchors["fn:field"]).toBeUndefined();
  });

  it("suppresses class expression parser ghosts", () => {
    const anchors = extractAnchors(
      "src/class-expressions.ts",
      `const Plain = class { method() {} };
const Named = class Inner { method() {} };
export const Exported = class ExportedInner { method() {} };
const Nested = foo(class NestedInner { method() {} });
class Real { method() {} }
function later() {}
`
    );

    expect(Object.keys(anchors)).toEqual(["fn:later", "class:Real", "fn:Real.method", "export:Exported"]);
    expect([anchors["export:Exported"]?.start, anchors["class:Real"]?.start, anchors["fn:later"]?.start]).toEqual([
      3,
      5,
      6
    ]);
    for (const ghost of [
      "class:Inner",
      "fn:Inner.method",
      "class:ExportedInner",
      "fn:ExportedInner.method",
      "class:NestedInner",
      "fn:NestedInner.method",
      "fn:Plain.method"
    ]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("captures optional class method anchor edges", () => {
    const anchors = extractAnchors(
      "src/optional-methods.ts",
      `class C {
  optional?() { return 1; }
  required() { return 2; }
  "quoted"?() { return 3; }
  42?() { return 4; }
  [computed]?() { return 5; }
  #secret?() { return 6; }
}
abstract class AbstractC { abstract optional?(): void; abstract required(): void; concrete() {} }
declare class DeclaredC { optional?(): void; required(): void; }
class SignatureOnly { optional?(): void; required(): void; concrete() {} }
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:after",
      "class:C",
      "fn:C.optional",
      "fn:C.required",
      'fn:C."quoted"',
      "fn:C.42",
      "fn:C.[computed]",
      "fn:C.#secret",
      "class:AbstractC",
      "fn:AbstractC.optional",
      "fn:AbstractC.required",
      "fn:AbstractC.concrete",
      "class:DeclaredC",
      "fn:DeclaredC.optional",
      "fn:DeclaredC.required",
      "class:SignatureOnly",
      "fn:SignatureOnly.concrete"
    ]);
    expect([
      anchors["fn:after"]?.start,
      anchors["class:C"]?.start,
      anchors["class:C"]?.end,
      anchors["fn:C.optional"]?.start,
      anchors["fn:C.[computed]"]?.start,
      anchors["fn:AbstractC.optional"]?.start,
      anchors["fn:DeclaredC.optional"]?.start,
      anchors["fn:SignatureOnly.concrete"]?.start
    ]).toEqual([12, 1, 8, 2, 6, 9, 10, 11]);
    expect(anchors["fn:SignatureOnly.optional"]).toBeUndefined();
    expect(anchors["fn:SignatureOnly.required"]).toBeUndefined();
  });

  it("captures TSX JSX anchor ranges and complexity edges", () => {
    const anchors = extractAnchors(
      "src/view.tsx",
      `
export function View(props: { ok: boolean }) {
  return <div>{props.ok ? <span/> : null}</div>;
}
export const Panel = (props: { ok: boolean }) => (
  <section>{props.ok && <span>{props.ok ? "yes" : "no"}</span>}</section>
);
const FragmentView = () => <>
  <span/>
  <span/>
</>;
export class Screen {
  render() {
    return <main>{items.map((item) => <span key={item.id}>{item.name}</span>)}</main>;
  }
}
function WithBlock(props) {
  if (props.ok && props.ready || props.admin) {
    return <div>{props.ok ? <span/> : null}</div>;
  }
  return null;
}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:View",
      "fn:WithBlock",
      "class:Screen",
      "fn:Screen.render",
      "fn:Panel",
      "fn:FragmentView",
      "export:View",
      "export:Panel",
      "export:Screen",
      "block:if_props.ok_props.ready_props.admin"
    ]);
    expect([anchors["fn:View"]?.start, anchors["fn:View"]?.end, anchors["fn:View"]?.complexity]).toEqual([2, 4, 2]);
    expect([anchors["fn:Panel"]?.start, anchors["fn:Panel"]?.end, anchors["fn:Panel"]?.complexity]).toEqual([5, 7, 3]);
    expect([anchors["fn:FragmentView"]?.start, anchors["fn:FragmentView"]?.end]).toEqual([8, 11]);
    expect([anchors["fn:Screen.render"]?.start, anchors["fn:Screen.render"]?.end]).toEqual([13, 15]);
    expect([anchors["fn:WithBlock"]?.start, anchors["fn:WithBlock"]?.end, anchors["fn:WithBlock"]?.complexity]).toEqual([
      17,
      22,
      5
    ]);
    expect([anchors["block:if_props.ok_props.ready_props.admin"]?.start, anchors["block:if_props.ok_props.ready_props.admin"]?.end]).toEqual([
      18,
      20
    ]);
  });

  it("captures import, re-export, and regex literal anchor edges", () => {
    const anchors = extractAnchors(
      "src/imports-regex.ts",
      `interface LocalType {}
type LocalAlias = string;
const local = () => true;
export type { LocalType, LocalAlias };
export { local as remoteName } from "./dep";
export { local as renamed };
export * from "./remote";
export * as remoteNs from "./remote";
const regex = /function fake() { return true; }/;
function real() {
  return /if (fake && other || third) { return true; }/.test("x");
}
const tpl = \`class Imaginary { run() {} }\`;
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "fn:local",
      "export:LocalType",
      "export:LocalAlias",
      "export:renamed"
    ]);
    expect([anchors["fn:real"]?.start, anchors["fn:real"]?.end, anchors["fn:real"]?.complexity]).toEqual([
      10,
      12,
      1
    ]);
    expect([anchors["fn:local"]?.start, anchors["fn:local"]?.end]).toEqual([3, 3]);
    expect([anchors["export:LocalType"]?.start, anchors["export:LocalAlias"]?.start]).toEqual([1, 2]);
    expect(anchors["export:remoteName"]).toBeUndefined();
    expect(anchors["fn:fake"]).toBeUndefined();
    expect(anchors["block:if_fake_other_third"]).toBeUndefined();

    const importEquals = extractAnchors(
      "src/import-equals.ts",
      `import Alias = require("dep");
import Other = ns.Other;
import { "dash-name" as dashName, regular as regularName } from "./dep";
export { Alias, Other as ExportedOther, dashName, regularName };
export type { Alias as AliasType };
function real() {}
`
    );

    expect(Object.keys(importEquals)).toEqual(["fn:real", "export:Alias", "export:ExportedOther", "export:AliasType"]);
    expect([
      importEquals["export:Alias"]?.start,
      importEquals["export:ExportedOther"]?.start,
      importEquals["export:AliasType"]?.start
    ]).toEqual([1, 2, 1]);
    expect(importEquals["export:dashName"]).toBeUndefined();
    expect(importEquals["export:regularName"]).toBeUndefined();

    const importTypeEquals = extractAnchors(
      "src/import-type-equals.ts",
      `import Alias = require("dep");
import Other = ns.Other;
import type TypeAlias = require("types");
import type BareType;
import type ImportedDefault from "dep";
import type { ImportedNamed } from "dep";
export { Alias, Other as ExportedOther, TypeAlias, BareType as BareAgain, ImportedDefault as DefaultAgain, ImportedNamed as NamedAgain };
export type { Alias as AliasType, TypeAlias as TypeAliasType };
function real() {}
`
    );

    expect(Object.keys(importTypeEquals)).toEqual([
      "fn:real",
      "export:Alias",
      "export:ExportedOther",
      "export:TypeAlias",
      "export:BareAgain",
      "export:AliasType",
      "export:TypeAliasType"
    ]);
    expect([
      importTypeEquals["export:TypeAlias"]?.start,
      importTypeEquals["export:BareAgain"]?.start,
      importTypeEquals["export:TypeAliasType"]?.start
    ]).toEqual([3, 4, 3]);
    expect(importTypeEquals["export:DefaultAgain"]).toBeUndefined();
    expect(importTypeEquals["export:NamedAgain"]).toBeUndefined();

    const malformedImportAlias = extractAnchors(
      "src/import-malformed-alias.ts",
      `import Broken Local;
import Bare;
import FromLike from "dep";
import Comma, { Other } from "dep";
export { Broken as BrokenAgain, Bare as BareAgain, FromLike as FromAgain, Comma as CommaAgain };
function real() {}
`
    );

    expect(Object.keys(malformedImportAlias)).toEqual(["fn:real", "export:BrokenAgain", "export:BareAgain"]);
    expect([
      malformedImportAlias["export:BrokenAgain"]?.start,
      malformedImportAlias["export:BareAgain"]?.start
    ]).toEqual([1, 2]);
    expect(malformedImportAlias["export:FromAgain"]).toBeUndefined();
    expect(malformedImportAlias["export:CommaAgain"]).toBeUndefined();

    const importAttributes = extractAnchors(
      "src/import-attrs.ts",
      `const local = () => true;
export { local as remoteName } from "./dep" with { type: "json" };
import { value as importedValue } from "./dep" with { type: "json" };
export { importedValue };
export { local as renamed };
function real() {}
`
    );

    expect(Object.keys(importAttributes)).toEqual(["fn:real", "fn:local", "export:renamed"]);
    expect([
      importAttributes["fn:real"]?.start,
      importAttributes["fn:local"]?.start,
      importAttributes["export:renamed"]?.start
    ]).toEqual([6, 1, 1]);
    expect(importAttributes["export:remoteName"]).toBeUndefined();
    expect(importAttributes["export:importedValue"]).toBeUndefined();
  });

  it("captures export import and export assignment namespace member edges", () => {
    const anchors = extractAnchors(
      "src/export-variants.ts",
      `namespace Local {
  export const value = () => true;
  export function run() {}
  export class Box { open() {} }
}
export import ExportedAlias = Local;
export import Other = Local.Box;
export { ExportedAlias as AliasAgain };
export type { Other as OtherType };
export import Req = require(
  "dep"
);
export { Req, Req as ReqAgain };
export type { Req as ReqType };
export import Multi = Local
  .Box;
export { Multi as MultiAgain };
export import Duplicate = require("a");
export import Duplicate = require("b");
export { Duplicate as DuplicateAgain };
export = Local;
export as namespace Archiva;
function real() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "export:run",
      "export:value",
      "export:Box",
      "export:ExportedAlias",
      "export:Other",
      "export:AliasAgain",
      "export:OtherType",
      "export:Req",
      "export:ReqAgain",
      "export:ReqType",
      "export:Multi",
      "export:MultiAgain",
      "export:Duplicate",
      "export:DuplicateAgain"
    ]);
    expect([anchors["export:run"]?.start, anchors["export:value"]?.start, anchors["export:Box"]?.start]).toEqual([
      3,
      2,
      4
    ]);
    expect([
      anchors["export:ExportedAlias"]?.start,
      anchors["export:Other"]?.start,
      anchors["export:AliasAgain"]?.start,
      anchors["export:OtherType"]?.start,
      anchors["export:Req"]?.start,
      anchors["export:Req"]?.end,
      anchors["export:ReqAgain"]?.start,
      anchors["export:ReqType"]?.start,
      anchors["export:Multi"]?.start,
      anchors["export:Multi"]?.end,
      anchors["export:MultiAgain"]?.start,
      anchors["export:Duplicate"]?.start,
      anchors["export:DuplicateAgain"]?.start
    ]).toEqual([6, 7, 6, 7, 10, 12, 10, 10, 15, 16, 15, 18, 18]);
    expect(anchors["export:Archiva"]).toBeUndefined();
    expect(anchors["fn:Local.run"]).toBeUndefined();

    const exportImportType = extractAnchors(
      "src/export-import-type.ts",
      `export import type Alias = require("types");
export import type Bare;
export import type Qualified = NS.Sub;
export import type Multi = require(
  "dep"
);
export import type = require("weird");
function real() {}
`
    );

    expect(Object.keys(exportImportType)).toEqual([
      "fn:real",
      "export:Alias",
      "export:Bare",
      "export:Qualified",
      "export:Multi",
      "export:type"
    ]);
    expect([
      exportImportType["export:Alias"]?.start,
      exportImportType["export:Bare"]?.start,
      exportImportType["export:Qualified"]?.start,
      exportImportType["export:Multi"]?.start,
      exportImportType["export:Multi"]?.end,
      exportImportType["export:type"]?.start
    ]).toEqual([1, 2, 3, 4, 6, 7]);

    const malformedExportImport = extractAnchors(
      "src/export-import-malformed.ts",
      `export import Broken Local;
export import Bare;
export import FromLike from "dep";
export import Comma, Other = require("dep");
export { Broken as BrokenAgain, Bare as BareAgain, FromLike as FromAgain, Comma as CommaAgain };
function real() {}
`
    );

    expect(Object.keys(malformedExportImport)).toEqual([
      "fn:real",
      "export:Broken",
      "export:Bare",
      "export:BrokenAgain",
      "export:BareAgain"
    ]);
    expect([
      malformedExportImport["export:Broken"]?.start,
      malformedExportImport["export:Bare"]?.start,
      malformedExportImport["export:BrokenAgain"]?.start,
      malformedExportImport["export:BareAgain"]?.start
    ]).toEqual([1, 2, 1, 2]);
    expect(malformedExportImport["export:FromLike"]).toBeUndefined();
    expect(malformedExportImport["export:Comma"]).toBeUndefined();
    expect(malformedExportImport["export:FromAgain"]).toBeUndefined();
    expect(malformedExportImport["export:CommaAgain"]).toBeUndefined();

    const enumExportAssignment = extractAnchors(
      "src/enum-export-assignment.ts",
      `enum Local {
  A = 1 +
    2,
  B,
  "dash-name" = "x",
  1 = 1,
  [key] = 3
}
namespace Local { export function run() {} }
export = Local;
function real() {}
`
    );

    expect(Object.keys(enumExportAssignment)).toEqual([
      "fn:real",
      "export:A",
      "export:B",
      "export:dash-name",
      "export:1",
      "export:run"
    ]);
    expect([
      enumExportAssignment["export:A"]?.start,
      enumExportAssignment["export:A"]?.end,
      enumExportAssignment["export:B"]?.start,
      enumExportAssignment["export:dash-name"]?.start,
      enumExportAssignment["export:1"]?.start,
      enumExportAssignment["export:run"]?.start
    ]).toEqual([2, 3, 4, 5, 6, 9]);
    expect(enumExportAssignment["export:key"]).toBeUndefined();

    const exportedEnumAssignment = extractAnchors(
      "src/exported-enum-assignment.ts",
      `export enum Local { A }
export = Local;
function real() {}
`
    );

    expect(Object.keys(exportedEnumAssignment)).toEqual(["fn:real", "export:A", "export:Local"]);

    const exportAssignmentOrder = extractAnchors(
      "src/export-assignment-order.ts",
      `export const other = 1;
namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(exportAssignmentOrder)).toEqual(["fn:after", "export:value", "export:other"]);

    const exportedEnumMergeAssignment = extractAnchors(
      "src/exported-enum-merge-assignment.ts",
      `export enum Local { A }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(exportedEnumMergeAssignment)).toEqual(["fn:after", "export:value", "export:Local"]);
    expect(exportedEnumMergeAssignment["export:A"]).toBeUndefined();

    const exportedNamespaceMergeAssignment = extractAnchors(
      "src/exported-namespace-merge-assignment.ts",
      `export namespace Local { export const self = 1; }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(exportedNamespaceMergeAssignment)).toEqual(["fn:after", "export:value", "export:Local"]);
    expect(exportedNamespaceMergeAssignment["export:self"]).toBeUndefined();

    const exportNamespaceOnlyAssignment = extractAnchors(
      "src/export-namespace-only-assignment.ts",
      `export namespace Local { export const self = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(exportNamespaceOnlyAssignment)).toEqual(["fn:after", "export:self", "export:Local"]);

    const plainEnumExportNamespaceAssignment = extractAnchors(
      "src/plain-enum-export-namespace-assignment.ts",
      `enum Local { A }
export namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(plainEnumExportNamespaceAssignment)).toEqual(["fn:after", "export:A", "export:Local"]);
    expect(plainEnumExportNamespaceAssignment["export:value"]).toBeUndefined();

    const exportedFunctionNamespaceAssignment = extractAnchors(
      "src/exported-function-namespace-assignment.ts",
      `export function Local() { return 1; }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(exportedFunctionNamespaceAssignment)).toEqual([
      "fn:Local",
      "fn:after",
      "export:value",
      "export:Local"
    ]);
    expect(exportedFunctionNamespaceAssignment["export:default"]).toBeUndefined();

    const exportedClassNamespaceAssignment = extractAnchors(
      "src/exported-class-namespace-assignment.ts",
      `export class Local { method() {} }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(exportedClassNamespaceAssignment)).toEqual([
      "fn:after",
      "class:Local",
      "fn:Local.method",
      "export:value",
      "export:Local"
    ]);
    expect(exportedClassNamespaceAssignment["export:default"]).toBeUndefined();

    const defaultClassNamespaceAssignment = extractAnchors(
      "src/default-class-namespace-assignment.ts",
      `export default class Local { method() {} }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`
    );

    expect(Object.keys(defaultClassNamespaceAssignment)).toEqual([
      "fn:after",
      "class:Local",
      "fn:Local.method",
      "export:value",
      "export:default"
    ]);
  });

  it("captures dotted namespace and namespace export alias assignment edges", () => {
    const anchors = extractAnchors(
      "src/ns-combined.ts",
      `namespace Local.Inner.Deep {
  export function deep() {}
}
namespace Local {
  const hidden = () => true;
  function secret() { if (a && b || c) return 1; }
  export { hidden, secret as revealed };
  export function run() {}
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "export:Inner",
      "export:run",
      "export:hidden",
      "export:revealed",
      "block:if_a_b_c"
    ]);
    expect([
      anchors["export:Inner"]?.start,
      anchors["export:Inner"]?.end,
      anchors["export:run"]?.start,
      anchors["export:hidden"]?.start,
      anchors["export:revealed"]?.start,
      anchors["export:revealed"]?.complexity
    ]).toEqual([1, 3, 8, 5, 6, 4]);
    expect([
      anchors["block:if_a_b_c"]?.start,
      anchors["block:if_a_b_c"]?.end,
      anchors["block:if_a_b_c"]?.complexity
    ]).toEqual([6, 6, 3]);
    expect(anchors["export:deep"]).toBeUndefined();
    expect(anchors["fn:secret"]).toBeUndefined();
  });

  it("captures merged namespace export alias edges", () => {
    const backward = extractAnchors(
      "src/ns-merge-exported-backward.ts",
      `namespace N {
  export class Box { open() {} }
}
namespace N {
  export { Box as Crate };
}
export = N;
function after() {}
`
    );

    expect(Object.keys(backward)).toEqual(["fn:after", "export:Box", "export:Crate"]);
    expect([
      backward["fn:after"]?.start,
      backward["export:Box"]?.start,
      backward["export:Crate"]?.start
    ]).toEqual([8, 2, 2]);

    const forward = extractAnchors(
      "src/ns-merge-exported-forward.ts",
      `namespace N {
  export { Box as Crate };
}
namespace N {
  export class Box { open() {} }
}
export = N;
function after() {}
`
    );

    expect(Object.keys(forward)).toEqual(["fn:after", "export:Crate", "export:Box"]);
    expect([
      forward["fn:after"]?.start,
      forward["export:Crate"]?.start,
      forward["export:Box"]?.start
    ]).toEqual([8, 5, 5]);

    const mixed = extractAnchors(
      "src/ns-merge-mixed.ts",
      `namespace N {
  class Hidden { method() {} }
  export class Box { open() {} }
  export const value = () => true;
}
namespace N {
  export { Hidden as Seen, Box as Crate, value as aliasValue };
}
export = N;
function after() {}
`
    );

    expect(Object.keys(mixed)).toEqual(["fn:after", "export:Box", "export:value", "export:Crate", "export:aliasValue"]);
    expect([
      mixed["fn:after"]?.start,
      mixed["export:Box"]?.start,
      mixed["export:value"]?.start,
      mixed["export:Crate"]?.start,
      mixed["export:aliasValue"]?.start
    ]).toEqual([10, 3, 4, 3, 4]);
    expect(mixed["export:Seen"]).toBeUndefined();
    expect(mixed["export:Hidden"]).toBeUndefined();
  });

  it("captures namespace alias-to-alias export edges", () => {
    const sameBlock = extractAnchors(
      "src/ns-alias-chain.ts",
      `namespace N {
  const value = 1;
  export { value as first };
  export { first as second };
}
export = N;
function after() {}
`
    );

    expect(Object.keys(sameBlock)).toEqual(["fn:after", "export:first", "export:second"]);
    expect([
      sameBlock["fn:after"]?.start,
      sameBlock["export:first"]?.start,
      sameBlock["export:second"]?.start
    ]).toEqual([7, 2, 2]);

    const forward = extractAnchors(
      "src/ns-merge-alias-forward.ts",
      `namespace N {
  export { first as second };
}
namespace N {
  export { value as first };
}
namespace N {
  export const value = 1;
}
export = N;
function after() {}
`
    );

    expect(Object.keys(forward)).toEqual(["fn:after", "export:second", "export:first", "export:value"]);
    expect([
      forward["fn:after"]?.start,
      forward["export:second"]?.start,
      forward["export:first"]?.start,
      forward["export:value"]?.start
    ]).toEqual([11, 8, 8, 8]);

    const directAndChain = extractAnchors(
      "src/ns-alias-direct-and-chain.ts",
      `namespace N {
  export const value = 1;
  export { value as first };
  export { value as second, first as third };
}
export = N;
function after() {}
`
    );

    expect(Object.keys(directAndChain)).toEqual([
      "fn:after",
      "export:value",
      "export:first",
      "export:second",
      "export:third"
    ]);
    expect([
      directAndChain["fn:after"]?.start,
      directAndChain["export:value"]?.start,
      directAndChain["export:first"]?.start,
      directAndChain["export:second"]?.start,
      directAndChain["export:third"]?.start
    ]).toEqual([7, 2, 2, 2, 2]);

    const typeOnlyForward = extractAnchors(
      "src/ns-type-alias-forward.ts",
      `namespace N {
  export type { value as PublicValue };
}
namespace N {
  export const value = 1;
}
export = N;
function after() {}
`
    );

    expect(Object.keys(typeOnlyForward)).toEqual(["fn:after", "export:PublicValue", "export:value"]);
    expect([
      typeOnlyForward["fn:after"]?.start,
      typeOnlyForward["export:PublicValue"]?.start,
      typeOnlyForward["export:value"]?.start
    ]).toEqual([8, 5, 5]);
    expect(typeOnlyForward["export:type"]).toBeUndefined();
  });

  it("captures else-if block chain anchor edges", () => {
    const anchors = extractAnchors(
      "src/else-if-blocks.ts",
      `function run() {
  if (a && b && c) { return 1; }
  else if (d && e && f) { return 2; }
}
`
    );

    expect(Object.keys(anchors)).toEqual(["fn:run", "block:if_a_b_c", "block:if_d_e_f"]);
    expect([anchors["fn:run"]?.start, anchors["fn:run"]?.end, anchors["fn:run"]?.complexity]).toEqual([
      1,
      4,
      7
    ]);
    expect([
      anchors["block:if_a_b_c"]?.start,
      anchors["block:if_a_b_c"]?.end,
      anchors["block:if_a_b_c"]?.complexity
    ]).toEqual([2, 3, 6]);
    expect([
      anchors["block:if_d_e_f"]?.start,
      anchors["block:if_d_e_f"]?.end,
      anchors["block:if_d_e_f"]?.complexity
    ]).toEqual([3, 3, 3]);
  });

  it("captures ambient namespace export assignment edges", () => {
    const implicit = extractAnchors(
      "src/ns-ambient.ts",
      `declare namespace Ambient {
  const value: number;
  function run(): void;
  class Box { open(): void; }
  interface Face { y: number }
  type Shape = { x: number };
}
export = Ambient;
function real() {}
`
    );

    expect(Object.keys(implicit)).toEqual([
      "fn:real",
      "export:run",
      "export:value",
      "export:Box",
      "export:Face",
      "export:Shape"
    ]);
    expect([
      implicit["fn:real"]?.start,
      implicit["export:run"]?.start,
      implicit["export:value"]?.start,
      implicit["export:Box"]?.start,
      implicit["export:Face"]?.start,
      implicit["export:Shape"]?.start
    ]).toEqual([9, 3, 2, 4, 5, 6]);

    const listed = extractAnchors(
      "src/ns-ambient-listed.ts",
      `declare namespace Listed {
  function hiddenRun(): void;
  const hidden: number;
  export { hidden as Hidden };
  export function explicit(): void;
}
export = Listed;
function real() {}
`
    );

    expect(Object.keys(listed)).toEqual(["fn:real", "export:explicit", "export:Hidden"]);
    expect([
      listed["fn:real"]?.start,
      listed["export:explicit"]?.start,
      listed["export:Hidden"]?.start
    ]).toEqual([8, 5, 3]);
    expect(listed["export:hiddenRun"]).toBeUndefined();
    expect(listed["export:hidden"]).toBeUndefined();

    const nested = extractAnchors(
      "src/ns-ambient-nested.ts",
      `declare namespace Local {
  namespace Inner { export function deep(): void; }
  namespace Plain { function hidden(): void; }
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(nested)).toEqual(["fn:real", "export:Inner", "export:Plain"]);
    expect([
      nested["fn:real"]?.start,
      nested["export:Inner"]?.start,
      nested["export:Plain"]?.start
    ]).toEqual([6, 2, 3]);
    expect(nested["export:deep"]).toBeUndefined();
    expect(nested["export:hidden"]).toBeUndefined();

    const nestedListed = extractAnchors(
      "src/ns-ambient-nested-listed.ts",
      `declare namespace Local {
  namespace Inner {
    const hidden: number;
    export { hidden as Hidden };
  }
  function run(): void;
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(nestedListed)).toEqual(["fn:real", "export:run", "export:Inner"]);
    expect([
      nestedListed["fn:real"]?.start,
      nestedListed["export:run"]?.start,
      nestedListed["export:Inner"]?.start,
      nestedListed["export:Inner"]?.end
    ]).toEqual([9, 6, 2, 5]);
    expect(nestedListed["export:Hidden"]).toBeUndefined();

    const ambientModule = extractAnchors(
      "src/ns-ambient-module.ts",
      `declare module "pkg" {
  export function ghost(): void;
  export const ghostValue: number;
}
declare module Local {
  export function run(): void;
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(ambientModule)).toEqual(["fn:real", "export:run"]);
    expect([ambientModule["fn:real"]?.start, ambientModule["export:run"]?.start]).toEqual([9, 6]);
    expect(ambientModule["export:ghost"]).toBeUndefined();
    expect(ambientModule["export:ghostValue"]).toBeUndefined();

    const mergeBackward = extractAnchors(
      "src/ns-ambient-merge-backward.ts",
      `declare namespace Local {
  const hidden: number;
}
declare namespace Local {
  export { hidden as Hidden };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(mergeBackward)).toEqual(["fn:real", "export:hidden", "export:Hidden"]);
    expect([
      mergeBackward["fn:real"]?.start,
      mergeBackward["export:hidden"]?.start,
      mergeBackward["export:Hidden"]?.start
    ]).toEqual([8, 2, 2]);

    const mergeForward = extractAnchors(
      "src/ns-ambient-merge-forward.ts",
      `declare namespace Local {
  export { hidden as Hidden };
}
declare namespace Local {
  const hidden: number;
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(mergeForward)).toEqual(["fn:real", "export:Hidden", "export:hidden"]);
    expect([
      mergeForward["fn:real"]?.start,
      mergeForward["export:Hidden"]?.start,
      mergeForward["export:hidden"]?.start
    ]).toEqual([8, 5, 5]);

    const typeOnlyMergeForward = extractAnchors(
      "src/ns-ambient-type-alias-forward.ts",
      `declare namespace Local {
  export type { Shape as PublicShape };
}
declare namespace Local {
  interface Shape { x: number }
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(typeOnlyMergeForward)).toEqual(["fn:real", "export:PublicShape", "export:Shape"]);
    expect([
      typeOnlyMergeForward["fn:real"]?.start,
      typeOnlyMergeForward["export:PublicShape"]?.start,
      typeOnlyMergeForward["export:Shape"]?.start
    ]).toEqual([8, 5, 5]);
    expect(typeOnlyMergeForward["export:type"]).toBeUndefined();
  });

  it("captures namespace multi-variable and type-only export alias edges", () => {
    const anchors = extractAnchors(
      "src/ns-multivar.ts",
      `namespace Local {
  const first = () => true, second = () => false;
  let count = 1, total = 2;
  type Shape = { x: number };
  interface Face { y: number }
  export { first, second as renamedSecond, count, total as renamedTotal, type Shape, Face as RenamedFace };
  export const direct = () => true, other = () => false;
  export { direct as aliasDirect, other as aliasOther };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "export:first",
      "export:renamedSecond",
      "export:count",
      "export:renamedTotal",
      "export:Shape",
      "export:RenamedFace",
      "export:direct",
      "export:other",
      "export:aliasDirect",
      "export:aliasOther"
    ]);
    expect([
      anchors["export:first"]?.start,
      anchors["export:renamedSecond"]?.start,
      anchors["export:count"]?.start,
      anchors["export:renamedTotal"]?.start,
      anchors["export:Shape"]?.start,
      anchors["export:RenamedFace"]?.start,
      anchors["export:direct"]?.start,
      anchors["export:other"]?.start,
      anchors["export:aliasDirect"]?.start,
      anchors["export:aliasOther"]?.start
    ]).toEqual([2, 2, 3, 3, 4, 5, 7, 7, 7, 7]);
    expect(anchors["export:second"]).toBeUndefined();
    expect(anchors["export:total"]).toBeUndefined();
    expect(anchors["export:Face"]).toBeUndefined();
  });

  it("captures namespace destructuring export assignment edges", () => {
    const anchors = extractAnchors(
      "src/ns-destructure.ts",
      `namespace Local {
  const { first, second: renamedLocal } = source;
  const [head, tail] = values;
  export const { direct, alias: directAlias } = source;
  export const [directHead, directTail] = values;
  const { outer: { inner }, ...rest } = source;
  const [nestedHead, ...others] = values;
  export { first, renamedLocal as exportedSecond, head, tail as exportedTail, inner, rest, nestedHead, others };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "export:direct",
      "export:directAlias",
      "export:directHead",
      "export:directTail",
      "export:first",
      "export:exportedSecond",
      "export:head",
      "export:exportedTail",
      "export:inner",
      "export:rest",
      "export:nestedHead",
      "export:others"
    ]);
    expect([
      anchors["export:direct"]?.start,
      anchors["export:directAlias"]?.start,
      anchors["export:directHead"]?.start,
      anchors["export:directTail"]?.start,
      anchors["export:first"]?.start,
      anchors["export:exportedSecond"]?.start,
      anchors["export:head"]?.start,
      anchors["export:exportedTail"]?.start,
      anchors["export:inner"]?.start,
      anchors["export:rest"]?.start,
      anchors["export:nestedHead"]?.start,
      anchors["export:others"]?.start
    ]).toEqual([4, 4, 5, 5, 2, 2, 3, 3, 6, 6, 7, 7]);
    expect(anchors["export:renamedLocal"]).toBeUndefined();
    expect(anchors["export:tail"]).toBeUndefined();
    expect(anchors["export:alias"]).toBeUndefined();
  });

  it("captures namespace binding default initializer edges", () => {
    const anchors = extractAnchors(
      "src/ns-binding-defaults.ts",
      `namespace Local {
  const { [dynamicKey]: computedValue = fallback(a, b), plain = make(c, d), nested: { inner = other(e, f) } = defaults } = source;
  const [head = pair(g, h), , tail = makeTail(i, j)] = values;
  export const { directComputed = call(k, l), directPlain = make(m, n) } = source;
  export const [directHead = pair(o, p), directTail = makeTail(q, r)] = values;
  export { computedValue, plain, inner, head, tail };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "export:directComputed",
      "export:directPlain",
      "export:directHead",
      "export:directTail",
      "export:computedValue",
      "export:plain",
      "export:inner",
      "export:head",
      "export:tail"
    ]);
    expect([
      anchors["export:directComputed"]?.start,
      anchors["export:directPlain"]?.start,
      anchors["export:directHead"]?.start,
      anchors["export:directTail"]?.start,
      anchors["export:computedValue"]?.start,
      anchors["export:plain"]?.start,
      anchors["export:inner"]?.start,
      anchors["export:head"]?.start,
      anchors["export:tail"]?.start
    ]).toEqual([4, 4, 5, 5, 2, 2, 2, 3, 3]);
    for (const ghost of ["export:b", "export:d", "export:f", "export:h", "export:j", "export:l", "export:n", "export:p", "export:r"]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("captures quoted export alias suppression edges", () => {
    const namespace = extractAnchors(
      "src/ns-quoted-alias.ts",
      `namespace Local {
  const value = 1;
  const keep = 2;
  function run() {}
  type Shape = { x: number };
  export { keep as kept, value as "dash-name", run as "call-run", type Shape as "shape-type" };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(namespace)).toEqual(["fn:real"]);
    for (const ghost of [
      "export:value",
      "export:keep",
      "export:kept",
      "export:run",
      "export:Shape",
      "export:dash-name",
      "export:call-run",
      "export:shape-type"
    ]) {
      expect(namespace[ghost]).toBeUndefined();
    }

    const topLevel = extractAnchors(
      "src/top-quoted-alias.ts",
      `const value = 1;
const keep = 2;
function run() {}
export { keep as kept, value as "dash-name" };
`
    );

    expect(Object.keys(topLevel)).toEqual(["fn:run"]);
    expect(topLevel["export:kept"]).toBeUndefined();
    expect(topLevel["export:value"]).toBeUndefined();
  });

  it("captures missing export alias target edges", () => {
    const topLevel = extractAnchors(
      "src/top-missing-alias.ts",
      `const keep = 1;
const other = 2;
export { keep as, other as valid };
function real() {}
`
    );

    expect(Object.keys(topLevel)).toEqual(["fn:real", "export:", "export:valid"]);
    expect([topLevel["fn:real"]?.start, topLevel["export:"]?.start, topLevel["export:valid"]?.start]).toEqual([
      4,
      1,
      2
    ]);
    expect(topLevel["export:keep"]).toBeUndefined();
    expect(topLevel["export:other"]).toBeUndefined();

    const namespace = extractAnchors(
      "src/ns-missing-alias.ts",
      `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as, other as valid };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(namespace)).toEqual(["fn:real", "export:", "export:valid"]);
    expect([namespace["fn:real"]?.start, namespace["export:"]?.start, namespace["export:valid"]?.start]).toEqual([
      7,
      2,
      3
    ]);
    expect(namespace["export:keep"]).toBeUndefined();
    expect(namespace["export:other"]).toBeUndefined();

    const commaHole = extractAnchors(
      "src/ns-alias-comma-hole.ts",
      `namespace Local {
  const keep = 1;
  const other = 2;
  export { , keep, other as valid };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(commaHole)).toEqual(["fn:real", "export:keep", "export:valid"]);
    expect(commaHole["export:"]).toBeUndefined();

    const numericTarget = extractAnchors(
      "src/ns-numeric-alias.ts",
      `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as 123, other as valid };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(numericTarget)).toEqual(["fn:real", "export:"]);
    expect([numericTarget["fn:real"]?.start, numericTarget["export:"]?.start]).toEqual([7, 2]);
    expect(numericTarget["export:valid"]).toBeUndefined();

    const numericAfterValid = extractAnchors(
      "src/top-numeric-alias-after-valid.ts",
      `const keep = 1;
const other = 2;
export { other as valid, keep as 123 };
function real() {}
`
    );

    expect(Object.keys(numericAfterValid)).toEqual(["fn:real", "export:valid", "export:"]);

    const templateTarget = extractAnchors(
      "src/top-template-alias.ts",
      `const keep = 1;
const other = 2;
export { keep as \`dash\`, other as valid };
function real() {}
`
    );

    expect(Object.keys(templateTarget)).toEqual(["fn:real", "export:"]);
    expect(templateTarget["export:valid"]).toBeUndefined();

    const dotTarget = extractAnchors(
      "src/ns-dot-alias.ts",
      `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as ., other as valid };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(dotTarget)).toEqual(["fn:real", "export:", "export:valid"]);

    const punctuationTargets = extractAnchors(
      "src/top-punctuation-alias.ts",
      `const q = 1;
const c = 2;
const p = 3;
const b = 4;
const other = 5;
export { q as ?, c as :, p as ), b as ], other as valid };
function real() {}
`
    );

    expect(Object.keys(punctuationTargets)).toEqual(["fn:real", "export:", "export:valid"]);
    expect([punctuationTargets["export:"]?.start, punctuationTargets["export:valid"]?.start]).toEqual([1, 5]);

    const namespacePunctuationTargets = extractAnchors(
      "src/ns-punctuation-alias.ts",
      `namespace Local {
  const q = 1;
  const c = 2;
  const p = 3;
  const b = 4;
  const other = 5;
  export { q as ?, c as :, p as ), b as ], other as valid };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(namespacePunctuationTargets)).toEqual(["fn:real", "export:", "export:valid"]);
    expect([
      namespacePunctuationTargets["export:"]?.start,
      namespacePunctuationTargets["export:valid"]?.start
    ]).toEqual([2, 6]);

    const privateNameTarget = extractAnchors(
      "src/top-private-name-alias.ts",
      `const keep = 1;
const other = 2;
export { keep as #foo, other as Valid };
function real() {}
`
    );

    expect(Object.keys(privateNameTarget)).toEqual(["fn:real", "export:#foo", "export:Valid"]);
    expect([privateNameTarget["export:#foo"]?.start, privateNameTarget["export:Valid"]?.start]).toEqual([1, 2]);

    const namespacePrivateNameTarget = extractAnchors(
      "src/ns-private-name-alias.ts",
      `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as #foo, other as Valid };
}
export = Local;
function real() {}
`
    );

    expect(Object.keys(namespacePrivateNameTarget)).toEqual(["fn:real", "export:#foo", "export:Valid"]);
    expect([
      namespacePrivateNameTarget["export:#foo"]?.start,
      namespacePrivateNameTarget["export:Valid"]?.start
    ]).toEqual([2, 3]);
  });

  it("captures literal method names and type-only export aliases", () => {
    const anchors = extractAnchors(
      "src/literal-methods.ts",
      `interface Foo {}
type Bar = string;
export type { Foo as RenamedFoo, Bar };
export { type Foo as TypeFoo };
class Names {
  "quoted"() { return 1; }
  42() { return 2; }
  static "static-name"() { return 3; }
  get "value"() { return 4; }
  [computed]() { return 5; }
}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "class:Names",
      'fn:Names."quoted"',
      "fn:Names.42",
      'fn:Names."static-name"',
      "fn:Names.[computed]",
      "export:RenamedFoo",
      "export:Bar",
      "export:TypeFoo"
    ]);
    expect([anchors['fn:Names."quoted"']?.start, anchors["fn:Names.42"]?.start]).toEqual([6, 7]);
    expect(anchors['fn:Names."static-name"']?.start).toBe(8);
    expect(anchors['fn:Names."value"']).toBeUndefined();
    expect([anchors["export:RenamedFoo"]?.start, anchors["export:Bar"]?.start, anchors["export:TypeFoo"]?.start]).toEqual([
      1,
      2,
      1
    ]);
  });

  it("captures escaped, numeric, and computed method name edges", () => {
    const anchors = extractAnchors(
      "src/literal-edge-methods.ts",
      `class Numeric {
  "with\\\\slash"() {}
  "with\\"quote"() {}
  3.14() {}
  1_000() {}
  0x10() {}
  1e3() {}
}
class Computed {
  ["literal"]() {}
  [1 + 2]() {}
  [Symbol.iterator]() {}
  [bad ? { x: 1 } : key]() {}
}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "class:Numeric",
      'fn:Numeric."with\\\\slash"',
      'fn:Numeric."with\\"quote"',
      "fn:Numeric.3.14",
      "fn:Numeric.1_000",
      "fn:Numeric.0x10",
      "fn:Numeric.1e3",
      "class:Computed",
      'fn:Computed.["literal"]',
      "fn:Computed.[1 + 2]",
      "fn:Computed.[Symbol.iterator]",
      "fn:Computed.[bad ? { x: 1 } : key]"
    ]);
    expect([anchors["fn:Numeric.3.14"]?.start, anchors["fn:Numeric.1_000"]?.start]).toEqual([4, 5]);
    expect([anchors["fn:Numeric.0x10"]?.start, anchors["fn:Numeric.1e3"]?.start]).toEqual([6, 7]);
    expect(anchors["fn:Computed.[1 + 2]"]?.start).toBe(11);
    expect([
      anchors["fn:Computed.[bad ? { x: 1 } : key]"]?.start,
      anchors["fn:Computed.[bad ? { x: 1 } : key]"]?.complexity
    ]).toEqual([13, 2]);
  });

  it("captures malformed BigInt-like class element recovery", () => {
    const anchors = extractAnchors(
      "src/bigint-recovery.ts",
      `class Broken {
  before() {}
  10n() {}
  after() {}
}
function later() { return 1; }
`
    );

    expect(Object.keys(anchors)).toEqual(["fn:later", "class:Broken", "fn:Broken.before"]);
    expect([anchors["class:Broken"]?.start, anchors["class:Broken"]?.end]).toEqual([1, 2]);
    expect([anchors["fn:Broken.before"]?.start, anchors["fn:Broken.before"]?.end]).toEqual([2, 2]);
    expect(anchors["fn:Broken.n"]).toBeUndefined();
    expect(anchors["fn:Broken.after"]).toBeUndefined();

    const computed = extractAnchors(
      "src/computed-recovery.ts",
      `class Broken {
  before() {}
  [bad() {}
  after() {}
}
function later() {}
`
    );

    expect(Object.keys(computed)).toEqual(["fn:later", "class:Broken", "fn:Broken.before"]);
    expect([
      computed["class:Broken"]?.start,
      computed["class:Broken"]?.end,
      computed["fn:Broken.before"]?.start
    ]).toEqual([1, 3, 2]);
    expect(computed["fn:Broken.bad"]).toBeUndefined();
    expect(computed["fn:Broken.after"]).toBeUndefined();

    const methodParams = extractAnchors(
      "src/method-params-recovery.ts",
      `class Broken {
  before() {}
  broken(a, b {
    return 1;
  }
  after() {}
}
function later() {}
`
    );

    expect(Object.keys(methodParams)).toEqual([
      "fn:later",
      "class:Broken",
      "fn:Broken.before",
      "fn:Broken.broken"
    ]);
    expect([
      methodParams["class:Broken"]?.start,
      methodParams["class:Broken"]?.end,
      methodParams["fn:Broken.broken"]?.start,
      methodParams["fn:Broken.broken"]?.end
    ]).toEqual([1, 6, 3, 6]);
    expect(methodParams["fn:Broken.after"]).toBeUndefined();
  });

  it("captures unclosed function body recovery", () => {
    const unclosedFunction = extractAnchors(
      "src/unclosed-function.ts",
      `function broken() {
  if (a && b) return 1;
function later() { return 2; }
`
    );

    expect(Object.keys(unclosedFunction)).toEqual(["fn:broken"]);
    expect([
      unclosedFunction["fn:broken"]?.start,
      unclosedFunction["fn:broken"]?.end,
      unclosedFunction["fn:broken"]?.complexity
    ]).toEqual([1, 3, 3]);
    expect(unclosedFunction["fn:later"]).toBeUndefined();

    const unclosedArrow = extractAnchors(
      "src/unclosed-arrow.ts",
      `const broken = () => {
  if (a && b) return 1;
function later() { return 2; }
`
    );

    expect(Object.keys(unclosedArrow)).toEqual(["fn:broken"]);
    expect([
      unclosedArrow["fn:broken"]?.start,
      unclosedArrow["fn:broken"]?.end,
      unclosedArrow["fn:broken"]?.complexity
    ]).toEqual([1, 3, 3]);
    expect(unclosedArrow["fn:later"]).toBeUndefined();
  });

  it("suppresses nested arrow initializer parser ghosts", () => {
    for (const source of [
      "const x = foo(() => true);\nfunction after() {}\n",
      "const x = (() => true)();\nfunction after() {}\n",
      "const x = { run: () => true };\nfunction after() {}\n"
    ]) {
      const anchors = extractAnchors("src/nested-arrow.ts", source);
      expect(Object.keys(anchors)).toEqual(["fn:after"]);
      expect(anchors["fn:x"]).toBeUndefined();
    }
  });

  it("captures function expression variable initializer edges", () => {
    const anchors = extractAnchors(
      "src/function-expressions.ts",
      `const plain = function() { return 1; };
const named = function hidden() { return 2; };
const asyncNamed = async function hiddenAsync() { await run(); };
export const exported = async function exportedHidden() { await run(); };
const paren = (function parenHidden() { return 3; });
const call = foo(function callHidden() { return 4; });
function real() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:real",
      "fn:plain",
      "fn:named",
      "fn:asyncNamed",
      "fn:exported",
      "export:exported"
    ]);
    expect([
      anchors["fn:plain"]?.start,
      anchors["fn:named"]?.start,
      anchors["fn:asyncNamed"]?.start,
      anchors["fn:exported"]?.start,
      anchors["export:exported"]?.start
    ]).toEqual([1, 2, 3, 4, 4]);
    for (const ghost of [
      "fn:hidden",
      "fn:hiddenAsync",
      "fn:exportedHidden",
      "fn:paren",
      "fn:parenHidden",
      "fn:call",
      "fn:callHidden"
    ]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("suppresses type-operator wrapped arrow initializer anchors", () => {
    const anchors = extractAnchors(
      "src/type-operator-wrapped-arrows.ts",
      `const satisfiesRun = ((x: number) => x) satisfies (x: number) => number;
const asRun = ((x: number) => x) as (x: number) => number;
export const exported = ((x: number) => x) satisfies (x: number) => number;
const direct = (x: number) => x satisfies number;
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual(["fn:after", "fn:direct", "export:exported"]);
    expect([anchors["fn:direct"]?.start, anchors["export:exported"]?.start, anchors["fn:after"]?.start]).toEqual([
      4,
      3,
      5
    ]);
    for (const ghost of ["fn:satisfiesRun", "fn:asRun", "fn:exported"]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("captures using declaration function initializer edges", () => {
    const anchors = extractAnchors(
      "src/using-declarations.ts",
      `using disposable = () => true;
await using asyncDisposable = async () => true;
export using exportedDisposable = function hidden() { return true; };
export await using exportedAsyncDisposable = async function hiddenAsync() { return true; };
using resource = acquire();
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:after",
      "fn:disposable",
      "fn:asyncDisposable",
      "fn:exportedDisposable",
      "fn:exportedAsyncDisposable",
      "export:exportedDisposable",
      "export:exportedAsyncDisposable"
    ]);
    expect([
      anchors["fn:disposable"]?.start,
      anchors["fn:asyncDisposable"]?.start,
      anchors["fn:exportedDisposable"]?.start,
      anchors["fn:exportedAsyncDisposable"]?.start,
      anchors["export:exportedAsyncDisposable"]?.start,
      anchors["fn:after"]?.start
    ]).toEqual([1, 2, 3, 4, 4, 6]);
    for (const ghost of ["fn:hidden", "fn:hiddenAsync", "fn:resource", "export:resource"]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("suppresses destructuring function initializer parser ghosts", () => {
    const exported = extractAnchors(
      "src/destructured-functions.ts",
      `export const {
  a = function hidden() {
    if (x) return 1;
    return 0;
  },
  b = () => {
    if (y && z) return 2;
    return 0;
  },
  c,
  source: alias,
  nested: { deep = () => true },
  ...rest
} = obj;
export const [first = function hiddenArray() {}, second = () => true, third] = arr;
function real() {}
`
    );

    expect(Object.keys(exported)).toEqual([
      "fn:real",
      "export:a",
      "export:b",
      "export:c",
      "export:alias",
      "export:deep",
      "export:rest",
      "export:first",
      "export:second",
      "export:third"
    ]);
    expect([
      exported["export:a"]?.start,
      exported["export:a"]?.end,
      exported["export:a"]?.complexity,
      exported["export:b"]?.start,
      exported["export:b"]?.end,
      exported["export:b"]?.complexity
    ]).toEqual([2, 5, 2, 6, 9, 3]);
    for (const ghost of [
      "fn:a",
      "fn:b",
      "fn:first",
      "fn:second",
      "fn:hidden",
      "fn:hiddenArray",
      "fn:deep",
      "fn:nested"
    ]) {
      expect(exported[ghost]).toBeUndefined();
    }

    const local = extractAnchors(
      "src/local-destructured-functions.ts",
      "const { a = function hidden() {}, b = () => true } = obj;\nconst [c = function hiddenArray() {}, d = () => true] = arr;\nfunction real() {}\n"
    );
    expect(Object.keys(local)).toEqual(["fn:real"]);

    const aliases = extractAnchors(
      "src/destructured-aliases.ts",
      "const { a, b = () => true } = obj;\nconst [c, d = function hidden() {}] = arr;\nexport { a, b as bee, c, d as dee };\n"
    );
    expect(Object.keys(aliases)).toEqual(["export:a", "export:bee", "export:c", "export:dee"]);
  });

  it("captures Unicode identifier anchor edges", () => {
    const anchors = extractAnchors(
      "src/unicode-identifiers.ts",
      `export function café() { return 1; }
const π = () => true;
export const 名前 = () => true;
class 店 { 開く() { return true; } }
export { π as piAlias };
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:café",
      "fn:after",
      "class:店",
      "fn:店.開く",
      "fn:π",
      "fn:名前",
      "export:café",
      "export:名前",
      "export:piAlias"
    ]);
    expect([
      anchors["fn:café"]?.start,
      anchors["fn:π"]?.start,
      anchors["fn:名前"]?.start,
      anchors["class:店"]?.start,
      anchors["fn:店.開く"]?.start,
      anchors["export:piAlias"]?.start
    ]).toEqual([1, 2, 3, 4, 4, 2]);
  });

  it("captures escaped Unicode identifier anchor edges", () => {
    const anchors = extractAnchors(
      "src/unicode-escapes.ts",
      String.raw`export function caf\u00e9() { return 1; }
const \u03c0 = () => true;
const \u{03c0}Brace = () => true;
export const \u540d\u524d = () => true;
class \u5e97 { \u958b\u304f() { return true; } }
export { \u03c0 as piAlias };
function after() {}
`
    );

    expect(Object.keys(anchors)).toEqual([
      "fn:café",
      "fn:after",
      "class:店",
      "fn:店.開く",
      "fn:π",
      "fn:πBrace",
      "fn:名前",
      "export:café",
      "export:名前",
      "export:piAlias"
    ]);
    expect([
      anchors["fn:café"]?.start,
      anchors["fn:π"]?.start,
      anchors["fn:πBrace"]?.start,
      anchors["fn:名前"]?.start,
      anchors["class:店"]?.start,
      anchors["fn:店.開く"]?.start,
      anchors["export:piAlias"]?.start
    ]).toEqual([1, 2, 3, 4, 5, 5, 2]);
    for (const ghost of ["fn:caf", "export:caf", "fn:u03c0", "fn:u540d", "class:u5e97", "fn:u958b"]) {
      expect(anchors[ghost]).toBeUndefined();
    }
  });

  it("captures default generator function edges", () => {
    const anonymous = extractAnchors(
      "src/default-generator.ts",
      "export default function*() { yield 1; }\nfunction real() {}\n"
    );

    expect(Object.keys(anonymous)).toEqual(["fn:real", "export:default"]);
    expect([anonymous["export:default"]?.start, anonymous["fn:real"]?.start]).toEqual([1, 2]);
    expect(anonymous["fn:yield"]).toBeUndefined();

    const named = extractAnchors(
      "src/named-default-generator.ts",
      "export default function* named() { yield 1; }\nfunction real() {}\n"
    );

    expect(Object.keys(named)).toEqual(["fn:named", "fn:real", "export:default"]);
    expect([named["fn:named"]?.start, named["fn:real"]?.start, named["export:default"]?.start]).toEqual([
      1,
      2,
      1
    ]);
  });

  it("captures malformed JSX recovery boundaries", () => {
    const unclosed = extractAnchors(
      "src/malformed-jsx.tsx",
      `function before() { return 0; }
const Broken = () => <div>
function after() { return 1; }
`
    );
    const mismatchedClose = extractAnchors(
      "src/malformed-jsx-close.tsx",
      `function before() { return 0; }
const Broken = () => <div>
</span>;
function after() { return 1; }
`
    );

    expect(Object.keys(unclosed)).toEqual(["fn:before", "fn:Broken"]);
    expect([unclosed["fn:Broken"]?.start, unclosed["fn:Broken"]?.end]).toEqual([2, 4]);
    expect(unclosed["fn:after"]).toBeUndefined();
    expect(Object.keys(mismatchedClose)).toEqual(["fn:before", "fn:after", "fn:Broken"]);
    expect([mismatchedClose["fn:Broken"]?.start, mismatchedClose["fn:Broken"]?.end]).toEqual([2, 3]);
    expect(mismatchedClose["fn:after"]?.start).toBe(4);
  });

  it("captures significant if blocks inside template literal expressions", () => {
    const anchors = extractAnchors(
      "src/template-block.ts",
      `const tpl = \`line one
\${(() => { if (a && b || c) { return 1; } return 0; })()}
line three\`;
function real() { return 1; }
const after = () => true;
`
    );

    expect(Object.keys(anchors)).toEqual(["fn:real", "fn:after", "block:if_a_b_c"]);
    expect([anchors["fn:real"]?.start, anchors["fn:after"]?.start]).toEqual([4, 5]);
    expect([anchors["block:if_a_b_c"]?.start, anchors["block:if_a_b_c"]?.end, anchors["block:if_a_b_c"]?.complexity]).toEqual([
      2,
      2,
      3
    ]);
    expect(anchors["fn:tpl"]).toBeUndefined();

    const nestedTagged = extractAnchors(
      "src/nested-template-block.ts",
      "const tpl = tag`outer ${(() => tag2`inner ${(() => { if (a && b || c) { return 1; } return 0; })()}`)()} done`;\nfunction real() { return 1; }\n"
    );

    expect(Object.keys(nestedTagged)).toEqual(["fn:real", "block:if_a_b_c"]);
    expect([
      nestedTagged["block:if_a_b_c"]?.start,
      nestedTagged["block:if_a_b_c"]?.end,
      nestedTagged["block:if_a_b_c"]?.complexity
    ]).toEqual([1, 1, 3]);
    expect(nestedTagged["fn:tpl"]).toBeUndefined();

    const regexInExpression = extractAnchors(
      "src/template-regex-block.ts",
      "const tpl = `x ${/}/.test(s) ? (() => { if (a && b || c) { return 1; } return 0; })() : 0}`;\nfunction after() {}\n"
    );

    expect(Object.keys(regexInExpression)).toEqual(["fn:after", "block:if_a_b_c"]);
    expect([
      regexInExpression["fn:after"]?.start,
      regexInExpression["block:if_a_b_c"]?.start,
      regexInExpression["block:if_a_b_c"]?.end,
      regexInExpression["block:if_a_b_c"]?.complexity
    ]).toEqual([2, 1, 1, 3]);

    const divisionLikeExpression = extractAnchors(
      "src/template-division-block.ts",
      "const tpl = `x ${a / } / b ? (() => { if (a && b || c) { return 1; } return 0; })() : 0}`;\nfunction after() {}\n"
    );

    expect(Object.keys(divisionLikeExpression)).toEqual(["fn:after"]);
    expect(divisionLikeExpression["block:if_a_b_c"]).toBeUndefined();

    const commentInExpression = extractAnchors(
      "src/template-comment-block.ts",
      "const tpl = `x ${/* comment } */ (() => { if (a && b || c) { return 1; } return 0; })()}`;\nfunction after() {}\n"
    );

    expect(Object.keys(commentInExpression)).toEqual(["fn:after", "block:if_a_b_c"]);

    const multilineNested = extractAnchors(
      "src/template-multiline-nested.ts",
      "const tpl = `outer ${tag`inner ${(() => {\n  if (a && b || c) { return 1; }\n  return 0;\n})()}`}`;\nfunction after() {}\n"
    );

    expect(Object.keys(multilineNested)).toEqual(["fn:after", "block:if_a_b_c"]);
    expect([
      multilineNested["fn:after"]?.start,
      multilineNested["block:if_a_b_c"]?.start,
      multilineNested["block:if_a_b_c"]?.end,
      multilineNested["block:if_a_b_c"]?.complexity
    ]).toEqual([5, 2, 2, 3]);

    const nestedUnclosedInner = extractAnchors(
      "src/template-nested-unclosed-inner.ts",
      "const tpl = `outer ${tag`inner ${(() => { if (a && b || c) { return 1; } return 0; })()} done`;\nfunction after() {}\n"
    );

    expect(Object.keys(nestedUnclosedInner)).toEqual(["fn:after", "block:if_a_b_c"]);
    expect([
      nestedUnclosedInner["fn:after"]?.start,
      nestedUnclosedInner["block:if_a_b_c"]?.start,
      nestedUnclosedInner["block:if_a_b_c"]?.end,
      nestedUnclosedInner["block:if_a_b_c"]?.complexity
    ]).toEqual([2, 1, 1, 3]);

    const nestedOpenExpression = extractAnchors(
      "src/template-nested-open-expression.ts",
      "const tpl = `outer ${tag`inner ${(() => { if (a && b || c) { return 1; } return 0; })()}\nfunction after() {}\n"
    );

    expect(Object.keys(nestedOpenExpression)).toEqual(["block:if_a_b_c"]);
    expect([
      nestedOpenExpression["block:if_a_b_c"]?.start,
      nestedOpenExpression["block:if_a_b_c"]?.end,
      nestedOpenExpression["block:if_a_b_c"]?.complexity
    ]).toEqual([1, 1, 3]);
    expect(nestedOpenExpression["fn:after"]).toBeUndefined();

    const escapedBacktick = extractAnchors(
      "src/template-escaped-backtick.ts",
      "const tpl = `line \\` still template ${(() => { if (a && b || c) { return 1; } return 0; })()}`;\nfunction after() {}\n"
    );

    expect(Object.keys(escapedBacktick)).toEqual(["fn:after", "block:if_a_b_c"]);

    const escapedDollar = extractAnchors(
      "src/template-escaped-dollar.ts",
      "const tpl = `literal \\${notExpression}`;\nfunction after() {}\n"
    );

    expect(Object.keys(escapedDollar)).toEqual(["fn:after"]);

    const unterminatedRaw = extractAnchors(
      "src/template-unterminated-raw.ts",
      "const tpl = `function hidden() {}\nfunction after() {}\n"
    );

    expect(Object.keys(unterminatedRaw)).toEqual([]);

    const unterminatedClosedExpression = extractAnchors(
      "src/template-unterminated-closed-expression.ts",
      "const tpl = `x ${(() => { if (a && b || c) { return 1; } return 0; })()}\nfunction after() {}\n"
    );

    expect(Object.keys(unterminatedClosedExpression)).toEqual(["block:if_a_b_c"]);
    expect(unterminatedClosedExpression["fn:after"]).toBeUndefined();

    const unterminatedOpenExpression = extractAnchors(
      "src/template-unterminated-open-expression.ts",
      "const tpl = `x ${(() => { if (a && b || c) { return 1; } return 0; })()\nfunction after() {}\n"
    );

    expect(Object.keys(unterminatedOpenExpression)).toEqual(["fn:after", "block:if_a_b_c"]);
    expect([
      unterminatedOpenExpression["fn:after"]?.start,
      unterminatedOpenExpression["block:if_a_b_c"]?.start,
      unterminatedOpenExpression["block:if_a_b_c"]?.end
    ]).toEqual([2, 1, 1]);
    expect(unterminatedOpenExpression["fn:tpl"]).toBeUndefined();
  });

  it("captures TSX generic arrow ambiguity and TS generic arrow parity", () => {
    const source = `function before() { return 0; }
const id = <T>(value: T) => value;
function after<T>(value: T) { return value; }
`;
    const tsAnchors = extractAnchors("src/ambig-generic.ts", source);
    const tsxAnchors = extractAnchors("src/ambig-generic.tsx", source);
    const validTsxAnchors = extractAnchors(
      "src/valid-generic.tsx",
      `function before() { return 0; }
const id = <T,>(value: T) => value;
function after<T>(value: T) { return value; }
`
    );

    expect(Object.keys(tsAnchors)).toEqual(["fn:before", "fn:after", "fn:id"]);
    expect(Object.keys(tsxAnchors)).toEqual(["fn:before"]);
    expect(Object.keys(validTsxAnchors)).toEqual(["fn:before", "fn:after", "fn:id"]);
    expect(tsAnchors["fn:id"]?.start).toBe(2);
    expect(tsxAnchors["fn:id"]).toBeUndefined();
    expect(tsxAnchors["fn:after"]).toBeUndefined();
    expect(validTsxAnchors["fn:id"]?.start).toBe(2);
  });

  it("captures that reanchor shifts lines before checking fingerprint", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/shift.ts"), "function kept() {\n  return 1;\n}\n", "utf8");
    execFileSync("git", ["init"], { cwd: root, stdio: "ignore" });
    execFileSync("git", ["add", "src/shift.ts"], { cwd: root, stdio: "ignore" });
    execFileSync(
      "git",
      ["-c", "user.name=Archiva Test", "-c", "user.email=archiva@example.invalid", "commit", "-m", "initial"],
      { cwd: root, stdio: "ignore" }
    );

    await writeDecision(root, {
      file: "src/shift.ts",
      anchor: "fn:kept",
      lines: [1, 3],
      chose: "keep function body",
      because: "fixture",
      rejected: []
    });

    await fs.writeFile(path.join(root, "src/shift.ts"), "// inserted\nfunction kept() {\n  return 1;\n}\n", "utf8");
    await expect(postToolUse(root, "src/shift.ts")).resolves.toBe("Re-anchored src/shift.ts: 0 stale, 0 orphan.");
    const dlog = await loadDlog(root, "src/shift.ts");
    expect(dlog?.decisions["fn:kept"]?.lines_hint).toEqual([2, 4]);
    expect(dlog?.decisions["fn:kept"]?.status).toBeUndefined();
  });

  it("captures status counting before lint side effects mutate stale decisions", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/s.ts"), "function kept() {\n  return 1;\n}\n", "utf8");
    await writeDecision(root, {
      file: "src/s.ts",
      anchor: "fn:kept",
      lines: [1, 3],
      chose: "initial",
      because: "fixture",
      rejected: []
    });

    await fs.writeFile(path.join(root, "src/s.ts"), "function kept() {\n  return 2;\n}\n", "utf8");
    await expect(status(root)).resolves.toBe(`src/s.ts                         1 decisions  0 stale  0 orphan

Total: 1 decisions  0 stale  0 orphan  1 issues`);
    const dlog = await loadDlog(root, "src/s.ts");
    expect(dlog?.decisions["fn:kept"]?.status).toBe("STALE");
    expect(dlog?.decisions["fn:kept"]?.stale_since).toBeTruthy();
  });

  it("captures ARCHIVA_SESSION fallback for write-decision", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/env.ts"), "export function fromEnv() {\n  return 1;\n}\n", "utf8");

    const result = runArchiva(
      ["write-decision"],
      JSON.stringify({
        file: "src/env.ts",
        anchor: "fn:fromEnv",
        lines: [1, 3],
        chose: "use env session",
        because: "fixture",
        rejected: []
      }),
      root,
      { ARCHIVA_SESSION: "env_session_contract" }
    );

    expect(result).toMatchObject({ status: 0, stdout: "Recorded dec_001.\n", stderr: "" });
    const dlog = await loadDlog(root, "src/env.ts");
    expect(dlog?.decisions["fn:fromEnv"]?.session).toBe("env_session_contract");
  });

  it("captures superseding a decision across anchors", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(
      path.join(root, "src/supersede.ts"),
      "export function first() {\n  return 1;\n}\nexport function second() {\n  return 2;\n}\n",
      "utf8"
    );

    const first = await writeDecision(root, {
      file: "src/supersede.ts",
      anchor: "fn:first",
      lines: [1, 3],
      chose: "first anchor",
      because: "initial reason",
      rejected: []
    });
    const second = await writeDecision(root, {
      file: "src/supersede.ts",
      anchor: "fn:second",
      lines: [4, 6],
      chose: "second anchor",
      because: "superseding reason",
      rejected: [{ approach: "keep first", reason: "moved responsibility" }],
      supersedes: first.id
    });

    expect(second.id).toBe("dec_002");
    expect(second.history).toEqual([
      {
        id: "dec_001",
        chose: "first anchor",
        because: "initial reason",
        timestamp: first.timestamp,
        superseded_reason: "superseding reason"
      }
    ]);
    const dlog = await loadDlog(root, "src/supersede.ts");
    expect(Object.keys(dlog?.decisions ?? {})).toEqual(["fn:second"]);
    await expect(history(root, "src/supersede.ts", "fn:second")).resolves.toContain("dec_001");
    await expect(why(root, "src/supersede.ts", "fn:first")).resolves.toBe(
      "No decision found for src/supersede.ts at fn:first."
    );
  });

  it("captures MCP ghost_check text and stale side effect", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/drift.ts"), "export function compute() {\n  return 1;\n}\n", "utf8");
    await writeDecision(root, {
      file: "src/drift.ts",
      anchor: "fn:compute",
      lines: [1, 3],
      chose: "return one",
      because: "fixture",
      rejected: []
    });

    await fs.writeFile(path.join(root, "src/drift.ts"), "export function compute() {\n  return 2;\n}\n", "utf8");
    await expect(handleRequest(root, "tools/call", { name: "ghost_check", arguments: { file: "src/drift.ts" } })).resolves.toEqual({
      content: [
        {
          type: "text",
          text: "arc/stale fn:compute: fn:compute code fingerprint differs from recorded decision"
        }
      ]
    });
    const dlog = await loadDlog(root, "src/drift.ts");
    expect(dlog?.decisions["fn:compute"]?.status).toBe("STALE");
  });

  it("captures lint --fix orphan cleanup behavior", async () => {
    const root = await tempProject();
    await writeDlog(root, {
      file: "src/missing.ts",
      schema: 1,
      decisions: {
        "fn:gone": {
          id: "dec_001",
          lines_hint: [1, 2],
          fingerprint: "deadbeef",
          chose: "missing source",
          because: "fixture",
          rejected: [],
          timestamp: "2026-06-26T20:31:18.340Z",
          history: []
        }
      }
    });

    const result = runArchiva(["lint", "--fix"], "", root);
    expect(result).toMatchObject({
      status: 0,
      stdout: "WARNING arc/orphan src/missing.ts fn:gone: fn:gone no longer exists in src/missing.ts\n",
      stderr: ""
    });
    await expect(loadDlog(root, "src/missing.ts")).resolves.toEqual({
      file: "src/missing.ts",
      schema: 1,
      decisions: {}
    });
    await expect(fs.readFile(path.join(root, ".decisions/src/missing.ts.dmap"), "utf8")).resolves.toBe("");
  });
});

async function tempProject(): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), "archiva-contract-test-"));
}

function runArchiva(args: string[], input: string, cwd: string, env: Record<string, string> = {}) {
  return spawnSync(process.execPath, [archivaBin, ...args], {
    cwd,
    input: input.length > 0 && !input.endsWith("\n") ? `${input}\n` : input,
    encoding: "utf8",
    env: { ...process.env, ...env }
  });
}

function findRepoRoot(start: string): string {
  let dir = start;
  while (true) {
    try {
      if (path.basename(JSON.parse(readFileSync(path.join(dir, "package.json"), "utf8")).name ?? "") === "archiva") {
        return dir;
      }
    } catch {
      // keep walking
    }
    const parent = path.dirname(dir);
    if (parent === dir) throw new Error("Could not find Archiva repository root");
    dir = parent;
  }
}
