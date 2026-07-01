import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

type CommandResult = {
  status: number | null;
  stdout: string;
  stderr: string;
};

const WHY_LINE_IMPROVEMENT_REASON =
  "Rust's MCP `why` tool intentionally accepts a `line` argument for line-based lookup (audit blocker B12: TypeScript silently dropped `line` and returned a confidently-wrong whole-file result), so its tools/list schema adds a `line` property and an updated description; all other tool surface is identical.";

type Runtime = {
  name: string;
  command: string;
  prefixArgs: string[];
};

type ScenarioResult = {
  name: string;
  ok: boolean;
  details?: unknown;
};

const repoRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const rustBinInput = process.env.ARCHIVA_RUST_BIN;
const configuredCommandTimeoutMs = Number.parseInt(process.env.ARCHIVA_DIFFERENTIAL_COMMAND_TIMEOUT_MS ?? "", 10);
const commandTimeoutMs =
  Number.isFinite(configuredCommandTimeoutMs) && configuredCommandTimeoutMs > 0
    ? configuredCommandTimeoutMs
    : 60000;
let activeScenario = "startup";

if (!rustBinInput) {
  console.error(
    JSON.stringify(
      {
        tool: "archiva-differential",
        status: "failed",
        reason: "Set ARCHIVA_RUST_BIN to compare the Rust binary against the TypeScript implementation."
      },
      null,
      2
    )
  );
  process.exit(1);
}

const rustBin = path.isAbsolute(rustBinInput) ? rustBinInput : path.resolve(repoRoot, rustBinInput);

const runtimes: [Runtime, Runtime] = [
  { name: "typescript", command: process.execPath, prefixArgs: [path.join(repoRoot, "bin", "archiva.js")] },
  { name: "rust", command: rustBin, prefixArgs: [] }
];

const results: ScenarioResult[] = [];

results.push(await scenario("version", async (runtime) => {
  return {
    command: run(runtime, ["--version"], "", repoRoot)
  };
}));

results.push(await scenario("init-default", async (runtime) => {
  const root = await tempProject(runtime.name, "init-default");
  const command = run(runtime, ["init"], "", root);
  return {
    command,
    files: await readProjectFiles(root, [".claude/settings.json", "AGENTS.md", ".gitignore"])
  };
}));

results.push(await scenario("init-merge-gitignore-idempotent", async (runtime) => {
  const root = await tempProject(runtime.name, "init-merge-gitignore-idempotent");
  await fs.mkdir(path.join(root, ".claude"), { recursive: true });
  await fs.writeFile(
    path.join(root, ".claude/settings.json"),
    `${JSON.stringify(
      {
        hooks: {
          SessionStart: [{ matcher: "custom", hooks: [{ type: "command", command: "echo custom-session" }] }],
          PostToolUse: [{ matcher: "custom", hooks: [{ type: "command", command: "echo custom-post" }] }]
        },
        mcpServers: {
          keep: { command: "keep", args: ["one"] },
          archiva: { command: "old", args: ["stale"] }
        },
        other: true
      },
      null,
      2
    )}\n`,
    "utf8"
  );
  await fs.writeFile(path.join(root, "AGENTS.md"), "Existing project notes\n", "utf8");
  await fs.writeFile(path.join(root, ".gitignore"), "node_modules/\n", "utf8");

  const first = run(runtime, ["init", "--gitignore-decisions"], "", root);
  const second = run(runtime, ["init", "--gitignore-decisions"], "", root);
  const files = await readProjectFiles(root, [".claude/settings.json", "AGENTS.md", ".gitignore"]);
  const settings = JSON.parse(files[".claude/settings.json"] ?? "{}");
  const agents = files["AGENTS.md"] ?? "";
  const gitignore = files[".gitignore"] ?? "";

  return {
    first,
    second,
    files,
    summary: {
      preservedCustomSessionHook: JSON.stringify(settings).includes("echo custom-session"),
      preservedCustomPostHook: JSON.stringify(settings).includes("echo custom-post"),
      preservedMcpServer: settings.mcpServers?.keep?.command === "keep",
      archivaMcpCommand: settings.mcpServers?.archiva?.command,
      archivaMcpArgs: settings.mcpServers?.archiva?.args,
      sessionHookCount: countOccurrences(JSON.stringify(settings), "archiva hooks session-start"),
      postToolUseHookCount: countOccurrences(JSON.stringify(settings), "archiva hooks post-tool-use"),
      agentsBlockCount: countOccurrences(agents, "## Decision Logging (Archiva)"),
      gitignoreDecisionsCount: countOccurrences(gitignore, ".decisions/")
    }
  };
}));

results.push(await scenario("write-why-session", async (runtime) => {
  const root = await tempProject(runtime.name, "write-why-session");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/a.ts",
      anchor: "fn:makeThing",
      lines: [1, 3],
      chose: "plain function",
      because: "fixture",
      rejected: [{ approach: "class", reason: "unneeded" }],
      session: "sess_diff"
    })],
    "",
    root
  );
  const why = run(runtime, ["why", "src/a.ts", "fn:makeThing"], "", root);
  const session = run(runtime, ["hooks", "session-start"], "", root);
  return {
    write,
    why: normalizeVolatile(why),
    session,
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/a.ts.dlog", ".decisions/src/a.ts.dmap"]))
  };
}));

results.push(await scenario("path-normalization-cli", async (runtime) => {
  const root = await tempProject(runtime.name, "path-normalization-cli");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/path.ts"), "export function pathThing() {\n  return 1;\n}\n", "utf8");
  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: ".//src/path.ts",
      anchor: "fn:pathThing",
      lines: [1, 3],
      chose: "normalized path",
      because: "path normalization fixture",
      rejected: []
    })],
    "",
    root
  );
  const why = run(runtime, ["why", "src\\path.ts", "fn:pathThing"], "", root);
  return {
    write: normalizeVolatile(write),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/path.ts.dlog", ".decisions/src/path.ts.dmap"]))
  };
}));

results.push(await scenario("cli-dash-operand-parity", async (runtime) => {
  const root = await tempProject(runtime.name, "cli-dash-operand-parity");
  return {
    whyOption: run(runtime, ["why", "src/a.ts", "--bad"], "", root),
    whyEscaped: run(runtime, ["why", "src/a.ts", "--", "--bad"], "", root),
    historyOption: run(runtime, ["history", "src/a.ts", "--bad"], "", root),
    historyEscaped: run(runtime, ["history", "src/a.ts", "--", "--bad"], "", root),
    postToolUseOption: run(runtime, ["hooks", "post-tool-use", "--bad"], "", root),
    postToolUseEscaped: run(runtime, ["hooks", "post-tool-use", "--", "--bad"], "", root)
  };
}));

results.push(await knownImprovementScenario(
  "cli-help-error-parity",
  async (runtime) => {
  const root = await tempProject(runtime.name, "cli-help-error-parity");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function a() {\n  return 1;\n}\n", "utf8");
  const payload = JSON.stringify({
    file: "src/a.ts",
    anchor: "fn:a",
    lines: [1, 3],
    chose: "json option decision",
    because: "CLI option input must win over stdin",
    rejected: []
  });
  return {
    rootHelp: run(runtime, ["--help"], "", root),
    helpCommand: run(runtime, ["help"], "", root),
    helpWhy: run(runtime, ["help", "why"], "", root),
    whyHelp: run(runtime, ["why", "--help"], "", root),
    hooksHelp: run(runtime, ["hooks", "--help"], "", root),
    lintUnknownOption: run(runtime, ["lint", "--bad"], "", root),
    writeMissingJson: run(runtime, ["write-decision", "--json"], "", root),
    writeJsonOptionIgnoresStdin: run(runtime, ["write-decision", `--json=${payload}`], "{bad stdin", root),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/a.ts.dlog", ".decisions/src/a.ts.dmap"]))
  };
  },
  (typescript, rust) => {
    // Rust's root help lists a global `-v, --verbose` diagnostic flag that
    // TypeScript lacks (audit blocker B9 observability). Strip that one line
    // wherever it appears, then require every help/error surface to match
    // exactly — so any divergence other than the documented flag still fails.
    const stripVerbose = (value: unknown): string =>
      JSON.stringify(value).replace(/\\n +-v, --verbose +enable diagnostic logging to stderr/g, "");
    return stripVerbose(typescript) === stripVerbose(rust);
  },
  "Rust's root help advertises a global `-v, --verbose` diagnostic-logging flag (audit blocker B9: observability) that the TypeScript CLI does not have; all other help text, error messages, exit codes, and written files are identical."
));

results.push(await knownImprovementScenario(
  "cli-extra-argument-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "cli-extra-argument-hardening-improvement");
    return {
      helpUnknown: run(runtime, ["help", "nope"], "", root),
      helpWhyExtra: run(runtime, ["help", "why", "extra"], "", root),
      statusExtra: run(runtime, ["status", "extra"], "", root),
      mcpExtra: run(runtime, ["mcp", "extra"], "", root)
    };
  },
  (typescript, rust) => {
    return Boolean(
      typescript.helpUnknown.status === 1 &&
      typescript.helpUnknown.stderr.includes("Usage: archiva [options] [command]") &&
      rust.helpUnknown.status === 1 &&
      rust.helpUnknown.stderr === "error: unknown command 'nope'\n" &&
      typescript.helpWhyExtra.status === 0 &&
      typescript.helpWhyExtra.stdout.includes("Usage: archiva why [options] <file> [lineOrAnchor]") &&
      rust.helpWhyExtra.status === 1 &&
      rust.helpWhyExtra.stderr === "error: unexpected argument 'extra'\n" &&
      typescript.statusExtra.status === 0 &&
      typescript.statusExtra.stdout.includes("No decision logs found.") &&
      rust.statusExtra.status === 1 &&
      rust.statusExtra.stderr === "error: unexpected argument 'extra'\n" &&
      typescript.mcpExtra.status === 0 &&
      typescript.mcpExtra.stdout === "" &&
      typescript.mcpExtra.stderr === "" &&
      rust.mcpExtra.status === 1 &&
      rust.mcpExtra.stderr === "error: unexpected argument 'extra'\n"
    );
  },
  "Rust intentionally rejects unexpected help/status/mcp arguments instead of silently printing unrelated help, ignoring arguments, or starting an empty MCP session."
));

results.push(await knownImprovementScenario(
  "path-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "path-hardening-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/double.ts"), "export function double() {\n  return 1;\n}\n", "utf8");
    await fs.writeFile(path.join(root, "src/dot.ts"), "export function dot() {\n  return 1;\n}\n", "utf8");
    await fs.writeFile(path.join(root, "src/parent.ts"), "export function parent() {\n  return 1;\n}\n", "utf8");
    await fs.writeFile(path.join(root, "CON.ts"), "export function reserved() {\n  return 1;\n}\n", "utf8");
    const double = run(runtime, ["write-decision", "--json", JSON.stringify({
      file: "src//double.ts",
      anchor: "fn:double",
      lines: [1, 3],
      chose: "double slash path",
      because: "path hardening fixture",
      rejected: []
    })], "", root);
    const dot = run(runtime, ["write-decision", "--json", JSON.stringify({
      file: "src/./dot.ts",
      anchor: "fn:dot",
      lines: [1, 3],
      chose: "dot segment path",
      because: "path hardening fixture",
      rejected: []
    })], "", root);
    const parent = run(runtime, ["write-decision", "--json", JSON.stringify({
      file: "src/../src/parent.ts",
      anchor: "fn:parent",
      lines: [1, 3],
      chose: "parent segment path",
      because: "path hardening fixture",
      rejected: []
    })], "", root);
    const reserved = run(runtime, ["write-decision", "--json", JSON.stringify({
      file: "CON.ts",
      anchor: "fn:reserved",
      lines: [1, 3],
      chose: "reserved name path",
      because: "path hardening fixture",
      rejected: []
    })], "", root);
    return {
      double: normalizeVolatile(double),
      dot: normalizeVolatile(dot),
      parent: normalizeVolatile(parent),
      reserved: normalizeVolatile(reserved),
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/double.ts.dlog",
        ".decisions/src/dot.ts.dlog",
        ".decisions/src/parent.ts.dlog",
        ".decisions/CON.ts.dlog"
      ]))
    };
  },
  (typescript, rust) => {
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.double.status === 0 &&
      typescript.dot.status === 0 &&
      typescript.parent.status === 0 &&
      typescript.reserved.status === 0 &&
      rust.double.status === 1 &&
      rust.dot.status === 1 &&
      rust.parent.status === 1 &&
      rust.reserved.status === 1 &&
      tsFiles[".decisions/src/double.ts.dlog"]?.includes("double slash path") &&
      tsFiles[".decisions/src/dot.ts.dlog"]?.includes("dot segment path") &&
      tsFiles[".decisions/src/parent.ts.dlog"]?.includes("parent segment path") &&
      tsFiles[".decisions/CON.ts.dlog"]?.includes("reserved name path") &&
      rustFiles[".decisions/src/double.ts.dlog"] === null &&
      rustFiles[".decisions/src/dot.ts.dlog"] === null &&
      rustFiles[".decisions/src/parent.ts.dlog"] === null &&
      rustFiles[".decisions/CON.ts.dlog"] === null
    );
  },
  "Rust intentionally rejects internal empty, dot, parent, and Windows-reserved path segments that TypeScript normalizes or accepts."
));

results.push(await knownImprovementScenario(
  "read-path-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "read-path-hardening-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/read.ts"), "export function readPath() {\n  return 1;\n}\n", "utf8");
    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/read.ts",
        anchor: "fn:readPath",
        lines: [1, 3],
        chose: "read path hardening",
        because: "read-side path hardening fixture",
        rejected: []
      })],
      "",
      root
    );
    const whyParent = run(runtime, ["why", "src/../src/read.ts", "fn:readPath"], "", root);
    const historyDot = run(runtime, ["history", "src/./read.ts", "fn:readPath"], "", root);
    const mcp = run(
      runtime,
      ["mcp"],
      [
        JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "tools/call",
          params: { name: "why", arguments: { file: "src/../src/read.ts", anchor: "fn:readPath" } }
        }),
        JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: { name: "ghost_check", arguments: { file: "src/./read.ts" } }
        })
      ].join("\n"),
      root
    );

    return {
      write: normalizeVolatile(write),
      whyParent: normalizeVolatile(whyParent),
      historyDot: normalizeVolatile(historyDot),
      mcp: {
        status: mcp.status,
        responses: normalizeMcpResponses(mcp.stdout),
        stderr: normalizeText(mcp.stderr)
      },
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/read.ts.dlog",
        ".decisions/src/read.ts.dmap"
      ]))
    };
  },
  (typescript, rust) => {
    const tsResponses = typescript.mcp.responses as Array<{
      result?: { content?: Array<{ text?: string }> };
    }>;
    const rustResponses = rust.mcp.responses as Array<{ error?: { code?: number; message?: string } }>;
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.write.status === 0 &&
      rust.write.status === 0 &&
      typescript.whyParent.status === 0 &&
      typescript.whyParent.stdout.includes("read path hardening") &&
      rust.whyParent.status === 1 &&
      rust.whyParent.stderr.includes("parent path segments are not allowed") &&
      typescript.historyDot.status === 0 &&
      typescript.historyDot.stdout.includes("dec_001") &&
      rust.historyDot.status === 1 &&
      rust.historyDot.stderr.includes("dot path segments are not allowed") &&
      typescript.mcp.status === 0 &&
      rust.mcp.status === 0 &&
      tsResponses[0]?.result?.content?.[0]?.text?.includes("read path hardening") &&
      tsResponses[1]?.result?.content?.[0]?.text === "No issues found for src/./read.ts." &&
      rustResponses[0]?.error?.code === -32000 &&
      rustResponses[0]?.error?.message?.includes("parent path segments are not allowed") &&
      rustResponses[1]?.error?.code === -32000 &&
      rustResponses[1]?.error?.message?.includes("dot path segments are not allowed") &&
      tsFiles[".decisions/src/read.ts.dlog"]?.includes("read path hardening") &&
      tsFiles[".decisions/src/read.ts.dmap"] === "1-3:fn:readPath\n" &&
      rustFiles[".decisions/src/read.ts.dlog"]?.includes("read path hardening") &&
      rustFiles[".decisions/src/read.ts.dmap"] === "1-3:fn:readPath\n"
    );
  },
  "Rust intentionally applies hardened project-relative path validation to CLI and MCP read-side paths while TypeScript normalizes internal dot and parent segments."
));

results.push(await knownImprovementScenario(
  "source-symlink-escape-hardening-improvement",
  async (runtime) => {
    if (process.platform === "win32") {
      return { skipped: "windows-symlink-privileges" };
    }
    const root = await tempProject(runtime.name, "source-symlink-escape-hardening-improvement");
    const outside = await tempProject(runtime.name, "source-symlink-escape-hardening-outside");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.mkdir(outside, { recursive: true });
    await fs.writeFile(path.join(outside, "linked.ts"), "export function linked() {\n  return 1;\n}\n", "utf8");
    await fs.symlink(path.join(outside, "linked.ts"), path.join(root, "src/linked.ts"));
    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/linked.ts",
        anchor: "fn:linked",
        lines: [1, 3],
        chose: "reject source symlink escape",
        because: "source symlink escape hardening fixture",
        rejected: []
      })],
      "",
      root
    );

    return {
      write: normalizeVolatile(write),
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/linked.ts.dlog",
        ".decisions/src/linked.ts.dmap",
        ".decisions/src/linked.ts.lock"
      ])),
      tempSiblings: await decisionTempSiblings(root, "src")
    };
  },
  (typescript, rust) => {
    if (typescript.skipped === "windows-symlink-privileges" && rust.skipped === "windows-symlink-privileges") {
      return true;
    }
    const tsResult = typescript as {
      write: CommandResult;
      files: Record<string, string | null>;
      tempSiblings: string[];
    };
    const rustResult = rust as {
      write: CommandResult;
      files: Record<string, string | null>;
      tempSiblings: string[];
    };
    const tsFiles = tsResult.files;
    const rustFiles = rustResult.files;
    return Boolean(
      tsResult.write.status === 0 &&
      rustResult.write.status === 1 &&
      rustResult.write.stderr.includes("path resolves outside the project root") &&
      tsFiles[".decisions/src/linked.ts.dlog"]?.includes("reject source symlink escape") &&
      tsFiles[".decisions/src/linked.ts.dmap"] === "1-3:fn:linked\n" &&
      tsFiles[".decisions/src/linked.ts.lock"] === null &&
      rustFiles[".decisions/src/linked.ts.dlog"] === null &&
      rustFiles[".decisions/src/linked.ts.dmap"] === null &&
      rustFiles[".decisions/src/linked.ts.lock"] === null &&
      tsResult.tempSiblings.length === 0 &&
      rustResult.tempSiblings.length === 0
    );
  },
  "Rust intentionally rejects source files whose existing symlink target canonicalizes outside the project root while TypeScript follows the symlink and writes decision storage."
));

results.push(await knownImprovementScenario(
  "corrupt-dlog-command-failures",
  async (runtime) => {
  const root = await tempProject(runtime.name, "corrupt-dlog-command-failures");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/bad.ts"), "function bad() {\n  return 1;\n}\n", "utf8");
  const corruptDlog = "schema: nope\nfile: src/bad.ts\ndecisions: {}\n";
  await fs.writeFile(path.join(root, ".decisions/src/bad.ts.dlog"), corruptDlog, "utf8");

  const why = run(runtime, ["why", "src/bad.ts"], "", root);
  const lint = run(runtime, ["lint"], "", root);
  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/bad.ts",
      anchor: "fn:bad",
      lines: [1, 3],
      chose: "do not overwrite corruption",
      because: "corrupt dlog differential fixture",
      rejected: []
    })],
    "",
    root
  );

  return {
    why: normalizeCorruptDlogFailure(why),
    lint: {
      status: lint.status,
      // Rust skip-and-reports the corrupt file as an `arc/corrupt` lint issue
      // on stdout; TypeScript aborts with the schema error on stderr. Reduce to
      // the shared signal (non-zero exit + corruption surfaced *somewhere*) so
      // the intentional B5 behavior change is accepted while a regression that
      // silently ignored the corrupt file would still fail.
      surfacesCorruption:
        /arc\/corrupt/.test(lint.stdout) || /schema/.test(lint.stderr)
    },
    write: normalizeCorruptDlogFailure(write),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/bad.ts.dlog",
      ".decisions/src/bad.ts.dmap",
      ".decisions/src/bad.ts.lock"
    ])),
    tempSiblings: await decisionTempSiblings(root, "src")
  };
  },
  (typescript, rust) => {
    // `lint` is the intentional divergence (B5 skip-and-report). Everything
    // else — why, write, on-disk files, temp siblings — must match exactly, and
    // both runtimes must still exit non-zero from lint having surfaced the
    // corruption.
    const tsLint = typescript.lint as { status: number; surfacesCorruption: boolean };
    const rustLint = rust.lint as { status: number; surfacesCorruption: boolean };
    return (
      JSON.stringify(typescript.why) === JSON.stringify(rust.why) &&
      JSON.stringify(typescript.write) === JSON.stringify(rust.write) &&
      JSON.stringify(typescript.files) === JSON.stringify(rust.files) &&
      JSON.stringify(typescript.tempSiblings) === JSON.stringify(rust.tempSiblings) &&
      tsLint.status === 1 &&
      rustLint.status === 1 &&
      tsLint.surfacesCorruption &&
      rustLint.surfacesCorruption
    );
  },
  "Rust whole-repo `lint` intentionally skips-and-reports a corrupt .dlog as an `arc/corrupt` error issue (naming the file) instead of aborting the whole command like TypeScript (audit blocker B5); single-file `why`/`write` still fail hard identically and the corrupt file is left untouched."
));

results.push(await knownImprovementScenario(
  "dlog-yaml-depth-limit-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "dlog-yaml-depth-limit-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/deep.ts"), "export function deep() {\n  return 1;\n}\n", "utf8");
    const deepIgnoredValue = `${"[".repeat(520)}0${"]".repeat(520)}`;
    const dlog = `file: src/deep.ts
schema: 1
decisions:
  fn:deep:
    id: dec_001
    lines_hint: [1, 3]
    fingerprint: abc123ef
    chose: bounded yaml depth
    because: depth hardening fixture
    rejected: []
    timestamp: '2026-06-26T20:31:18.340Z'
    history: []
ignored: ${deepIgnoredValue}
`;
    await fs.writeFile(path.join(root, ".decisions/src/deep.ts.dlog"), dlog, "utf8");

    const why = run(runtime, ["why", "src/deep.ts", "fn:deep"], "", root);
    return {
      why: normalizeVolatile(why),
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/deep.ts.dlog",
        ".decisions/src/deep.ts.dmap",
        ".decisions/src/deep.ts.lock"
      ])),
      tempSiblings: await decisionTempSiblings(root, "src")
    };
  },
  (typescript, rust) => {
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.why.status === 0 &&
      typescript.why.stdout.includes("bounded yaml depth") &&
      rust.why.status === 1 &&
      rust.why.stdout === "" &&
      rust.why.stderr.includes("YAML nesting exceeds configured depth limit") &&
      tsFiles[".decisions/src/deep.ts.dlog"] === rustFiles[".decisions/src/deep.ts.dlog"] &&
      tsFiles[".decisions/src/deep.ts.dmap"] === null &&
      rustFiles[".decisions/src/deep.ts.dmap"] === null &&
      tsFiles[".decisions/src/deep.ts.lock"] === null &&
      rustFiles[".decisions/src/deep.ts.lock"] === null &&
      typescript.tempSiblings.length === 0 &&
      rust.tempSiblings.length === 0
    );
  },
  "Rust intentionally caps YAML nesting in .dlog reads to reject adversarial depth that TypeScript's js-yaml parser accepts."
));

results.push(await knownImprovementScenario(
  "write-existing-lock-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "write-existing-lock-hardening-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/locked.ts"), "export function locked() {\n  return 1;\n}\n", "utf8");
    const lockContent = `version=1\npid=${process.pid}\ntoken=active\ncommand=other\ntimestamp=2099-01-01T00:00:00.000Z\n`;
    await fs.writeFile(path.join(root, ".decisions/src/locked.ts.lock"), lockContent, "utf8");

    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/locked.ts",
        anchor: "fn:locked",
        lines: [1, 3],
        chose: "respect existing lock",
        because: "lock hardening differential fixture",
        rejected: []
      })],
      "",
      root
    );

    return {
      write: normalizeVolatile(write),
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/locked.ts.dlog",
        ".decisions/src/locked.ts.dmap",
        ".decisions/src/locked.ts.lock"
      ])),
      tempSiblings: await decisionTempSiblings(root, "src")
    };
  },
  (typescript, rust) => {
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.write.status === 0 &&
      rust.write.status === 1 &&
      rust.write.stderr.includes("Archiva lock already exists") &&
      tsFiles[".decisions/src/locked.ts.dlog"]?.includes("respect existing lock") &&
      tsFiles[".decisions/src/locked.ts.dmap"] === "1-3:fn:locked\n" &&
      tsFiles[".decisions/src/locked.ts.lock"]?.includes("command=other") &&
      rustFiles[".decisions/src/locked.ts.dlog"] === null &&
      rustFiles[".decisions/src/locked.ts.dmap"] === null &&
      rustFiles[".decisions/src/locked.ts.lock"]?.includes("command=other") &&
      Array.isArray(typescript.tempSiblings) &&
      typescript.tempSiblings.length === 0 &&
      Array.isArray(rust.tempSiblings) &&
      rust.tempSiblings.length === 0
    );
  },
  "Rust intentionally refuses write_decision while a live lock exists, preserving the lock and avoiding partial dlog/dmap writes, while TypeScript ignores the lock file."
));

results.push(await scenario("explain-history-session-text", async (runtime) => {
  const root = await tempProject(runtime.name, "explain-history-session-text");
  await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
  await fs.writeFile(
    path.join(root, ".decisions/src/explain.ts.dlog"),
    `file: src/explain.ts
schema: 1
decisions:
  fn:first:
    id: dec_001
    lines_hint:
      - 1
      - 3
    fingerprint: '11111111'
    chose: first approach with extra whitespace
    because: first reason
    rejected:
      - approach: class wrapper
        reason: adds no behavior
      - approach: global helper
        reason: hides coupling
      - approach: third hidden
        reason: not shown in session map
    expires_if: api changes
    session: sess_a
    timestamp: '2026-06-26T20:31:18.340Z'
    history:
      - id: dec_000
        chose: older approach
        because: older reason
        timestamp: '2026-06-25T10:00:00.000Z'
        superseded_reason: first reason
    status: STALE
    stale_since: '2026-06-26T21:00:00.000Z'
  fn:second:
    id: dec_002
    lines_hint:
      - 5
      - 8
    fingerprint: '22222222'
    chose: |-
      second approach
      with newlines and      spaces
    because: second reason
    rejected: []
    timestamp: '2026-06-26T20:32:18.340Z'
    history: []
`,
    "utf8"
  );

  return {
    why: run(runtime, ["why", "src/explain.ts", "fn:first"], "", root),
    whyLine: run(runtime, ["why", "src/explain.ts", "6"], "", root),
    whyAll: run(runtime, ["why", "src/explain.ts"], "", root),
    history: run(runtime, ["history", "src/explain.ts", "fn:first"], "", root),
    session: run(runtime, ["hooks", "session-start"], "", root)
  };
}));

results.push(await scenario("write-stdin-env-supersedes", async (runtime) => {
  const root = await tempProject(runtime.name, "write-stdin-env-supersedes");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/history.ts"), "function next() {\n  return 1;\n}\n", "utf8");

  const first = run(
    runtime,
    ["write-decision"],
    JSON.stringify({
      file: "src/history.ts",
      anchor: "fn:next",
      lines: [1, 3],
      chose: "first approach",
      because: "fixture setup",
      rejected: []
    }),
    root
  );
  const second = run(
    runtime,
    ["write-decision"],
    JSON.stringify({
      file: "src/history.ts",
      anchor: "fn:next",
      lines: [1, 3],
      chose: "second approach",
      because: "new fixture reason",
      rejected: [],
      supersedes: "dec_001"
    }),
    root
  );
  const history = run(runtime, ["history", "src/history.ts", "fn:next"], "", root);
  const why = run(runtime, ["why", "src/history.ts", "fn:next"], "", root);

  return {
    first: normalizeVolatile(first),
    second: normalizeVolatile(second),
    history: normalizeVolatile(history),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/history.ts.dlog", ".decisions/src/history.ts.dmap"]))
  };
}));

results.push(await scenario("modern-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "modern-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/modern.ts"),
    `export async function fetchData() { await run(); }
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/function-expressions.ts"),
    `const plain = function() { return 1; };
const named = function hidden() { return 2; };
const asyncNamed = async function hiddenAsync() { await run(); };
export const exported = async function exportedHidden() { await run(); };
const paren = (function parenHidden() { return 3; });
const call = foo(function callHidden() { return 4; });
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/typed-initializers.ts"),
    `const objectTyped: { a: number, b: number } = () => true;
const tupleTyped: [number, string] = function hiddenTuple() { return [1, "x"]; };
export const genericTyped: Promise<string | number> = async () => "x";
const unionTyped: (() => number) | null = () => 1;
const plain = () => true;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/type-operator-wrapped-arrows.ts"),
    `const satisfiesRun = ((x: number) => x) satisfies (x: number) => number;
const asRun = ((x: number) => x) as (x: number) => number;
export const exported = ((x: number) => x) satisfies (x: number) => number;
const direct = (x: number) => x satisfies number;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/using-declarations.ts"),
    `using disposable = () => true;
await using asyncDisposable = async () => true;
export using exportedDisposable = function hidden() { return true; };
export await using exportedAsyncDisposable = async function hiddenAsync() { return true; };
using resource = acquire();
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/default-generator.ts"),
    "export default function*() { yield 1; }\nfunction real() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/named-default-generator.ts"),
    "export default function* named() { yield 1; }\nfunction real() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/class-expressions.ts"),
    `const Plain = class { method() {} };
const Named = class Inner { method() {} };
export const Exported = class ExportedInner { method() {} };
const Nested = foo(class NestedInner { method() {} });
class Real { method() {} }
function later() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/class-fields.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/semicolonless-class-fields.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/multiline-class-fields.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/field-followed-by-generator.ts"),
    `class C {
  field = 1
  *items() { yield 1; }
  other = 2
  async save() { return 1; }
}
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/field-followed-by-generator-semicolon.ts"),
    `class C {
  field = 1;
  *items() { yield 1; }
}
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/optional-methods.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/destructured-functions.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/unicode-identifiers.ts"),
    `export function café() { return 1; }
const π = () => true;
export const 名前 = () => true;
class 店 { 開く() { return true; } }
export { π as piAlias };
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/unicode-escapes.ts"),
    String.raw`export function caf\u00e9() { return 1; }
const \u03c0 = () => true;
const \u{03c0}Brace = () => true;
export const \u540d\u524d = () => true;
class \u5e97 { \u958b\u304f() { return true; } }
export { \u03c0 as piAlias };
function after() {}
`,
    "utf8"
  );

  const writes = [
    ["export:fetchData", [1, 1], "exported async function"],
    ["export:default", [2, 2], "first default export"],
    ["export:value", [4, 4], "plain exported value"],
    ["export:renamed", [5, 5], "renamed export alias"],
    ["fn:C.[computed]", [10, 10], "computed class method"],
    ["fn:C.#secret", [11, 11], "private class method"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/modern.ts",
        anchor,
        lines,
        chose,
        because: "modern anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const functionExpressionWrites = [
    ["fn:plain", [1, 1], "plain function expression variable"],
    ["fn:named", [2, 2], "named function expression variable"],
    ["fn:asyncNamed", [3, 3], "async named function expression variable"],
    ["fn:exported", [4, 4], "exported async function expression variable"],
    ["export:exported", [4, 4], "exported async function expression alias"],
    ["fn:real", [7, 7], "real function after function expressions"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/function-expressions.ts",
        anchor,
        lines,
        chose,
        because: "function expression variable initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidFunctionExpressionWrites = [
    "fn:hidden",
    "fn:hiddenAsync",
    "fn:exportedHidden",
    "fn:paren",
    "fn:parenHidden",
    "fn:call",
    "fn:callHidden"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/function-expressions.ts",
        anchor,
        lines: [1, 7],
        chose: "function expression parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const typedVariableWrites = [
    ["fn:objectTyped", [1, 1], "object type annotation arrow initializer"],
    ["fn:tupleTyped", [2, 2], "tuple type annotation function initializer"],
    ["fn:genericTyped", [3, 3], "generic type annotation async initializer"],
    ["export:genericTyped", [3, 3], "generic type annotation export"],
    ["fn:unionTyped", [4, 4], "union type annotation arrow initializer"],
    ["fn:plain", [5, 5], "plain arrow initializer after typed variables"],
    ["fn:after", [6, 6], "function after typed variables"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/typed-initializers.ts",
        anchor,
        lines,
        chose,
        because: "typed variable initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidTypedVariableWrites = ["fn:b", "fn:string", "fn:hiddenTuple"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/typed-initializers.ts",
        anchor,
        lines: [1, 6],
        chose: "typed variable initializer parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const typeOperatorWrappedArrowWrites = [
    ["export:exported", [3, 3], "satisfies-wrapped exported variable"],
    ["fn:direct", [4, 4], "direct arrow with satisfies expression body"],
    ["fn:after", [5, 5], "function after type-operator wrapped arrows"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/type-operator-wrapped-arrows.ts",
        anchor,
        lines,
        chose,
        because: "type operator wrapped arrow initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidTypeOperatorWrappedArrowWrites = ["fn:satisfiesRun", "fn:asRun", "fn:exported"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/type-operator-wrapped-arrows.ts",
        anchor,
        lines: [1, 5],
        chose: "type operator wrapped arrow parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const usingDeclarationWrites = [
    ["fn:disposable", [1, 1], "using arrow initializer"],
    ["fn:asyncDisposable", [2, 2], "await using async arrow initializer"],
    ["fn:exportedDisposable", [3, 3], "exported using function initializer"],
    ["fn:exportedAsyncDisposable", [4, 4], "exported await using async function initializer"],
    ["export:exportedDisposable", [3, 3], "exported using alias"],
    ["export:exportedAsyncDisposable", [4, 4], "exported await using alias"],
    ["fn:after", [6, 6], "function after using declarations"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/using-declarations.ts",
        anchor,
        lines,
        chose,
        because: "using declaration function initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidUsingDeclarationWrites = ["fn:hidden", "fn:hiddenAsync", "fn:resource", "export:resource"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/using-declarations.ts",
        anchor,
        lines: [1, 6],
        chose: "using declaration parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const defaultGeneratorWrites = [
    ["src/default-generator.ts", "export:default", [1, 1], "anonymous default generator export"],
    ["src/default-generator.ts", "fn:real", [2, 2], "function after anonymous default generator"],
    ["src/named-default-generator.ts", "fn:named", [1, 1], "named default generator function"],
    ["src/named-default-generator.ts", "export:default", [1, 1], "named default generator export"],
    ["src/named-default-generator.ts", "fn:real", [2, 2], "function after named default generator"]
  ].map(([file, anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor,
        lines,
        chose,
        because: "default generator anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidDefaultGeneratorYield = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/default-generator.ts",
      anchor: "fn:yield",
      lines: [1, 1],
      chose: "anonymous default generator parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const classExpressionWrites = [
    ["export:Exported", [3, 3], "exported class expression variable"],
    ["class:Real", [5, 5], "real class declaration after class expressions"],
    ["fn:Real.method", [5, 5], "real class method after class expressions"],
    ["fn:later", [6, 6], "function after class expressions"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/class-expressions.ts",
        anchor,
        lines,
        chose,
        because: "class expression suppression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidClassExpressionWrites = [
    "class:Inner",
    "fn:Inner.method",
    "class:ExportedInner",
    "fn:ExportedInner.method",
    "class:NestedInner",
    "fn:NestedInner.method",
    "fn:Plain.method"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/class-expressions.ts",
        anchor,
        lines: [1, 4],
        chose: "class expression parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const classFieldWrites = [
    ["class:C", [1, 12], "class with function-valued fields"],
    ["fn:C.method", [8, 8], "method after class fields"],
    ["fn:C.#method", [9, 9], "private method after private field"],
    ["fn:C.[methodName]", [10, 10], "computed method after computed field"],
    ['fn:C."quotedMethod"', [11, 11], "quoted method after quoted field"],
    ["fn:after", [13, 13], "function after class fields"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/class-fields.ts",
        anchor,
        lines,
        chose,
        because: "class field initializer suppression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidClassFieldWrites = [
    "fn:C.field",
    "fn:C.staticField",
    "fn:C.hidden",
    "fn:C.privateHidden",
    "fn:C.hiddenComputed",
    "fn:C.hiddenQuoted",
    "fn:C.auto"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/class-fields.ts",
        anchor,
        lines: [1, 13],
        chose: "class field initializer parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const semicolonlessClassFieldWrites = [
    ["class:C", [1, 8], "semicolonless class field recovery range"],
    ["fn:C.method", [3, 3], "method after semicolonless public field"],
    ["fn:C.#method", [6, 6], "private method after semicolonless private field"],
    ["fn:after", [10, 10], "function after semicolonless class fields"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/semicolonless-class-fields.ts",
        anchor,
        lines,
        chose,
        because: "semicolonless class field recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidSemicolonlessClassFieldWrites = [
    "fn:C.field",
    "fn:C.hidden",
    "fn:C.privateField",
    "fn:C.hiddenComputed",
    "fn:C.[methodName]"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/semicolonless-class-fields.ts",
        anchor,
        lines: [1, 10],
        chose: "semicolonless class field parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const multilineClassFieldWrites = [
    ["class:C", [1, 9], "multiline class field range"],
    ["fn:C.method", [4, 4], "method after multiline arrow field"],
    ["fn:C.afterData", [8, 8], "method after multiline object field"],
    ["fn:after", [10, 10], "function after multiline class fields"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/multiline-class-fields.ts",
        anchor,
        lines,
        chose,
        because: "multiline class field recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidMultilineClassFieldWrites = ["fn:C.run", "fn:C.field"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/multiline-class-fields.ts",
        anchor,
        lines: [1, 10],
        chose: "multiline class field parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const fieldBeforeGeneratorWrites = [
    ["src/field-followed-by-generator.ts", "class:C", [1, 3], "semicolonless field generator recovery range"],
    ["src/field-followed-by-generator.ts", "fn:after", [7, 7], "function after semicolonless field generator recovery"],
    ["src/field-followed-by-generator-semicolon.ts", "class:C", [1, 4], "semicolon-protected field generator class"],
    ["src/field-followed-by-generator-semicolon.ts", "fn:C.items", [3, 3], "generator method after semicolon field"],
    ["src/field-followed-by-generator-semicolon.ts", "fn:after", [5, 5], "function after semicolon-protected generator method"]
  ].map(([file, anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor,
        lines,
        chose,
        because: "semicolonless field before generator parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidFieldBeforeGeneratorWrites = ["fn:C.items", "fn:C.save"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/field-followed-by-generator.ts",
        anchor,
        lines: [1, 7],
        chose: "semicolonless field generator parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const optionalMethodWrites = [
    ["class:C", [1, 8], "optional method class"],
    ["fn:C.optional", [2, 2], "optional method"],
    ["fn:C.required", [3, 3], "required method"],
    ['fn:C."quoted"', [4, 4], "optional quoted method"],
    ["fn:C.42", [5, 5], "optional numeric method"],
    ["fn:C.[computed]", [6, 6], "optional computed method"],
    ["fn:C.#secret", [7, 7], "optional private method"],
    ["class:AbstractC", [9, 9], "abstract class with method signatures"],
    ["fn:AbstractC.optional", [9, 9], "abstract optional method signature"],
    ["fn:AbstractC.required", [9, 9], "abstract required method signature"],
    ["fn:AbstractC.concrete", [9, 9], "abstract class concrete method"],
    ["class:DeclaredC", [10, 10], "declared class with optional method signatures"],
    ["fn:DeclaredC.optional", [10, 10], "declared optional method signature"],
    ["fn:DeclaredC.required", [10, 10], "declared required method signature"],
    ["class:SignatureOnly", [11, 11], "non-abstract signature-only class"],
    ["fn:SignatureOnly.concrete", [11, 11], "non-abstract concrete method after signatures"],
    ["fn:after", [12, 12], "function after optional methods"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/optional-methods.ts",
        anchor,
        lines,
        chose,
        because: "optional class method parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidOptionalMethodWrites = ["fn:SignatureOnly.optional", "fn:SignatureOnly.required"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/optional-methods.ts",
        anchor,
        lines: [11, 11],
        chose: "non-abstract signature parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const destructuredFunctionWrites = [
    ["export:a", [2, 5], "destructured function default export"],
    ["export:b", [6, 9], "destructured arrow default export"],
    ["export:c", [10, 10], "destructured shorthand export"],
    ["export:alias", [11, 11], "destructured alias export"],
    ["export:deep", [12, 12], "nested destructured export"],
    ["export:rest", [13, 13], "destructured rest export"],
    ["export:first", [15, 15], "array destructured function default export"],
    ["export:second", [15, 15], "array destructured arrow default export"],
    ["export:third", [15, 15], "array destructured plain export"],
    ["fn:real", [16, 16], "function after destructuring"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/destructured-functions.ts",
        anchor,
        lines,
        chose,
        because: "destructuring function initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidDestructuredFunctionWrites = [
    "fn:a",
    "fn:b",
    "fn:first",
    "fn:second",
    "fn:hidden",
    "fn:hiddenArray",
    "fn:deep",
    "fn:nested"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/destructured-functions.ts",
        anchor,
        lines: [1, 16],
        chose: "destructuring function parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const unicodeIdentifierWrites = [
    ["fn:café", [1, 1], "unicode function identifier"],
    ["fn:π", [2, 2], "unicode variable arrow identifier"],
    ["fn:名前", [3, 3], "unicode exported variable identifier"],
    ["class:店", [4, 4], "unicode class identifier"],
    ["fn:店.開く", [4, 4], "unicode class method identifier"],
    ["export:café", [1, 1], "unicode function export"],
    ["export:名前", [3, 3], "unicode variable export"],
    ["export:piAlias", [2, 2], "unicode alias export"],
    ["fn:after", [6, 6], "function after unicode identifiers"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/unicode-identifiers.ts",
        anchor,
        lines,
        chose,
        because: "unicode identifier parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const escapedUnicodeIdentifierWrites = [
    ["fn:café", [1, 1], "escaped unicode function identifier"],
    ["fn:π", [2, 2], "escaped unicode variable arrow identifier"],
    ["fn:πBrace", [3, 3], "braced escaped unicode variable identifier"],
    ["fn:名前", [4, 4], "escaped unicode exported variable identifier"],
    ["class:店", [5, 5], "escaped unicode class identifier"],
    ["fn:店.開く", [5, 5], "escaped unicode class method identifier"],
    ["export:café", [1, 1], "escaped unicode function export"],
    ["export:名前", [4, 4], "escaped unicode variable export"],
    ["export:piAlias", [2, 2], "escaped unicode alias export"],
    ["fn:after", [7, 7], "function after escaped unicode identifiers"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/unicode-escapes.ts",
        anchor,
        lines,
        chose,
        because: "escaped unicode identifier parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidEscapedUnicodeIdentifierWrites = [
    "fn:caf",
    "export:caf",
    "fn:u03c0",
    "fn:u540d",
    "class:u5e97",
    "fn:u958b"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/unicode-escapes.ts",
        anchor,
        lines: [1, 7],
        chose: "escaped unicode parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/modern.ts"], "", root);
  const functionExpressionWhy = run(runtime, ["why", "src/function-expressions.ts"], "", root);
  const typedVariableWhy = run(runtime, ["why", "src/typed-initializers.ts"], "", root);
  const typeOperatorWrappedArrowWhy = run(runtime, ["why", "src/type-operator-wrapped-arrows.ts"], "", root);
  const usingDeclarationWhy = run(runtime, ["why", "src/using-declarations.ts"], "", root);
  const defaultGeneratorWhy = run(runtime, ["why", "src/default-generator.ts"], "", root);
  const namedDefaultGeneratorWhy = run(runtime, ["why", "src/named-default-generator.ts"], "", root);
  const classExpressionWhy = run(runtime, ["why", "src/class-expressions.ts"], "", root);
  const classFieldWhy = run(runtime, ["why", "src/class-fields.ts"], "", root);
  const semicolonlessClassFieldWhy = run(runtime, ["why", "src/semicolonless-class-fields.ts"], "", root);
  const multilineClassFieldWhy = run(runtime, ["why", "src/multiline-class-fields.ts"], "", root);
  const fieldBeforeGeneratorWhy = run(runtime, ["why", "src/field-followed-by-generator.ts"], "", root);
  const fieldBeforeGeneratorSemicolonWhy = run(runtime, ["why", "src/field-followed-by-generator-semicolon.ts"], "", root);
  const optionalMethodWhy = run(runtime, ["why", "src/optional-methods.ts"], "", root);
  const destructuredFunctionWhy = run(runtime, ["why", "src/destructured-functions.ts"], "", root);
  const unicodeIdentifierWhy = run(runtime, ["why", "src/unicode-identifiers.ts"], "", root);
  const escapedUnicodeIdentifierWhy = run(runtime, ["why", "src/unicode-escapes.ts"], "", root);

  return {
    writes: writes.map(normalizeVolatile),
    functionExpressionWrites: functionExpressionWrites.map(normalizeVolatile),
    invalidFunctionExpressionWrites: invalidFunctionExpressionWrites.map(normalizeVolatile),
    typedVariableWrites: typedVariableWrites.map(normalizeVolatile),
    invalidTypedVariableWrites: invalidTypedVariableWrites.map(normalizeVolatile),
    typeOperatorWrappedArrowWrites: typeOperatorWrappedArrowWrites.map(normalizeVolatile),
    invalidTypeOperatorWrappedArrowWrites: invalidTypeOperatorWrappedArrowWrites.map(normalizeVolatile),
    usingDeclarationWrites: usingDeclarationWrites.map(normalizeVolatile),
    invalidUsingDeclarationWrites: invalidUsingDeclarationWrites.map(normalizeVolatile),
    defaultGeneratorWrites: defaultGeneratorWrites.map(normalizeVolatile),
    invalidDefaultGeneratorYield: normalizeVolatile(invalidDefaultGeneratorYield),
    classExpressionWrites: classExpressionWrites.map(normalizeVolatile),
    invalidClassExpressionWrites: invalidClassExpressionWrites.map(normalizeVolatile),
    classFieldWrites: classFieldWrites.map(normalizeVolatile),
    invalidClassFieldWrites: invalidClassFieldWrites.map(normalizeVolatile),
    semicolonlessClassFieldWrites: semicolonlessClassFieldWrites.map(normalizeVolatile),
    invalidSemicolonlessClassFieldWrites: invalidSemicolonlessClassFieldWrites.map(normalizeVolatile),
    multilineClassFieldWrites: multilineClassFieldWrites.map(normalizeVolatile),
    invalidMultilineClassFieldWrites: invalidMultilineClassFieldWrites.map(normalizeVolatile),
    fieldBeforeGeneratorWrites: fieldBeforeGeneratorWrites.map(normalizeVolatile),
    invalidFieldBeforeGeneratorWrites: invalidFieldBeforeGeneratorWrites.map(normalizeVolatile),
    optionalMethodWrites: optionalMethodWrites.map(normalizeVolatile),
    invalidOptionalMethodWrites: invalidOptionalMethodWrites.map(normalizeVolatile),
    destructuredFunctionWrites: destructuredFunctionWrites.map(normalizeVolatile),
    invalidDestructuredFunctionWrites: invalidDestructuredFunctionWrites.map(normalizeVolatile),
    unicodeIdentifierWrites: unicodeIdentifierWrites.map(normalizeVolatile),
    escapedUnicodeIdentifierWrites: escapedUnicodeIdentifierWrites.map(normalizeVolatile),
    invalidEscapedUnicodeIdentifierWrites: invalidEscapedUnicodeIdentifierWrites.map(normalizeVolatile),
    why: normalizeVolatile(why),
    functionExpressionWhy: normalizeVolatile(functionExpressionWhy),
    typedVariableWhy: normalizeVolatile(typedVariableWhy),
    typeOperatorWrappedArrowWhy: normalizeVolatile(typeOperatorWrappedArrowWhy),
    usingDeclarationWhy: normalizeVolatile(usingDeclarationWhy),
    defaultGeneratorWhy: normalizeVolatile(defaultGeneratorWhy),
    namedDefaultGeneratorWhy: normalizeVolatile(namedDefaultGeneratorWhy),
    classExpressionWhy: normalizeVolatile(classExpressionWhy),
    classFieldWhy: normalizeVolatile(classFieldWhy),
    semicolonlessClassFieldWhy: normalizeVolatile(semicolonlessClassFieldWhy),
    multilineClassFieldWhy: normalizeVolatile(multilineClassFieldWhy),
    fieldBeforeGeneratorWhy: normalizeVolatile(fieldBeforeGeneratorWhy),
    fieldBeforeGeneratorSemicolonWhy: normalizeVolatile(fieldBeforeGeneratorSemicolonWhy),
    optionalMethodWhy: normalizeVolatile(optionalMethodWhy),
    destructuredFunctionWhy: normalizeVolatile(destructuredFunctionWhy),
    unicodeIdentifierWhy: normalizeVolatile(unicodeIdentifierWhy),
    escapedUnicodeIdentifierWhy: normalizeVolatile(escapedUnicodeIdentifierWhy),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/modern.ts.dlog",
      ".decisions/src/modern.ts.dmap",
      ".decisions/src/function-expressions.ts.dlog",
      ".decisions/src/function-expressions.ts.dmap",
      ".decisions/src/typed-initializers.ts.dlog",
      ".decisions/src/typed-initializers.ts.dmap",
      ".decisions/src/type-operator-wrapped-arrows.ts.dlog",
      ".decisions/src/type-operator-wrapped-arrows.ts.dmap",
      ".decisions/src/using-declarations.ts.dlog",
      ".decisions/src/using-declarations.ts.dmap",
      ".decisions/src/default-generator.ts.dlog",
      ".decisions/src/default-generator.ts.dmap",
      ".decisions/src/named-default-generator.ts.dlog",
      ".decisions/src/named-default-generator.ts.dmap",
      ".decisions/src/class-expressions.ts.dlog",
      ".decisions/src/class-expressions.ts.dmap",
      ".decisions/src/class-fields.ts.dlog",
      ".decisions/src/class-fields.ts.dmap",
      ".decisions/src/semicolonless-class-fields.ts.dlog",
      ".decisions/src/semicolonless-class-fields.ts.dmap",
      ".decisions/src/multiline-class-fields.ts.dlog",
      ".decisions/src/multiline-class-fields.ts.dmap",
      ".decisions/src/field-followed-by-generator.ts.dlog",
      ".decisions/src/field-followed-by-generator.ts.dmap",
      ".decisions/src/field-followed-by-generator-semicolon.ts.dlog",
      ".decisions/src/field-followed-by-generator-semicolon.ts.dmap",
      ".decisions/src/optional-methods.ts.dlog",
      ".decisions/src/optional-methods.ts.dmap",
      ".decisions/src/destructured-functions.ts.dlog",
      ".decisions/src/destructured-functions.ts.dmap",
      ".decisions/src/unicode-identifiers.ts.dlog",
      ".decisions/src/unicode-identifiers.ts.dmap",
      ".decisions/src/unicode-escapes.ts.dlog",
      ".decisions/src/unicode-escapes.ts.dmap"
    ]))
  };
}));

results.push(await scenario("type-question-complexity-lint", async (runtime) => {
  const root = await tempProject(runtime.name, "type-question-complexity-lint");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/type-question-complexity.ts"),
    `function optionalParams(a?: string, b?: string, c?: string, d?: string, e?: string) {
  return a;
}
function conditionalTypes(
  a: T extends A ? B : C,
  b: U extends D ? E : F,
  c: V extends G ? H : I,
  d: W extends J ? K : L,
  e: X extends M ? N : O
) {
  return a;
}
function runtimeTernaries(a: boolean, b: boolean, c: boolean, d: boolean) {
  return a ? 1 : b ? 2 : c ? 3 : d ? 4 : 5;
}
`,
    "utf8"
  );

  const lint = run(runtime, ["lint"], "", root);
  const runtimeTernaryWrite = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/type-question-complexity.ts",
      anchor: "fn:runtimeTernaries",
      lines: [13, 15],
      chose: "runtime ternaries remain complex",
      because: "type-only question complexity parity fixture",
      rejected: []
    })],
    "",
    root
  );

  return {
    lint: normalizeVolatile(lint),
    runtimeTernaryWrite: normalizeVolatile(runtimeTernaryWrite),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/type-question-complexity.ts.dlog",
      ".decisions/src/type-question-complexity.ts.dmap"
    ]))
  };
}));

results.push(await scenario("type-like-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "type-like-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/exports.ts"),
    `export const run = () => true;
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/default-type-like.ts"),
    `export default interface I { x: number }
export { I as NamedI };
export default enum E { A }
export { E as NamedE };
export default type T = string;
export { T as NamedT };
export default namespace N { export const x = 1; }
export { N as NamedN };
function after() {}
`,
    "utf8"
  );

  const writes = [
    ["export:I", [4, 4], "interface export"],
    ["export:T", [5, 5], "type export"],
    ["export:E", [6, 6], "enum export"],
    ["export:CE", [7, 7], "const enum export"],
    ["export:N", [8, 8], "namespace export"],
    ["fn:declared", [9, 9], "declared function"],
    ["export:declaredValue", [10, 10], "declared const export"],
    ["fn:Declared.run", [11, 11], "declared class method"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exports.ts",
        anchor,
        lines,
        chose,
        because: "type-like anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const defaultTypeLikeWrites = [
    ["export:default", [1, 1], "default interface export"],
    ["export:NamedI", [1, 1], "default interface renamed export"],
    ["export:NamedE", [3, 3], "default enum renamed export"],
    ["fn:after", [9, 9], "function after default type-like declarations"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/default-type-like.ts",
        anchor,
        lines,
        chose,
        because: "default type-like export parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidDefaultTypeLikeWrites = [
    "export:I",
    "export:E",
    "export:T",
    "export:N",
    "export:NamedT",
    "export:NamedN"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/default-type-like.ts",
        anchor,
        lines: [1, 9],
        chose: "default type-like parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/exports.ts"], "", root);
  const defaultTypeLikeWhy = run(runtime, ["why", "src/default-type-like.ts"], "", root);

  return {
    writes: writes.map(normalizeVolatile),
    defaultTypeLikeWrites: defaultTypeLikeWrites.map(normalizeVolatile),
    invalidDefaultTypeLikeWrites: invalidDefaultTypeLikeWrites.map(normalizeVolatile),
    why: normalizeVolatile(why),
    defaultTypeLikeWhy: normalizeVolatile(defaultTypeLikeWhy),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/exports.ts.dlog",
      ".decisions/src/exports.ts.dmap",
      ".decisions/src/default-type-like.ts.dlog",
      ".decisions/src/default-type-like.ts.dmap"
    ]))
  };
}));

results.push(await scenario("decorated-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "decorated-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/decorators.ts"),
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
`,
    "utf8"
  );

  const writes = [
    ["class:Service", [1, 9], "decorated class range"],
    ["fn:Service.run", [5, 8], "decorated method range"],
    ["export:Service", [1, 9], "decorated export range"],
    ["fn:Local.[compute]", [11, 12], "decorated computed method"],
    ["fn:Local.#run", [13, 14], "decorated private method"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/decorators.ts",
        anchor,
        lines,
        chose,
        because: "decorated anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/decorators.ts"], "", root);

  return {
    writes: writes.map(normalizeVolatile),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/decorators.ts.dlog", ".decisions/src/decorators.ts.dmap"]))
  };
}));

results.push(await scenario("anonymous-default-class-write", async (runtime) => {
  const root = await tempProject(runtime.name, "anonymous-default-class-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/anon-default.ts"),
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
`,
    "utf8"
  );

  const writes = [
    ["fn:open", [3, 4], "decorated anonymous method"],
    ["fn:items", [5, 5], "anonymous generator method"],
    ["fn:[compute]", [6, 6], "anonymous computed method"],
    ["fn:#secret", [7, 7], "anonymous private method"],
    ["export:default", [1, 10], "anonymous default class export"],
    ["fn:Later.m", [11, 11], "later named class method"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/anon-default.ts",
        anchor,
        lines,
        chose,
        because: "anonymous default class parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/anon-default.ts"], "", root);

  return {
    writes: writes.map(normalizeVolatile),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/anon-default.ts.dlog", ".decisions/src/anon-default.ts.dmap"]))
  };
}));

results.push(await scenario("tsx-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "tsx-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/view.tsx"),
    `export function View(props: { ok: boolean }) {
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
`,
    "utf8"
  );

  const writes = [
    ["fn:View", [1, 3], "tsx function component"],
    ["fn:Panel", [4, 6], "tsx arrow component"],
    ["fn:FragmentView", [7, 10], "tsx fragment component"],
    ["fn:Screen.render", [12, 14], "tsx render method"],
    ["fn:WithBlock", [16, 21], "tsx function with block"],
    ["block:if_props.ok_props.ready_props.admin", [17, 19], "tsx significant if block"],
    ["export:Panel", [4, 6], "tsx exported arrow component"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/view.tsx",
        anchor,
        lines,
        chose,
        because: "tsx anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/view.tsx"], "", root);

  return {
    writes: writes.map(normalizeVolatile),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/view.tsx.dlog", ".decisions/src/view.tsx.dmap"]))
  };
}));

results.push(await scenario("tsx-generic-ambiguity-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "tsx-generic-ambiguity-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const ambiguous = `function before() { return 0; }
const id = <T>(value: T) => value;
function after<T>(value: T) { return value; }
`;
  const validTsx = `function before() { return 0; }
const id = <T,>(value: T) => value;
function after<T>(value: T) { return value; }
`;
  await fs.writeFile(path.join(root, "src/ambig-generic.ts"), ambiguous, "utf8");
  await fs.writeFile(path.join(root, "src/ambig-generic.tsx"), ambiguous, "utf8");
  await fs.writeFile(path.join(root, "src/valid-generic.tsx"), validTsx, "utf8");

  const validWrites = [
    ["src/ambig-generic.ts", "fn:id", [2, 2], "ts generic arrow"],
    ["src/ambig-generic.ts", "fn:after", [3, 3], "ts function after generic"],
    ["src/ambig-generic.tsx", "fn:before", [1, 1], "tsx before ambiguous generic"],
    ["src/valid-generic.tsx", "fn:id", [2, 2], "tsx comma generic arrow"],
    ["src/valid-generic.tsx", "fn:after", [3, 3], "tsx function after valid generic"]
  ].map(([file, anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor,
        lines,
        chose,
        because: "tsx generic ambiguity parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidWrites = [
    ["src/ambig-generic.tsx", "fn:id"],
    ["src/ambig-generic.tsx", "fn:after"]
  ].map(([file, anchor]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor,
        lines: [2, 2],
        chose: "tsx parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );

  return {
    validWrites: validWrites.map(normalizeVolatile),
    invalidWrites: invalidWrites.map(normalizeVolatile),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/ambig-generic.ts.dlog",
      ".decisions/src/ambig-generic.ts.dmap",
      ".decisions/src/ambig-generic.tsx.dlog",
      ".decisions/src/ambig-generic.tsx.dmap",
      ".decisions/src/valid-generic.tsx.dlog",
      ".decisions/src/valid-generic.tsx.dmap"
    ]))
  };
}));

results.push(await scenario("malformed-jsx-recovery-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "malformed-jsx-recovery-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/malformed-jsx.tsx"),
    `function before() { return 0; }
const Broken = () => <div>
function after() { return 1; }
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/malformed-jsx-close.tsx"),
    `function before() { return 0; }
const Broken = () => <div>
</span>;
function after() { return 1; }
`,
    "utf8"
  );

  const validWrites = [
    ["src/malformed-jsx.tsx", "fn:before", [1, 1], "before unclosed jsx"],
    ["src/malformed-jsx.tsx", "fn:Broken", [2, 4], "unclosed jsx arrow"],
    ["src/malformed-jsx-close.tsx", "fn:Broken", [2, 3], "mismatched close jsx arrow"],
    ["src/malformed-jsx-close.tsx", "fn:after", [4, 4], "recovered after mismatched close"]
  ].map(([file, anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor,
        lines,
        chose,
        because: "malformed jsx recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidUnclosedAfter = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/malformed-jsx.tsx",
      anchor: "fn:after",
      lines: [3, 3],
      chose: "unclosed jsx parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );

  return {
    validWrites: validWrites.map(normalizeVolatile),
    invalidUnclosedAfter: normalizeVolatile(invalidUnclosedAfter),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/malformed-jsx.tsx.dlog",
      ".decisions/src/malformed-jsx.tsx.dmap",
      ".decisions/src/malformed-jsx-close.tsx.dlog",
      ".decisions/src/malformed-jsx-close.tsx.dmap"
    ]))
  };
}));

results.push(await scenario("import-regex-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "import-regex-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/imports-regex.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/import-equals.ts"),
    `import Alias = require("dep");
import Other = ns.Other;
import { "dash-name" as dashName, regular as regularName } from "./dep";
export { Alias, Other as ExportedOther, dashName, regularName };
export type { Alias as AliasType };
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/import-type-equals.ts"),
    `import Alias = require("dep");
import Other = ns.Other;
import type TypeAlias = require("types");
import type BareType;
import type ImportedDefault from "dep";
import type { ImportedNamed } from "dep";
export { Alias, Other as ExportedOther, TypeAlias, BareType as BareAgain, ImportedDefault as DefaultAgain, ImportedNamed as NamedAgain };
export type { Alias as AliasType, TypeAlias as TypeAliasType };
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/import-attrs.ts"),
    `const local = () => true;
export { local as remoteName } from "./dep" with { type: "json" };
import { value as importedValue } from "./dep" with { type: "json" };
export { importedValue };
export { local as renamed };
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/import-malformed-alias.ts"),
    `import Broken Local;
import Bare;
import FromLike from "dep";
import Comma, { Other } from "dep";
export { Broken as BrokenAgain, Bare as BareAgain, FromLike as FromAgain, Comma as CommaAgain };
function real() {}
`,
    "utf8"
  );

  const validWrites = [
    ["fn:real", [10, 12], "real function"],
    ["fn:local", [3, 3], "local arrow function"],
    ["export:LocalType", [1, 1], "local type export"],
    ["export:LocalAlias", [2, 2], "local alias export"],
    ["export:renamed", [3, 3], "local renamed export"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/imports-regex.ts",
        anchor,
        lines,
        chose,
        because: "import and regex anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const importEqualsWrites = [
    ["fn:real", [6, 6], "import equals fixture real function"],
    ["export:Alias", [1, 1], "import equals alias export"],
    ["export:ExportedOther", [2, 2], "qualified import equals renamed export"],
    ["export:AliasType", [1, 1], "import equals type-only renamed export"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-equals.ts",
        anchor,
        lines,
        chose,
        because: "import equals export alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const importTypeEqualsWrites = [
    ["fn:real", [9, 9], "import type equals fixture real function"],
    ["export:Alias", [1, 1], "import equals alias export"],
    ["export:ExportedOther", [2, 2], "qualified import equals renamed export"],
    ["export:TypeAlias", [3, 3], "type import equals alias export"],
    ["export:BareAgain", [4, 4], "bare malformed import type renamed export"],
    ["export:AliasType", [1, 1], "import equals type-only renamed export"],
    ["export:TypeAliasType", [3, 3], "type import equals type-only renamed export"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-type-equals.ts",
        anchor,
        lines,
        chose,
        because: "import type equals parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const importAttributeWrites = [
    ["fn:real", [6, 6], "import attributes fixture real function"],
    ["fn:local", [1, 1], "import attributes local arrow function"],
    ["export:renamed", [1, 1], "import attributes local renamed export"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-attrs.ts",
        anchor,
        lines,
        chose,
        because: "import attributes export alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const malformedImportAliasWrites = [
    ["fn:real", [6, 6], "malformed import alias real function"],
    ["export:BrokenAgain", [1, 1], "malformed import alias renamed export"],
    ["export:BareAgain", [2, 2], "bare malformed import alias renamed export"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-malformed-alias.ts",
        anchor,
        lines,
        chose,
        because: "malformed import alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidWrites = [
    "export:remoteName",
    "fn:fake",
    "block:if_fake_other_third"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/imports-regex.ts",
        anchor,
        lines: [1, 1],
        chose: "invalid parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidImportEqualsWrites = ["export:dashName", "export:regularName"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-equals.ts",
        anchor,
        lines: [1, 1],
        chose: "import binding parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidImportTypeEqualsWrites = ["export:DefaultAgain", "export:NamedAgain"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-type-equals.ts",
        anchor,
        lines: [1, 1],
        chose: "import type binding parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidImportAttributeWrites = ["export:remoteName", "export:importedValue"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-attrs.ts",
        anchor,
        lines: [1, 1],
        chose: "import attributes parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidMalformedImportAliasWrites = ["export:FromAgain", "export:CommaAgain"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/import-malformed-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "malformed import alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/imports-regex.ts"], "", root);
  const importEqualsWhy = run(runtime, ["why", "src/import-equals.ts"], "", root);
  const importTypeEqualsWhy = run(runtime, ["why", "src/import-type-equals.ts"], "", root);
  const importAttributesWhy = run(runtime, ["why", "src/import-attrs.ts"], "", root);
  const malformedImportAliasWhy = run(runtime, ["why", "src/import-malformed-alias.ts"], "", root);

  return {
    validWrites: validWrites.map(normalizeVolatile),
    importEqualsWrites: importEqualsWrites.map(normalizeVolatile),
    importTypeEqualsWrites: importTypeEqualsWrites.map(normalizeVolatile),
    importAttributeWrites: importAttributeWrites.map(normalizeVolatile),
    malformedImportAliasWrites: malformedImportAliasWrites.map(normalizeVolatile),
    invalidWrites: invalidWrites.map(normalizeVolatile),
    invalidImportEqualsWrites: invalidImportEqualsWrites.map(normalizeVolatile),
    invalidImportTypeEqualsWrites: invalidImportTypeEqualsWrites.map(normalizeVolatile),
    invalidImportAttributeWrites: invalidImportAttributeWrites.map(normalizeVolatile),
    invalidMalformedImportAliasWrites: invalidMalformedImportAliasWrites.map(normalizeVolatile),
    why: normalizeVolatile(why),
    importEqualsWhy: normalizeVolatile(importEqualsWhy),
    importTypeEqualsWhy: normalizeVolatile(importTypeEqualsWhy),
    importAttributesWhy: normalizeVolatile(importAttributesWhy),
    malformedImportAliasWhy: normalizeVolatile(malformedImportAliasWhy),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/imports-regex.ts.dlog",
      ".decisions/src/imports-regex.ts.dmap",
      ".decisions/src/import-equals.ts.dlog",
      ".decisions/src/import-equals.ts.dmap",
      ".decisions/src/import-type-equals.ts.dlog",
      ".decisions/src/import-type-equals.ts.dmap",
      ".decisions/src/import-attrs.ts.dlog",
      ".decisions/src/import-attrs.ts.dmap",
      ".decisions/src/import-malformed-alias.ts.dlog",
      ".decisions/src/import-malformed-alias.ts.dmap"
    ]))
  };
}));

results.push(await scenario("export-variant-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "export-variant-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/export-variants.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/export-import-type.ts"),
    `export import type Alias = require("types");
export import type Bare;
export import type Qualified = NS.Sub;
export import type Multi = require(
  "dep"
);
export import type = require("weird");
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-combined.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-merge-exported-backward.ts"),
    `namespace N {
  export class Box { open() {} }
}
namespace N {
  export { Box as Crate };
}
export = N;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-merge-exported-forward.ts"),
    `namespace N {
  export { Box as Crate };
}
namespace N {
  export class Box { open() {} }
}
export = N;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-merge-mixed.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-alias-chain.ts"),
    `namespace N {
  const value = 1;
  export { value as first };
  export { first as second };
}
export = N;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-merge-alias-forward.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-alias-direct-and-chain.ts"),
    `namespace N {
  export const value = 1;
  export { value as first };
  export { value as second, first as third };
}
export = N;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-type-alias-forward.ts"),
    `namespace N {
  export type { value as PublicValue };
}
namespace N {
  export const value = 1;
}
export = N;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient.ts"),
    `declare namespace Ambient {
  const value: number;
  function run(): void;
  class Box { open(): void; }
  interface Face { y: number }
  type Shape = { x: number };
}
export = Ambient;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-listed.ts"),
    `declare namespace Listed {
  function hiddenRun(): void;
  const hidden: number;
  export { hidden as Hidden };
  export function explicit(): void;
}
export = Listed;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-nested.ts"),
    `declare namespace Local {
  namespace Inner { export function deep(): void; }
  namespace Plain { function hidden(): void; }
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-nested-listed.ts"),
    `declare namespace Local {
  namespace Inner {
    const hidden: number;
    export { hidden as Hidden };
  }
  function run(): void;
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-module.ts"),
    `declare module "pkg" {
  export function ghost(): void;
  export const ghostValue: number;
}
declare module Local {
  export function run(): void;
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-merge-backward.ts"),
    `declare namespace Local {
  const hidden: number;
}
declare namespace Local {
  export { hidden as Hidden };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-merge-forward.ts"),
    `declare namespace Local {
  export { hidden as Hidden };
}
declare namespace Local {
  const hidden: number;
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-ambient-type-alias-forward.ts"),
    `declare namespace Local {
  export type { Shape as PublicShape };
}
declare namespace Local {
  interface Shape { x: number }
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-multivar.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-typed-initializers.ts"),
    `namespace Local {
  const objectTyped: { a: number, b: number } = () => true;
  const tupleTyped: [number, string] = function hiddenTuple() { return [1, "x"]; };
  export { objectTyped, tupleTyped };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-destructure.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-binding-defaults.ts"),
    `namespace Local {
  const { [dynamicKey]: computedValue = fallback(a, b), plain = make(c, d), nested: { inner = other(e, f) } = defaults } = source;
  const [head = pair(g, h), , tail = makeTail(i, j)] = values;
  export const { directComputed = call(k, l), directPlain = make(m, n) } = source;
  export const [directHead = pair(o, p), directTail = makeTail(q, r)] = values;
  export { computedValue, plain, inner, head, tail };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-quoted-alias.ts"),
    `namespace Local {
  const value = 1;
  const keep = 2;
  function run() {}
  type Shape = { x: number };
  export { keep as kept, value as "dash-name", run as "call-run", type Shape as "shape-type" };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-missing-alias.ts"),
    `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as, other as valid };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-numeric-alias.ts"),
    `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as 123, other as valid };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-dot-alias.ts"),
    `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as ., other as valid };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/top-punctuation-alias.ts"),
    `const q = 1;
const c = 2;
const p = 3;
const b = 4;
const other = 5;
export { q as ?, c as :, p as ), b as ], other as valid };
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-punctuation-alias.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/top-private-name-alias.ts"),
    `const keep = 1;
const other = 2;
export { keep as #foo, other as Valid };
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/ns-private-name-alias.ts"),
    `namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as #foo, other as Valid };
}
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/export-import-malformed.ts"),
    `export import Broken Local;
export import Bare;
export import FromLike from "dep";
export import Comma, Other = require("dep");
export { Broken as BrokenAgain, Bare as BareAgain, FromLike as FromAgain, Comma as CommaAgain };
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/enum-export-assignment.ts"),
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
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/exported-enum-assignment.ts"),
    `export enum Local { A }
export = Local;
function real() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/export-assignment-order.ts"),
    `export const other = 1;
namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/exported-enum-merge-assignment.ts"),
    `export enum Local { A }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/exported-namespace-merge-assignment.ts"),
    `export namespace Local { export const self = 1; }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/export-namespace-only-assignment.ts"),
    `export namespace Local { export const self = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/plain-enum-export-namespace-assignment.ts"),
    `enum Local { A }
export namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/exported-function-namespace-assignment.ts"),
    `export function Local() { return 1; }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/exported-class-namespace-assignment.ts"),
    `export class Local { method() {} }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/default-class-namespace-assignment.ts"),
    `export default class Local { method() {} }
namespace Local { export const value = 1; }
export = Local;
function after() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/top-template-alias.ts"),
    `const keep = 1;
const other = 2;
export { keep as \`dash\`, other as valid };
function real() {}
`,
    "utf8"
  );

  const validWrites = [
    ["export:run", [3, 3], "namespace function export assignment"],
    ["export:value", [2, 2], "namespace value export assignment"],
    ["export:Box", [4, 4], "namespace class export assignment"],
    ["export:ExportedAlias", [6, 6], "export import alias"],
    ["export:Other", [7, 7], "export import qualified alias"],
    ["export:AliasAgain", [6, 6], "export import renamed alias"],
    ["export:OtherType", [7, 7], "export import type-only renamed alias"],
    ["export:Req", [10, 12], "multiline require export import"],
    ["export:ReqAgain", [10, 12], "multiline require export import renamed alias"],
    ["export:ReqType", [10, 12], "multiline require export import type-only alias"],
    ["export:Multi", [15, 16], "multiline qualified export import"],
    ["export:MultiAgain", [15, 16], "multiline qualified export import renamed alias"],
    ["export:Duplicate", [18, 18], "first duplicate export import"],
    ["export:DuplicateAgain", [18, 18], "duplicate export import renamed alias"],
    ["fn:real", [23, 23], "top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-variants.ts",
        anchor,
        lines,
        chose,
        because: "export variant anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportImportTypeWrites = [
    ["export:Alias", [1, 1], "export import type require alias"],
    ["export:Bare", [2, 2], "bare export import type alias"],
    ["export:Qualified", [3, 3], "qualified export import type alias"],
    ["export:Multi", [4, 6], "multiline export import type require alias"],
    ["export:type", [7, 7], "malformed export import type keyword alias"],
    ["fn:real", [8, 8], "export import type fixture real function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-import-type.ts",
        anchor,
        lines,
        chose,
        because: "export import type parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceAliasWrites = [
    ["export:Inner", [1, 3], "dotted namespace export assignment"],
    ["export:run", [8, 8], "merged namespace function export assignment"],
    ["export:hidden", [5, 5], "namespace local export alias"],
    ["export:revealed", [6, 6], "namespace renamed function export alias"],
    ["block:if_a_b_c", [6, 6], "namespace alias source block"],
    ["fn:real", [11, 11], "namespace fixture top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-combined.ts",
        anchor,
        lines,
        chose,
        because: "dotted namespace export alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceWrites = ["export:deep", "fn:secret"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-combined.ts",
        anchor,
        lines: [1, 1],
        chose: "dotted namespace parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const mergedNamespaceBackwardWrites = [
    ["export:Box", [2, 2], "merged namespace direct class before alias"],
    ["export:Crate", [2, 2], "merged namespace backward class alias"],
    ["fn:after", [8, 8], "merged namespace backward top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-merge-exported-backward.ts",
        anchor,
        lines,
        chose,
        because: "merged namespace exported alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const mergedNamespaceForwardWrites = [
    ["export:Crate", [5, 5], "merged namespace forward class alias"],
    ["export:Box", [5, 5], "merged namespace direct class after alias"],
    ["fn:after", [8, 8], "merged namespace forward top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-merge-exported-forward.ts",
        anchor,
        lines,
        chose,
        because: "merged namespace forward exported alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const mergedNamespaceMixedWrites = [
    ["export:Box", [3, 3], "merged namespace mixed direct class export"],
    ["export:value", [4, 4], "merged namespace mixed direct value export"],
    ["export:Crate", [3, 3], "merged namespace mixed class alias"],
    ["export:aliasValue", [4, 4], "merged namespace mixed value alias"],
    ["fn:after", [10, 10], "merged namespace mixed top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-merge-mixed.ts",
        anchor,
        lines,
        chose,
        because: "merged namespace mixed exported alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidMergedNamespaceMixedWrites = ["export:Seen", "export:Hidden"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-merge-mixed.ts",
        anchor,
        lines: [1, 10],
        chose: "merged namespace local parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceAliasChainWrites = [
    ["export:first", [2, 2], "namespace alias chain first alias"],
    ["export:second", [2, 2], "namespace alias chain second alias"],
    ["fn:after", [7, 7], "namespace alias chain top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-alias-chain.ts",
        anchor,
        lines,
        chose,
        because: "namespace alias-to-alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceForwardAliasChainWrites = [
    ["export:second", [8, 8], "namespace forward alias chain second alias"],
    ["export:first", [8, 8], "namespace forward alias chain first alias"],
    ["export:value", [8, 8], "namespace forward alias chain direct export"],
    ["fn:after", [11, 11], "namespace forward alias chain top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-merge-alias-forward.ts",
        anchor,
        lines,
        chose,
        because: "namespace forward alias-to-alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceDirectAliasChainWrites = [
    ["export:value", [2, 2], "namespace direct alias chain value export"],
    ["export:first", [2, 2], "namespace direct alias chain first alias"],
    ["export:second", [2, 2], "namespace direct alias chain second alias"],
    ["export:third", [2, 2], "namespace direct alias chain third alias"],
    ["fn:after", [7, 7], "namespace direct alias chain top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-alias-direct-and-chain.ts",
        anchor,
        lines,
        chose,
        because: "namespace direct alias-to-alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceTypeAliasForwardWrites = [
    ["export:PublicValue", [5, 5], "namespace type-only forward alias export"],
    ["export:value", [5, 5], "namespace type-only forward direct export"],
    ["fn:after", [8, 8], "namespace type-only forward top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-type-alias-forward.ts",
        anchor,
        lines,
        chose,
        because: "namespace type-only forward alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceTypeAliasForwardWrites = ["export:type"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-type-alias-forward.ts",
        anchor,
        lines: [2, 2],
        chose: "namespace type-only forward alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientWrites = [
    ["export:run", [3, 3], "ambient namespace implicit function export"],
    ["export:value", [2, 2], "ambient namespace implicit value export"],
    ["export:Box", [4, 4], "ambient namespace implicit class export"],
    ["export:Face", [5, 5], "ambient namespace implicit interface export"],
    ["export:Shape", [6, 6], "ambient namespace implicit type export"],
    ["fn:real", [9, 9], "ambient namespace top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient.ts",
        anchor,
        lines,
        chose,
        because: "ambient namespace export assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientListedWrites = [
    ["export:explicit", [5, 5], "ambient namespace explicit export with export list"],
    ["export:Hidden", [3, 3], "ambient namespace listed renamed export"],
    ["fn:real", [8, 8], "ambient namespace listed top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-listed.ts",
        anchor,
        lines,
        chose,
        because: "ambient namespace export list parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidAmbientListedWrites = ["export:hiddenRun", "export:hidden"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-listed.ts",
        anchor,
        lines: [1, 1],
        chose: "ambient namespace parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientNestedWrites = [
    ["export:Inner", [2, 2], "ambient nested namespace export"],
    ["export:Plain", [3, 3], "ambient plain nested namespace export"],
    ["fn:real", [6, 6], "ambient nested top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-nested.ts",
        anchor,
        lines,
        chose,
        because: "ambient nested namespace export assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidAmbientNestedWrites = ["export:deep", "export:hidden"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-nested.ts",
        anchor,
        lines: [1, 1],
        chose: "ambient nested namespace parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientNestedListedWrites = [
    ["export:run", [6, 6], "ambient nested listed function export"],
    ["export:Inner", [2, 5], "ambient nested listed namespace export"],
    ["fn:real", [9, 9], "ambient nested listed top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-nested-listed.ts",
        anchor,
        lines,
        chose,
        because: "ambient nested namespace export-list parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidAmbientNestedListedWrites = ["export:Hidden"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-nested-listed.ts",
        anchor,
        lines: [1, 1],
        chose: "ambient nested listed parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientModuleWrites = [
    ["export:run", [6, 6], "ambient identifier module export assignment"],
    ["fn:real", [9, 9], "ambient module top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-module.ts",
        anchor,
        lines,
        chose,
        because: "ambient module export assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidAmbientModuleWrites = ["export:ghost", "export:ghostValue"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-module.ts",
        anchor,
        lines: [1, 1],
        chose: "ambient string module parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientMergeBackwardWrites = [
    ["export:hidden", [2, 2], "ambient merged implicit export before alias"],
    ["export:Hidden", [2, 2], "ambient merged backward alias export"],
    ["fn:real", [8, 8], "ambient merged backward top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-merge-backward.ts",
        anchor,
        lines,
        chose,
        because: "ambient namespace merged alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientMergeForwardWrites = [
    ["export:Hidden", [5, 5], "ambient merged forward alias export"],
    ["export:hidden", [5, 5], "ambient merged implicit export after alias"],
    ["fn:real", [8, 8], "ambient merged forward top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-merge-forward.ts",
        anchor,
        lines,
        chose,
        because: "ambient namespace forward merged alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const ambientTypeAliasForwardWrites = [
    ["export:PublicShape", [5, 5], "ambient type-only forward alias export"],
    ["export:Shape", [5, 5], "ambient type-only forward direct export"],
    ["fn:real", [8, 8], "ambient type-only forward top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-type-alias-forward.ts",
        anchor,
        lines,
        chose,
        because: "ambient type-only forward alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidAmbientTypeAliasForwardWrites = ["export:type"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-ambient-type-alias-forward.ts",
        anchor,
        lines: [2, 2],
        chose: "ambient type-only forward alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceMultiVarWrites = [
    ["export:first", [2, 2], "namespace first variable alias"],
    ["export:renamedSecond", [2, 2], "namespace second variable renamed alias"],
    ["export:count", [3, 3], "namespace first let alias"],
    ["export:renamedTotal", [3, 3], "namespace second let renamed alias"],
    ["export:Shape", [4, 4], "namespace type-only alias"],
    ["export:RenamedFace", [5, 5], "namespace interface renamed alias"],
    ["export:direct", [7, 7], "namespace direct first variable export"],
    ["export:other", [7, 7], "namespace direct second variable export"],
    ["export:aliasDirect", [7, 7], "namespace direct first variable renamed alias"],
    ["export:aliasOther", [7, 7], "namespace direct second variable renamed alias"],
    ["fn:real", [11, 11], "namespace multivar top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-multivar.ts",
        anchor,
        lines,
        chose,
        because: "namespace multivar export alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceMultiVarWrites = ["export:second", "export:total", "export:Face"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-multivar.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace multivar parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceTypedVariableWrites = [
    ["export:objectTyped", [2, 2], "namespace object type annotation export"],
    ["export:tupleTyped", [3, 3], "namespace tuple type annotation export"],
    ["fn:real", [7, 7], "namespace typed variable top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-typed-initializers.ts",
        anchor,
        lines,
        chose,
        because: "namespace typed variable initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceTypedVariableWrites = ["export:b", "export:string", "fn:hiddenTuple"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-typed-initializers.ts",
        anchor,
        lines: [1, 7],
        chose: "namespace typed variable initializer parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceDestructureWrites = [
    ["export:direct", [4, 4], "namespace direct object destructuring export"],
    ["export:directAlias", [4, 4], "namespace direct object destructuring alias"],
    ["export:directHead", [5, 5], "namespace direct array destructuring export"],
    ["export:directTail", [5, 5], "namespace direct array destructuring second export"],
    ["export:first", [2, 2], "namespace object destructuring alias source"],
    ["export:exportedSecond", [2, 2], "namespace object destructuring renamed alias"],
    ["export:head", [3, 3], "namespace array destructuring alias source"],
    ["export:exportedTail", [3, 3], "namespace array destructuring renamed alias"],
    ["export:inner", [6, 6], "namespace nested destructuring alias"],
    ["export:rest", [6, 6], "namespace object rest destructuring alias"],
    ["export:nestedHead", [7, 7], "namespace nested array destructuring alias"],
    ["export:others", [7, 7], "namespace array rest destructuring alias"],
    ["fn:real", [11, 11], "namespace destructuring top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-destructure.ts",
        anchor,
        lines,
        chose,
        because: "namespace destructuring export alias parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceDestructureWrites = ["export:renamedLocal", "export:tail", "export:alias"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-destructure.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace destructuring parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceBindingDefaultWrites = [
    ["export:directComputed", [4, 4], "namespace direct computed default binding"],
    ["export:directPlain", [4, 4], "namespace direct plain default binding"],
    ["export:directHead", [5, 5], "namespace direct array default binding"],
    ["export:directTail", [5, 5], "namespace direct array second default binding"],
    ["export:computedValue", [2, 2], "namespace computed property default alias"],
    ["export:plain", [2, 2], "namespace object default alias"],
    ["export:inner", [2, 2], "namespace nested object default alias"],
    ["export:head", [3, 3], "namespace array default alias"],
    ["export:tail", [3, 3], "namespace sparse array default alias"],
    ["fn:real", [9, 9], "namespace binding defaults top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-binding-defaults.ts",
        anchor,
        lines,
        chose,
        because: "namespace binding default initializer parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceBindingDefaultWrites = [
    "export:b",
    "export:d",
    "export:f",
    "export:h",
    "export:j",
    "export:l",
    "export:n",
    "export:p",
    "export:r"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-binding-defaults.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace binding default parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceQuotedAliasWrites = [
    ["fn:real", [9, 9], "namespace quoted alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-quoted-alias.ts",
        anchor,
        lines,
        chose,
        because: "namespace quoted export alias suppression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceQuotedAliasWrites = [
    "export:value",
    "export:keep",
    "export:kept",
    "export:run",
    "export:Shape",
    "export:dash-name",
    "export:call-run",
    "export:shape-type"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-quoted-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace quoted alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceMissingAliasWrites = [
    ["export:", [2, 2], "namespace missing alias target empty export"],
    ["export:valid", [3, 3], "namespace valid alias after missing target"],
    ["fn:real", [7, 7], "namespace missing alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-missing-alias.ts",
        anchor,
        lines,
        chose,
        because: "namespace missing export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceMissingAliasWrites = ["export:keep", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-missing-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace missing alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceNumericAliasWrites = [
    ["export:", [2, 2], "namespace numeric alias target empty export"],
    ["fn:real", [7, 7], "namespace numeric alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-numeric-alias.ts",
        anchor,
        lines,
        chose,
        because: "namespace numeric export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceNumericAliasWrites = ["export:valid", "export:keep", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-numeric-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace numeric alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespaceDotAliasWrites = [
    ["export:", [2, 2], "namespace dot alias target empty export"],
    ["export:valid", [3, 3], "namespace dot alias target recovered valid export"],
    ["fn:real", [7, 7], "namespace dot alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-dot-alias.ts",
        anchor,
        lines,
        chose,
        because: "namespace dot export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespaceDotAliasWrites = ["export:keep", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-dot-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace dot alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const topPunctuationAliasWrites = [
    ["export:", [1, 1], "top punctuation alias target empty export"],
    ["export:valid", [5, 5], "top punctuation alias target recovered valid export"],
    ["fn:real", [7, 7], "top punctuation alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/top-punctuation-alias.ts",
        anchor,
        lines,
        chose,
        because: "top punctuation export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidTopPunctuationAliasWrites = ["export:q", "export:c", "export:p", "export:b", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/top-punctuation-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "top punctuation alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespacePunctuationAliasWrites = [
    ["export:", [2, 2], "namespace punctuation alias target empty export"],
    ["export:valid", [6, 6], "namespace punctuation alias target recovered valid export"],
    ["fn:real", [10, 10], "namespace punctuation alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-punctuation-alias.ts",
        anchor,
        lines,
        chose,
        because: "namespace punctuation export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespacePunctuationAliasWrites = ["export:q", "export:c", "export:p", "export:b", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-punctuation-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace punctuation alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const topPrivateNameAliasWrites = [
    ["export:#foo", [1, 1], "top private-name alias target export"],
    ["export:Valid", [2, 2], "top private-name alias recovered valid export"],
    ["fn:real", [4, 4], "top private-name alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/top-private-name-alias.ts",
        anchor,
        lines,
        chose,
        because: "top private-name export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidTopPrivateNameAliasWrites = ["export:keep", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/top-private-name-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "top private-name alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const namespacePrivateNameAliasWrites = [
    ["export:#foo", [2, 2], "namespace private-name alias target export"],
    ["export:Valid", [3, 3], "namespace private-name alias recovered valid export"],
    ["fn:real", [7, 7], "namespace private-name alias top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-private-name-alias.ts",
        anchor,
        lines,
        chose,
        because: "namespace private-name export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNamespacePrivateNameAliasWrites = ["export:keep", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/ns-private-name-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "namespace private-name alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const malformedExportImportWrites = [
    ["export:Broken", [1, 1], "malformed export import alias"],
    ["export:Bare", [2, 2], "bare malformed export import alias"],
    ["export:BrokenAgain", [1, 1], "malformed export import renamed alias"],
    ["export:BareAgain", [2, 2], "bare malformed export import renamed alias"],
    ["fn:real", [6, 6], "malformed export import top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-import-malformed.ts",
        anchor,
        lines,
        chose,
        because: "malformed export import parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidMalformedExportImportWrites = [
    "export:FromLike",
    "export:Comma",
    "export:FromAgain",
    "export:CommaAgain"
  ].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-import-malformed.ts",
        anchor,
        lines: [1, 1],
        chose: "malformed export import parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const enumExportAssignmentWrites = [
    ["export:A", [2, 3], "enum member multiline initializer export assignment"],
    ["export:B", [4, 4], "enum member export assignment"],
    ["export:dash-name", [5, 5], "string enum member export assignment"],
    ["export:1", [6, 6], "numeric enum member export assignment"],
    ["export:run", [9, 9], "merged namespace export after enum assignment"],
    ["fn:real", [11, 11], "enum export assignment top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/enum-export-assignment.ts",
        anchor,
        lines,
        chose,
        because: "enum export assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidEnumExportAssignmentWrites = ["export:key"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/enum-export-assignment.ts",
        anchor,
        lines: [7, 7],
        chose: "computed enum member parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportedEnumAssignmentWrites = [
    ["export:A", [1, 1], "exported enum member through export assignment"],
    ["export:Local", [1, 1], "direct exported enum declaration"],
    ["fn:real", [3, 3], "exported enum assignment top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-enum-assignment.ts",
        anchor,
        lines,
        chose,
        because: "exported enum assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportAssignmentOrderWrites = [
    ["export:value", [2, 2], "export assignment namespace member before direct export"],
    ["export:other", [1, 1], "direct export after export assignment member"],
    ["fn:after", [4, 4], "export assignment order top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-assignment-order.ts",
        anchor,
        lines,
        chose,
        because: "export assignment ordering parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportedEnumMergeAssignmentWrites = [
    ["export:value", [2, 2], "exported enum merged namespace value export"],
    ["export:Local", [1, 1], "exported enum merged direct declaration"],
    ["fn:after", [4, 4], "exported enum merge top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-enum-merge-assignment.ts",
        anchor,
        lines,
        chose,
        because: "exported enum merge export assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidExportedEnumMergeAssignmentWrites = ["export:A"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-enum-merge-assignment.ts",
        anchor,
        lines: [1, 1],
        chose: "exported enum merge parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportedNamespaceMergeAssignmentWrites = [
    ["export:value", [2, 2], "exported namespace merged value export"],
    ["export:Local", [1, 1], "exported namespace direct declaration"],
    ["fn:after", [4, 4], "exported namespace merge top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-namespace-merge-assignment.ts",
        anchor,
        lines,
        chose,
        because: "exported namespace merge export assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidExportedNamespaceMergeAssignmentWrites = ["export:self"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-namespace-merge-assignment.ts",
        anchor,
        lines: [1, 1],
        chose: "exported namespace merge parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportNamespaceOnlyAssignmentWrites = [
    ["export:self", [1, 1], "export namespace member without merge suppression"],
    ["export:Local", [1, 1], "export namespace direct declaration"],
    ["fn:after", [3, 3], "export namespace only top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-namespace-only-assignment.ts",
        anchor,
        lines,
        chose,
        because: "export namespace only assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const plainEnumExportNamespaceAssignmentWrites = [
    ["export:A", [1, 1], "plain enum member with exported namespace merge"],
    ["export:Local", [2, 2], "export namespace direct declaration after plain enum"],
    ["fn:after", [4, 4], "plain enum export namespace top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/plain-enum-export-namespace-assignment.ts",
        anchor,
        lines,
        chose,
        because: "plain enum export namespace assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidPlainEnumExportNamespaceAssignmentWrites = ["export:value"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/plain-enum-export-namespace-assignment.ts",
        anchor,
        lines: [2, 2],
        chose: "plain enum export namespace parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportedFunctionNamespaceAssignmentWrites = [
    ["fn:Local", [1, 1], "exported function declaration"],
    ["fn:after", [4, 4], "exported function namespace top-level function"],
    ["export:value", [2, 2], "exported function namespace value export"],
    ["export:Local", [1, 1], "exported function direct declaration"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-function-namespace-assignment.ts",
        anchor,
        lines,
        chose,
        because: "exported function namespace assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidExportedFunctionNamespaceAssignmentWrites = ["export:default"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-function-namespace-assignment.ts",
        anchor,
        lines: [1, 1],
        chose: "exported function namespace parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const exportedClassNamespaceAssignmentWrites = [
    ["class:Local", [1, 1], "exported class declaration"],
    ["fn:Local.method", [1, 1], "exported class namespace method"],
    ["fn:after", [4, 4], "exported class namespace top-level function"],
    ["export:value", [2, 2], "exported class namespace value export"],
    ["export:Local", [1, 1], "exported class direct declaration"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-class-namespace-assignment.ts",
        anchor,
        lines,
        chose,
        because: "exported class namespace assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidExportedClassNamespaceAssignmentWrites = ["export:default"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/exported-class-namespace-assignment.ts",
        anchor,
        lines: [1, 1],
        chose: "exported class namespace parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const defaultClassNamespaceAssignmentWrites = [
    ["export:value", [2, 2], "default class namespace value export"],
    ["export:default", [1, 1], "default class export after namespace member"],
    ["fn:after", [4, 4], "default class namespace top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/default-class-namespace-assignment.ts",
        anchor,
        lines,
        chose,
        because: "default class namespace assignment parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const templateAliasWrites = [
    ["export:", [1, 1], "template alias target empty export"],
    ["fn:real", [4, 4], "template alias target top-level function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/top-template-alias.ts",
        anchor,
        lines,
        chose,
        because: "template export alias target parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidTemplateAliasWrites = ["export:valid", "export:keep", "export:other"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/top-template-alias.ts",
        anchor,
        lines: [1, 1],
        chose: "template alias parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidWrites = ["export:Archiva", "fn:Local.run"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/export-variants.ts",
        anchor,
        lines: [1, 1],
        chose: "export variant parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/export-variants.ts"], "", root);
  const exportImportTypeWhy = run(runtime, ["why", "src/export-import-type.ts"], "", root);
  const namespaceWhy = run(runtime, ["why", "src/ns-combined.ts"], "", root);
  const mergedNamespaceBackwardWhy = run(runtime, ["why", "src/ns-merge-exported-backward.ts"], "", root);
  const mergedNamespaceForwardWhy = run(runtime, ["why", "src/ns-merge-exported-forward.ts"], "", root);
  const mergedNamespaceMixedWhy = run(runtime, ["why", "src/ns-merge-mixed.ts"], "", root);
  const namespaceAliasChainWhy = run(runtime, ["why", "src/ns-alias-chain.ts"], "", root);
  const namespaceForwardAliasChainWhy = run(runtime, ["why", "src/ns-merge-alias-forward.ts"], "", root);
  const namespaceDirectAliasChainWhy = run(runtime, ["why", "src/ns-alias-direct-and-chain.ts"], "", root);
  const namespaceTypeAliasForwardWhy = run(runtime, ["why", "src/ns-type-alias-forward.ts"], "", root);
  const ambientWhy = run(runtime, ["why", "src/ns-ambient.ts"], "", root);
  const ambientListedWhy = run(runtime, ["why", "src/ns-ambient-listed.ts"], "", root);
  const ambientNestedWhy = run(runtime, ["why", "src/ns-ambient-nested.ts"], "", root);
  const ambientNestedListedWhy = run(runtime, ["why", "src/ns-ambient-nested-listed.ts"], "", root);
  const ambientModuleWhy = run(runtime, ["why", "src/ns-ambient-module.ts"], "", root);
  const ambientMergeBackwardWhy = run(runtime, ["why", "src/ns-ambient-merge-backward.ts"], "", root);
  const ambientMergeForwardWhy = run(runtime, ["why", "src/ns-ambient-merge-forward.ts"], "", root);
  const ambientTypeAliasForwardWhy = run(runtime, ["why", "src/ns-ambient-type-alias-forward.ts"], "", root);
  const namespaceMultiVarWhy = run(runtime, ["why", "src/ns-multivar.ts"], "", root);
  const namespaceTypedVariableWhy = run(runtime, ["why", "src/ns-typed-initializers.ts"], "", root);
  const namespaceDestructureWhy = run(runtime, ["why", "src/ns-destructure.ts"], "", root);
  const namespaceBindingDefaultWhy = run(runtime, ["why", "src/ns-binding-defaults.ts"], "", root);
  const namespaceQuotedAliasWhy = run(runtime, ["why", "src/ns-quoted-alias.ts"], "", root);
  const namespaceMissingAliasWhy = run(runtime, ["why", "src/ns-missing-alias.ts"], "", root);
  const namespaceNumericAliasWhy = run(runtime, ["why", "src/ns-numeric-alias.ts"], "", root);
  const namespaceDotAliasWhy = run(runtime, ["why", "src/ns-dot-alias.ts"], "", root);
  const topPunctuationAliasWhy = run(runtime, ["why", "src/top-punctuation-alias.ts"], "", root);
  const namespacePunctuationAliasWhy = run(runtime, ["why", "src/ns-punctuation-alias.ts"], "", root);
  const topPrivateNameAliasWhy = run(runtime, ["why", "src/top-private-name-alias.ts"], "", root);
  const namespacePrivateNameAliasWhy = run(runtime, ["why", "src/ns-private-name-alias.ts"], "", root);
  const malformedExportImportWhy = run(runtime, ["why", "src/export-import-malformed.ts"], "", root);
  const enumExportAssignmentWhy = run(runtime, ["why", "src/enum-export-assignment.ts"], "", root);
  const exportedEnumAssignmentWhy = run(runtime, ["why", "src/exported-enum-assignment.ts"], "", root);
  const exportAssignmentOrderWhy = run(runtime, ["why", "src/export-assignment-order.ts"], "", root);
  const exportedEnumMergeAssignmentWhy = run(runtime, ["why", "src/exported-enum-merge-assignment.ts"], "", root);
  const exportedNamespaceMergeAssignmentWhy = run(runtime, ["why", "src/exported-namespace-merge-assignment.ts"], "", root);
  const exportNamespaceOnlyAssignmentWhy = run(runtime, ["why", "src/export-namespace-only-assignment.ts"], "", root);
  const plainEnumExportNamespaceAssignmentWhy = run(runtime, ["why", "src/plain-enum-export-namespace-assignment.ts"], "", root);
  const exportedFunctionNamespaceAssignmentWhy = run(runtime, ["why", "src/exported-function-namespace-assignment.ts"], "", root);
  const exportedClassNamespaceAssignmentWhy = run(runtime, ["why", "src/exported-class-namespace-assignment.ts"], "", root);
  const defaultClassNamespaceAssignmentWhy = run(runtime, ["why", "src/default-class-namespace-assignment.ts"], "", root);
  const templateAliasWhy = run(runtime, ["why", "src/top-template-alias.ts"], "", root);

  return {
    validWrites: validWrites.map(normalizeVolatile),
    exportImportTypeWrites: exportImportTypeWrites.map(normalizeVolatile),
    namespaceAliasWrites: namespaceAliasWrites.map(normalizeVolatile),
    mergedNamespaceBackwardWrites: mergedNamespaceBackwardWrites.map(normalizeVolatile),
    mergedNamespaceForwardWrites: mergedNamespaceForwardWrites.map(normalizeVolatile),
    mergedNamespaceMixedWrites: mergedNamespaceMixedWrites.map(normalizeVolatile),
    namespaceAliasChainWrites: namespaceAliasChainWrites.map(normalizeVolatile),
    namespaceForwardAliasChainWrites: namespaceForwardAliasChainWrites.map(normalizeVolatile),
    namespaceDirectAliasChainWrites: namespaceDirectAliasChainWrites.map(normalizeVolatile),
    namespaceTypeAliasForwardWrites: namespaceTypeAliasForwardWrites.map(normalizeVolatile),
    ambientWrites: ambientWrites.map(normalizeVolatile),
    ambientListedWrites: ambientListedWrites.map(normalizeVolatile),
    ambientNestedWrites: ambientNestedWrites.map(normalizeVolatile),
    ambientNestedListedWrites: ambientNestedListedWrites.map(normalizeVolatile),
    ambientModuleWrites: ambientModuleWrites.map(normalizeVolatile),
    ambientMergeBackwardWrites: ambientMergeBackwardWrites.map(normalizeVolatile),
    ambientMergeForwardWrites: ambientMergeForwardWrites.map(normalizeVolatile),
    ambientTypeAliasForwardWrites: ambientTypeAliasForwardWrites.map(normalizeVolatile),
    namespaceMultiVarWrites: namespaceMultiVarWrites.map(normalizeVolatile),
    namespaceTypedVariableWrites: namespaceTypedVariableWrites.map(normalizeVolatile),
    namespaceDestructureWrites: namespaceDestructureWrites.map(normalizeVolatile),
    namespaceBindingDefaultWrites: namespaceBindingDefaultWrites.map(normalizeVolatile),
    namespaceQuotedAliasWrites: namespaceQuotedAliasWrites.map(normalizeVolatile),
    namespaceMissingAliasWrites: namespaceMissingAliasWrites.map(normalizeVolatile),
    namespaceNumericAliasWrites: namespaceNumericAliasWrites.map(normalizeVolatile),
    namespaceDotAliasWrites: namespaceDotAliasWrites.map(normalizeVolatile),
    topPunctuationAliasWrites: topPunctuationAliasWrites.map(normalizeVolatile),
    namespacePunctuationAliasWrites: namespacePunctuationAliasWrites.map(normalizeVolatile),
    topPrivateNameAliasWrites: topPrivateNameAliasWrites.map(normalizeVolatile),
    namespacePrivateNameAliasWrites: namespacePrivateNameAliasWrites.map(normalizeVolatile),
    malformedExportImportWrites: malformedExportImportWrites.map(normalizeVolatile),
    enumExportAssignmentWrites: enumExportAssignmentWrites.map(normalizeVolatile),
    exportedEnumAssignmentWrites: exportedEnumAssignmentWrites.map(normalizeVolatile),
    exportAssignmentOrderWrites: exportAssignmentOrderWrites.map(normalizeVolatile),
    exportedEnumMergeAssignmentWrites: exportedEnumMergeAssignmentWrites.map(normalizeVolatile),
    exportedNamespaceMergeAssignmentWrites: exportedNamespaceMergeAssignmentWrites.map(normalizeVolatile),
    exportNamespaceOnlyAssignmentWrites: exportNamespaceOnlyAssignmentWrites.map(normalizeVolatile),
    plainEnumExportNamespaceAssignmentWrites: plainEnumExportNamespaceAssignmentWrites.map(normalizeVolatile),
    exportedFunctionNamespaceAssignmentWrites: exportedFunctionNamespaceAssignmentWrites.map(normalizeVolatile),
    exportedClassNamespaceAssignmentWrites: exportedClassNamespaceAssignmentWrites.map(normalizeVolatile),
    defaultClassNamespaceAssignmentWrites: defaultClassNamespaceAssignmentWrites.map(normalizeVolatile),
    templateAliasWrites: templateAliasWrites.map(normalizeVolatile),
    invalidWrites: invalidWrites.map(normalizeVolatile),
    invalidNamespaceWrites: invalidNamespaceWrites.map(normalizeVolatile),
    invalidMergedNamespaceMixedWrites: invalidMergedNamespaceMixedWrites.map(normalizeVolatile),
    invalidNamespaceTypeAliasForwardWrites: invalidNamespaceTypeAliasForwardWrites.map(normalizeVolatile),
    invalidAmbientListedWrites: invalidAmbientListedWrites.map(normalizeVolatile),
    invalidAmbientNestedWrites: invalidAmbientNestedWrites.map(normalizeVolatile),
    invalidAmbientNestedListedWrites: invalidAmbientNestedListedWrites.map(normalizeVolatile),
    invalidAmbientModuleWrites: invalidAmbientModuleWrites.map(normalizeVolatile),
    invalidAmbientTypeAliasForwardWrites: invalidAmbientTypeAliasForwardWrites.map(normalizeVolatile),
    invalidNamespaceMultiVarWrites: invalidNamespaceMultiVarWrites.map(normalizeVolatile),
    invalidNamespaceTypedVariableWrites: invalidNamespaceTypedVariableWrites.map(normalizeVolatile),
    invalidNamespaceDestructureWrites: invalidNamespaceDestructureWrites.map(normalizeVolatile),
    invalidNamespaceBindingDefaultWrites: invalidNamespaceBindingDefaultWrites.map(normalizeVolatile),
    invalidNamespaceQuotedAliasWrites: invalidNamespaceQuotedAliasWrites.map(normalizeVolatile),
    invalidNamespaceMissingAliasWrites: invalidNamespaceMissingAliasWrites.map(normalizeVolatile),
    invalidNamespaceNumericAliasWrites: invalidNamespaceNumericAliasWrites.map(normalizeVolatile),
    invalidNamespaceDotAliasWrites: invalidNamespaceDotAliasWrites.map(normalizeVolatile),
    invalidTopPunctuationAliasWrites: invalidTopPunctuationAliasWrites.map(normalizeVolatile),
    invalidNamespacePunctuationAliasWrites: invalidNamespacePunctuationAliasWrites.map(normalizeVolatile),
    invalidTopPrivateNameAliasWrites: invalidTopPrivateNameAliasWrites.map(normalizeVolatile),
    invalidNamespacePrivateNameAliasWrites: invalidNamespacePrivateNameAliasWrites.map(normalizeVolatile),
    invalidMalformedExportImportWrites: invalidMalformedExportImportWrites.map(normalizeVolatile),
    invalidEnumExportAssignmentWrites: invalidEnumExportAssignmentWrites.map(normalizeVolatile),
    invalidExportedEnumMergeAssignmentWrites: invalidExportedEnumMergeAssignmentWrites.map(normalizeVolatile),
    invalidExportedNamespaceMergeAssignmentWrites: invalidExportedNamespaceMergeAssignmentWrites.map(normalizeVolatile),
    invalidPlainEnumExportNamespaceAssignmentWrites: invalidPlainEnumExportNamespaceAssignmentWrites.map(normalizeVolatile),
    invalidExportedFunctionNamespaceAssignmentWrites: invalidExportedFunctionNamespaceAssignmentWrites.map(normalizeVolatile),
    invalidExportedClassNamespaceAssignmentWrites: invalidExportedClassNamespaceAssignmentWrites.map(normalizeVolatile),
    invalidTemplateAliasWrites: invalidTemplateAliasWrites.map(normalizeVolatile),
    why: normalizeVolatile(why),
    exportImportTypeWhy: normalizeVolatile(exportImportTypeWhy),
    namespaceWhy: normalizeVolatile(namespaceWhy),
    mergedNamespaceBackwardWhy: normalizeVolatile(mergedNamespaceBackwardWhy),
    mergedNamespaceForwardWhy: normalizeVolatile(mergedNamespaceForwardWhy),
    mergedNamespaceMixedWhy: normalizeVolatile(mergedNamespaceMixedWhy),
    namespaceAliasChainWhy: normalizeVolatile(namespaceAliasChainWhy),
    namespaceForwardAliasChainWhy: normalizeVolatile(namespaceForwardAliasChainWhy),
    namespaceDirectAliasChainWhy: normalizeVolatile(namespaceDirectAliasChainWhy),
    namespaceTypeAliasForwardWhy: normalizeVolatile(namespaceTypeAliasForwardWhy),
    ambientWhy: normalizeVolatile(ambientWhy),
    ambientListedWhy: normalizeVolatile(ambientListedWhy),
    ambientNestedWhy: normalizeVolatile(ambientNestedWhy),
    ambientNestedListedWhy: normalizeVolatile(ambientNestedListedWhy),
    ambientModuleWhy: normalizeVolatile(ambientModuleWhy),
    ambientMergeBackwardWhy: normalizeVolatile(ambientMergeBackwardWhy),
    ambientMergeForwardWhy: normalizeVolatile(ambientMergeForwardWhy),
    ambientTypeAliasForwardWhy: normalizeVolatile(ambientTypeAliasForwardWhy),
    namespaceMultiVarWhy: normalizeVolatile(namespaceMultiVarWhy),
    namespaceTypedVariableWhy: normalizeVolatile(namespaceTypedVariableWhy),
    namespaceDestructureWhy: normalizeVolatile(namespaceDestructureWhy),
    namespaceBindingDefaultWhy: normalizeVolatile(namespaceBindingDefaultWhy),
    namespaceQuotedAliasWhy: normalizeVolatile(namespaceQuotedAliasWhy),
    namespaceMissingAliasWhy: normalizeVolatile(namespaceMissingAliasWhy),
    namespaceNumericAliasWhy: normalizeVolatile(namespaceNumericAliasWhy),
    namespaceDotAliasWhy: normalizeVolatile(namespaceDotAliasWhy),
    topPunctuationAliasWhy: normalizeVolatile(topPunctuationAliasWhy),
    namespacePunctuationAliasWhy: normalizeVolatile(namespacePunctuationAliasWhy),
    topPrivateNameAliasWhy: normalizeVolatile(topPrivateNameAliasWhy),
    namespacePrivateNameAliasWhy: normalizeVolatile(namespacePrivateNameAliasWhy),
    malformedExportImportWhy: normalizeVolatile(malformedExportImportWhy),
    enumExportAssignmentWhy: normalizeVolatile(enumExportAssignmentWhy),
    exportedEnumAssignmentWhy: normalizeVolatile(exportedEnumAssignmentWhy),
    exportAssignmentOrderWhy: normalizeVolatile(exportAssignmentOrderWhy),
    exportedEnumMergeAssignmentWhy: normalizeVolatile(exportedEnumMergeAssignmentWhy),
    exportedNamespaceMergeAssignmentWhy: normalizeVolatile(exportedNamespaceMergeAssignmentWhy),
    exportNamespaceOnlyAssignmentWhy: normalizeVolatile(exportNamespaceOnlyAssignmentWhy),
    plainEnumExportNamespaceAssignmentWhy: normalizeVolatile(plainEnumExportNamespaceAssignmentWhy),
    exportedFunctionNamespaceAssignmentWhy: normalizeVolatile(exportedFunctionNamespaceAssignmentWhy),
    exportedClassNamespaceAssignmentWhy: normalizeVolatile(exportedClassNamespaceAssignmentWhy),
    defaultClassNamespaceAssignmentWhy: normalizeVolatile(defaultClassNamespaceAssignmentWhy),
    templateAliasWhy: normalizeVolatile(templateAliasWhy),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/export-variants.ts.dlog",
      ".decisions/src/export-variants.ts.dmap",
      ".decisions/src/export-import-type.ts.dlog",
      ".decisions/src/export-import-type.ts.dmap",
      ".decisions/src/ns-combined.ts.dlog",
      ".decisions/src/ns-combined.ts.dmap",
      ".decisions/src/ns-merge-exported-backward.ts.dlog",
      ".decisions/src/ns-merge-exported-backward.ts.dmap",
      ".decisions/src/ns-merge-exported-forward.ts.dlog",
      ".decisions/src/ns-merge-exported-forward.ts.dmap",
      ".decisions/src/ns-merge-mixed.ts.dlog",
      ".decisions/src/ns-merge-mixed.ts.dmap",
      ".decisions/src/ns-alias-chain.ts.dlog",
      ".decisions/src/ns-alias-chain.ts.dmap",
      ".decisions/src/ns-merge-alias-forward.ts.dlog",
      ".decisions/src/ns-merge-alias-forward.ts.dmap",
      ".decisions/src/ns-alias-direct-and-chain.ts.dlog",
      ".decisions/src/ns-alias-direct-and-chain.ts.dmap",
      ".decisions/src/ns-type-alias-forward.ts.dlog",
      ".decisions/src/ns-type-alias-forward.ts.dmap",
      ".decisions/src/ns-ambient.ts.dlog",
      ".decisions/src/ns-ambient.ts.dmap",
      ".decisions/src/ns-ambient-listed.ts.dlog",
      ".decisions/src/ns-ambient-listed.ts.dmap",
      ".decisions/src/ns-ambient-nested.ts.dlog",
      ".decisions/src/ns-ambient-nested.ts.dmap",
      ".decisions/src/ns-ambient-nested-listed.ts.dlog",
      ".decisions/src/ns-ambient-nested-listed.ts.dmap",
      ".decisions/src/ns-ambient-module.ts.dlog",
      ".decisions/src/ns-ambient-module.ts.dmap",
      ".decisions/src/ns-ambient-merge-backward.ts.dlog",
      ".decisions/src/ns-ambient-merge-backward.ts.dmap",
      ".decisions/src/ns-ambient-merge-forward.ts.dlog",
      ".decisions/src/ns-ambient-merge-forward.ts.dmap",
      ".decisions/src/ns-ambient-type-alias-forward.ts.dlog",
      ".decisions/src/ns-ambient-type-alias-forward.ts.dmap",
      ".decisions/src/ns-multivar.ts.dlog",
      ".decisions/src/ns-multivar.ts.dmap",
      ".decisions/src/ns-typed-initializers.ts.dlog",
      ".decisions/src/ns-typed-initializers.ts.dmap",
      ".decisions/src/ns-destructure.ts.dlog",
      ".decisions/src/ns-destructure.ts.dmap",
      ".decisions/src/ns-binding-defaults.ts.dlog",
      ".decisions/src/ns-binding-defaults.ts.dmap",
      ".decisions/src/ns-quoted-alias.ts.dlog",
      ".decisions/src/ns-quoted-alias.ts.dmap",
      ".decisions/src/ns-missing-alias.ts.dlog",
      ".decisions/src/ns-missing-alias.ts.dmap",
      ".decisions/src/ns-numeric-alias.ts.dlog",
      ".decisions/src/ns-numeric-alias.ts.dmap",
      ".decisions/src/ns-dot-alias.ts.dlog",
      ".decisions/src/ns-dot-alias.ts.dmap",
      ".decisions/src/top-punctuation-alias.ts.dlog",
      ".decisions/src/top-punctuation-alias.ts.dmap",
      ".decisions/src/ns-punctuation-alias.ts.dlog",
      ".decisions/src/ns-punctuation-alias.ts.dmap",
      ".decisions/src/top-private-name-alias.ts.dlog",
      ".decisions/src/top-private-name-alias.ts.dmap",
      ".decisions/src/ns-private-name-alias.ts.dlog",
      ".decisions/src/ns-private-name-alias.ts.dmap",
      ".decisions/src/export-import-malformed.ts.dlog",
      ".decisions/src/export-import-malformed.ts.dmap",
      ".decisions/src/enum-export-assignment.ts.dlog",
      ".decisions/src/enum-export-assignment.ts.dmap",
      ".decisions/src/exported-enum-assignment.ts.dlog",
      ".decisions/src/exported-enum-assignment.ts.dmap",
      ".decisions/src/export-assignment-order.ts.dlog",
      ".decisions/src/export-assignment-order.ts.dmap",
      ".decisions/src/exported-enum-merge-assignment.ts.dlog",
      ".decisions/src/exported-enum-merge-assignment.ts.dmap",
      ".decisions/src/exported-namespace-merge-assignment.ts.dlog",
      ".decisions/src/exported-namespace-merge-assignment.ts.dmap",
      ".decisions/src/export-namespace-only-assignment.ts.dlog",
      ".decisions/src/export-namespace-only-assignment.ts.dmap",
      ".decisions/src/plain-enum-export-namespace-assignment.ts.dlog",
      ".decisions/src/plain-enum-export-namespace-assignment.ts.dmap",
      ".decisions/src/exported-function-namespace-assignment.ts.dlog",
      ".decisions/src/exported-function-namespace-assignment.ts.dmap",
      ".decisions/src/exported-class-namespace-assignment.ts.dlog",
      ".decisions/src/exported-class-namespace-assignment.ts.dmap",
      ".decisions/src/default-class-namespace-assignment.ts.dlog",
      ".decisions/src/default-class-namespace-assignment.ts.dmap",
      ".decisions/src/top-template-alias.ts.dlog",
      ".decisions/src/top-template-alias.ts.dmap"
    ]))
  };
}));

results.push(await scenario("literal-method-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "literal-method-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/literal-methods.ts"),
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
`,
    "utf8"
  );

  const validWrites = [
    ['fn:Names."quoted"', [6, 6], "quoted method"],
    ["fn:Names.42", [7, 7], "numeric method"],
    ['fn:Names."static-name"', [8, 8], "static string method"],
    ["fn:Names.[computed]", [10, 10], "computed method"],
    ["export:RenamedFoo", [1, 1], "renamed type export"],
    ["export:Bar", [2, 2], "type export"],
    ["export:TypeFoo", [1, 1], "inline type export"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/literal-methods.ts",
        anchor,
        lines,
        chose,
        because: "literal method anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidGetter = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/literal-methods.ts",
      anchor: 'fn:Names."value"',
      lines: [9, 9],
      chose: "getter parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const why = run(runtime, ["why", "src/literal-methods.ts"], "", root);

  return {
    validWrites: validWrites.map(normalizeVolatile),
    invalidGetter: normalizeVolatile(invalidGetter),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/literal-methods.ts.dlog", ".decisions/src/literal-methods.ts.dmap"]))
  };
}));

results.push(await scenario("literal-edge-method-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "literal-edge-method-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/literal-edge-methods.ts"),
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
`,
    "utf8"
  );

  const writes = [
    ['fn:Numeric."with\\\\slash"', [2, 2], "escaped slash method"],
    ['fn:Numeric."with\\"quote"', [3, 3], "escaped quote method"],
    ["fn:Numeric.3.14", [4, 4], "decimal method"],
    ["fn:Numeric.1_000", [5, 5], "numeric separator method"],
    ["fn:Numeric.0x10", [6, 6], "hex method"],
    ["fn:Numeric.1e3", [7, 7], "exponent method"],
    ['fn:Computed.["literal"]', [10, 10], "computed literal method"],
    ["fn:Computed.[1 + 2]", [11, 11], "computed expression method"],
    ["fn:Computed.[Symbol.iterator]", [12, 12], "computed member method"],
    ["fn:Computed.[bad ? { x: 1 } : key]", [13, 13], "computed object literal expression method"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/literal-edge-methods.ts",
        anchor,
        lines,
        chose,
        because: "literal edge method anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/literal-edge-methods.ts"], "", root);

  return {
    writes: writes.map(normalizeVolatile),
    why: normalizeVolatile(why),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/literal-edge-methods.ts.dlog", ".decisions/src/literal-edge-methods.ts.dmap"]))
  };
}));

results.push(await scenario("malformed-bigint-class-recovery", async (runtime) => {
  const root = await tempProject(runtime.name, "malformed-bigint-class-recovery");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/bigint-recovery.ts"),
    `class Broken {
  before() {}
  10n() {}
  after() {}
}
function later() { return 1; }
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/unclosed-function.ts"),
    `function broken() {
  if (a && b) return 1;
function later() { return 2; }
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/unclosed-arrow.ts"),
    `const broken = () => {
  if (a && b) return 1;
function later() { return 2; }
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/computed-recovery.ts"),
    `class Broken {
  before() {}
  [bad() {}
  after() {}
}
function later() {}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/method-params-recovery.ts"),
    `class Broken {
  before() {}
  broken(a, b {
    return 1;
  }
  after() {}
}
function later() {}
`,
    "utf8"
  );

  const validWrites = [
    ["class:Broken", [1, 2], "recover broken class range"],
    ["fn:Broken.before", [2, 2], "method before malformed member"],
    ["fn:later", [6, 6], "top-level recovery after broken class"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/bigint-recovery.ts",
        anchor,
        lines,
        chose,
        because: "malformed bigint class recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const unclosedFunctionWrites = [
    ["src/unclosed-function.ts", "fn:broken", [1, 3], "unclosed function body anchor"],
    ["src/unclosed-arrow.ts", "fn:broken", [1, 3], "unclosed arrow body anchor"]
  ].map(([file, anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor,
        lines,
        chose,
        because: "unclosed function body recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const computedRecoveryWrites = [
    ["class:Broken", [1, 3], "recover malformed computed class range"],
    ["fn:Broken.before", [2, 2], "method before malformed computed member"],
    ["fn:later", [6, 6], "top-level recovery after malformed computed class"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/computed-recovery.ts",
        anchor,
        lines,
        chose,
        because: "malformed computed class recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const methodParamsRecoveryWrites = [
    ["class:Broken", [1, 6], "recover malformed method parameter class range"],
    ["fn:Broken.before", [2, 2], "method before malformed parameter member"],
    ["fn:Broken.broken", [3, 6], "malformed parameter method recovery"],
    ["fn:later", [8, 8], "top-level recovery after malformed parameter class"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/method-params-recovery.ts",
        anchor,
        lines,
        chose,
        because: "malformed method parameter class recovery parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidWrites = ["fn:Broken.n", "fn:Broken.after"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/bigint-recovery.ts",
        anchor,
        lines: [3, 4],
        chose: "malformed parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidUnclosedFunctionWrites = ["src/unclosed-function.ts", "src/unclosed-arrow.ts"].map((file) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file,
        anchor: "fn:later",
        lines: [3, 3],
        chose: "unclosed body parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidComputedRecoveryWrites = ["fn:Broken.bad", "fn:Broken.after"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/computed-recovery.ts",
        anchor,
        lines: [3, 4],
        chose: "malformed computed class parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidMethodParamsRecoveryWrites = ["fn:Broken.after"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/method-params-recovery.ts",
        anchor,
        lines: [6, 6],
        chose: "malformed method parameter parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const why = run(runtime, ["why", "src/bigint-recovery.ts"], "", root);
  const unclosedFunctionWhy = run(runtime, ["why", "src/unclosed-function.ts"], "", root);
  const unclosedArrowWhy = run(runtime, ["why", "src/unclosed-arrow.ts"], "", root);
  const computedRecoveryWhy = run(runtime, ["why", "src/computed-recovery.ts"], "", root);
  const methodParamsRecoveryWhy = run(runtime, ["why", "src/method-params-recovery.ts"], "", root);

  return {
    validWrites: validWrites.map(normalizeVolatile),
    unclosedFunctionWrites: unclosedFunctionWrites.map(normalizeVolatile),
    computedRecoveryWrites: computedRecoveryWrites.map(normalizeVolatile),
    methodParamsRecoveryWrites: methodParamsRecoveryWrites.map(normalizeVolatile),
    invalidWrites: invalidWrites.map(normalizeVolatile),
    invalidUnclosedFunctionWrites: invalidUnclosedFunctionWrites.map(normalizeVolatile),
    invalidComputedRecoveryWrites: invalidComputedRecoveryWrites.map(normalizeVolatile),
    invalidMethodParamsRecoveryWrites: invalidMethodParamsRecoveryWrites.map(normalizeVolatile),
    why: normalizeVolatile(why),
    unclosedFunctionWhy: normalizeVolatile(unclosedFunctionWhy),
    unclosedArrowWhy: normalizeVolatile(unclosedArrowWhy),
    computedRecoveryWhy: normalizeVolatile(computedRecoveryWhy),
    methodParamsRecoveryWhy: normalizeVolatile(methodParamsRecoveryWhy),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/bigint-recovery.ts.dlog",
      ".decisions/src/bigint-recovery.ts.dmap",
      ".decisions/src/unclosed-function.ts.dlog",
      ".decisions/src/unclosed-function.ts.dmap",
      ".decisions/src/unclosed-arrow.ts.dlog",
      ".decisions/src/unclosed-arrow.ts.dmap",
      ".decisions/src/computed-recovery.ts.dlog",
      ".decisions/src/computed-recovery.ts.dmap",
      ".decisions/src/method-params-recovery.ts.dlog",
      ".decisions/src/method-params-recovery.ts.dmap"
    ]))
  };
}));

results.push(await scenario("template-block-anchor-write", async (runtime) => {
  const root = await tempProject(runtime.name, "template-block-anchor-write");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/template-block.ts"),
    `const tpl = \`line one
\${(() => { if (a && b || c) { return 1; } return 0; })()}
line three\`;
function real() { return 1; }
const after = () => true;
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/nested-template-block.ts"),
    "const tpl = tag`outer ${(() => tag2`inner ${(() => { if (a && b || c) { return 1; } return 0; })()}`)()} done`;\nfunction real() { return 1; }\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-regex-block.ts"),
    "const tpl = `x ${/}/.test(s) ? (() => { if (a && b || c) { return 1; } return 0; })() : 0}`;\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-division-block.ts"),
    "const tpl = `x ${a / } / b ? (() => { if (a && b || c) { return 1; } return 0; })() : 0}`;\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-multiline-nested.ts"),
    "const tpl = `outer ${tag`inner ${(() => {\n  if (a && b || c) { return 1; }\n  return 0;\n})()}`}`;\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-nested-unclosed-inner.ts"),
    "const tpl = `outer ${tag`inner ${(() => { if (a && b || c) { return 1; } return 0; })()} done`;\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-nested-open-expression.ts"),
    "const tpl = `outer ${tag`inner ${(() => { if (a && b || c) { return 1; } return 0; })()}\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/nested-arrow.ts"),
    "const call = foo(() => true);\nconst paren = (() => true)();\nconst object = { run: () => true };\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-escaped-backtick.ts"),
    "const tpl = `line \\` still template ${(() => { if (a && b || c) { return 1; } return 0; })()}`;\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-escaped-dollar.ts"),
    "const tpl = `literal \\${notExpression}`;\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-unterminated-raw.ts"),
    "const tpl = `function hidden() {}\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-unterminated-closed-expression.ts"),
    "const tpl = `x ${(() => { if (a && b || c) { return 1; } return 0; })()}\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/template-unterminated-open-expression.ts"),
    "const tpl = `x ${(() => { if (a && b || c) { return 1; } return 0; })()\nfunction after() {}\n",
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "src/else-if-blocks.ts"),
    "function run() {\n  if (a && b && c) { return 1; }\n  else if (d && e && f) { return 2; }\n}\n",
    "utf8"
  );

  const validWrites = [
    ["fn:real", [4, 4], "real function"],
    ["fn:after", [5, 5], "after arrow function"],
    ["block:if_a_b_c", [2, 2], "template expression block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-block.ts",
        anchor,
        lines,
        chose,
        because: "template block anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const elseIfWrites = [
    ["fn:run", [1, 4], "else-if chain function"],
    ["block:if_a_b_c", [2, 3], "leading else-if chain block"],
    ["block:if_d_e_f", [3, 3], "nested else-if block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/else-if-blocks.ts",
        anchor,
        lines,
        chose,
        because: "else-if block chain parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidTemplateVariable = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/template-block.ts",
      anchor: "fn:tpl",
      lines: [1, 3],
      chose: "template variable parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const nestedTaggedWrites = [
    ["fn:real", [2, 2], "nested tagged template real function"],
    ["block:if_a_b_c", [1, 1], "nested tagged template block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/nested-template-block.ts",
        anchor,
        lines,
        chose,
        because: "nested tagged template anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNestedTaggedVariable = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/nested-template-block.ts",
      anchor: "fn:tpl",
      lines: [1, 1],
      chose: "nested tagged template variable parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const regexTemplateWrites = [
    ["fn:after", [2, 2], "regex template real function"],
    ["block:if_a_b_c", [1, 1], "regex template expression block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-regex-block.ts",
        anchor,
        lines,
        chose,
        because: "regex template expression anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidRegexTemplateVariable = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/template-regex-block.ts",
      anchor: "fn:tpl",
      lines: [1, 1],
      chose: "regex template variable parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const divisionTemplateWrites = [
    ["fn:after", [2, 2], "division-like template real function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-division-block.ts",
        anchor,
        lines,
        chose,
        because: "division-like template expression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidDivisionTemplateWrites = ["fn:tpl", "block:if_a_b_c"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-division-block.ts",
        anchor,
        lines: [1, 1],
        chose: "division-like template parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const multilineNestedWrites = [
    ["fn:after", [5, 5], "multiline nested template real function"],
    ["block:if_a_b_c", [2, 2], "multiline nested template block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-multiline-nested.ts",
        anchor,
        lines,
        chose,
        because: "multiline nested template anchor parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidMultilineNestedVariable = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/template-multiline-nested.ts",
      anchor: "fn:tpl",
      lines: [1, 4],
      chose: "multiline nested template variable parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const nestedUnclosedInnerWrites = [
    ["fn:after", [2, 2], "nested unclosed template real function"],
    ["block:if_a_b_c", [1, 1], "nested unclosed template block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-nested-unclosed-inner.ts",
        anchor,
        lines,
        chose,
        because: "nested unclosed template expression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const nestedOpenExpressionWrites = [
    ["block:if_a_b_c", [1, 1], "nested open template expression block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-nested-open-expression.ts",
        anchor,
        lines,
        chose,
        because: "nested open template expression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNestedOpenExpressionWrites = ["fn:after", "fn:tpl"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-nested-open-expression.ts",
        anchor,
        lines: [1, 2],
        chose: "nested open template parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const nestedArrowWrites = [
    ["fn:after", [4, 4], "function after nested arrow expressions"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/nested-arrow.ts",
        anchor,
        lines,
        chose,
        because: "nested arrow initializer suppression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidNestedArrowWrites = ["fn:call", "fn:paren", "fn:object"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/nested-arrow.ts",
        anchor,
        lines: [1, 3],
        chose: "nested arrow initializer parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const escapedBacktickWrites = [
    ["fn:after", [2, 2], "escaped backtick template real function"],
    ["block:if_a_b_c", [1, 1], "escaped backtick template block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-escaped-backtick.ts",
        anchor,
        lines,
        chose,
        because: "escaped backtick template parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const escapedDollarWrites = [
    ["fn:after", [2, 2], "escaped dollar template real function"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-escaped-dollar.ts",
        anchor,
        lines,
        chose,
        because: "escaped dollar template parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const unterminatedClosedExpressionWrites = [
    ["block:if_a_b_c", [1, 1], "unterminated template closed expression block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-unterminated-closed-expression.ts",
        anchor,
        lines,
        chose,
        because: "unterminated template closed expression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const unterminatedOpenExpressionWrites = [
    ["fn:after", [2, 2], "unterminated template open expression real function"],
    ["block:if_a_b_c", [1, 1], "unterminated template open expression block"]
  ].map(([anchor, lines, chose]) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-unterminated-open-expression.ts",
        anchor,
        lines,
        chose,
        because: "unterminated template open expression parity fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidEscapedBacktickVariable = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/template-escaped-backtick.ts",
      anchor: "fn:tpl",
      lines: [1, 1],
      chose: "escaped backtick template variable parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const invalidEscapedDollarWrites = ["fn:tpl", "block:if_notExpression"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-escaped-dollar.ts",
        anchor,
        lines: [1, 1],
        chose: "escaped dollar template parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidUnterminatedRawWrites = ["fn:hidden", "fn:after"].map((anchor) =>
    run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/template-unterminated-raw.ts",
        anchor,
        lines: [1, 2],
        chose: "unterminated raw template parser ghost",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    )
  );
  const invalidUnterminatedClosedExpressionAfter = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/template-unterminated-closed-expression.ts",
      anchor: "fn:after",
      lines: [2, 2],
      chose: "unterminated closed expression parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const invalidUnterminatedOpenExpressionVariable = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/template-unterminated-open-expression.ts",
      anchor: "fn:tpl",
      lines: [1, 2],
      chose: "unterminated open expression variable parser ghost",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  const why = run(runtime, ["why", "src/template-block.ts"], "", root);
  const nestedWhy = run(runtime, ["why", "src/nested-template-block.ts"], "", root);
  const regexTemplateWhy = run(runtime, ["why", "src/template-regex-block.ts"], "", root);
  const divisionTemplateWhy = run(runtime, ["why", "src/template-division-block.ts"], "", root);
  const multilineNestedWhy = run(runtime, ["why", "src/template-multiline-nested.ts"], "", root);
  const nestedUnclosedInnerWhy = run(runtime, ["why", "src/template-nested-unclosed-inner.ts"], "", root);
  const nestedOpenExpressionWhy = run(runtime, ["why", "src/template-nested-open-expression.ts"], "", root);
  const nestedArrowWhy = run(runtime, ["why", "src/nested-arrow.ts"], "", root);
  const escapedBacktickWhy = run(runtime, ["why", "src/template-escaped-backtick.ts"], "", root);
  const escapedDollarWhy = run(runtime, ["why", "src/template-escaped-dollar.ts"], "", root);
  const unterminatedClosedExpressionWhy = run(runtime, ["why", "src/template-unterminated-closed-expression.ts"], "", root);
  const unterminatedOpenExpressionWhy = run(runtime, ["why", "src/template-unterminated-open-expression.ts"], "", root);
  const elseIfWhy = run(runtime, ["why", "src/else-if-blocks.ts"], "", root);

  return {
    validWrites: validWrites.map(normalizeVolatile),
    invalidTemplateVariable: normalizeVolatile(invalidTemplateVariable),
    nestedTaggedWrites: nestedTaggedWrites.map(normalizeVolatile),
    invalidNestedTaggedVariable: normalizeVolatile(invalidNestedTaggedVariable),
    regexTemplateWrites: regexTemplateWrites.map(normalizeVolatile),
    invalidRegexTemplateVariable: normalizeVolatile(invalidRegexTemplateVariable),
    divisionTemplateWrites: divisionTemplateWrites.map(normalizeVolatile),
    invalidDivisionTemplateWrites: invalidDivisionTemplateWrites.map(normalizeVolatile),
    multilineNestedWrites: multilineNestedWrites.map(normalizeVolatile),
    invalidMultilineNestedVariable: normalizeVolatile(invalidMultilineNestedVariable),
    nestedUnclosedInnerWrites: nestedUnclosedInnerWrites.map(normalizeVolatile),
    nestedOpenExpressionWrites: nestedOpenExpressionWrites.map(normalizeVolatile),
    invalidNestedOpenExpressionWrites: invalidNestedOpenExpressionWrites.map(normalizeVolatile),
    nestedArrowWrites: nestedArrowWrites.map(normalizeVolatile),
    invalidNestedArrowWrites: invalidNestedArrowWrites.map(normalizeVolatile),
    escapedBacktickWrites: escapedBacktickWrites.map(normalizeVolatile),
    escapedDollarWrites: escapedDollarWrites.map(normalizeVolatile),
    unterminatedClosedExpressionWrites: unterminatedClosedExpressionWrites.map(normalizeVolatile),
    unterminatedOpenExpressionWrites: unterminatedOpenExpressionWrites.map(normalizeVolatile),
    elseIfWrites: elseIfWrites.map(normalizeVolatile),
    invalidEscapedBacktickVariable: normalizeVolatile(invalidEscapedBacktickVariable),
    invalidEscapedDollarWrites: invalidEscapedDollarWrites.map(normalizeVolatile),
    invalidUnterminatedRawWrites: invalidUnterminatedRawWrites.map(normalizeVolatile),
    invalidUnterminatedClosedExpressionAfter: normalizeVolatile(invalidUnterminatedClosedExpressionAfter),
    invalidUnterminatedOpenExpressionVariable: normalizeVolatile(invalidUnterminatedOpenExpressionVariable),
    why: normalizeVolatile(why),
    nestedWhy: normalizeVolatile(nestedWhy),
    regexTemplateWhy: normalizeVolatile(regexTemplateWhy),
    divisionTemplateWhy: normalizeVolatile(divisionTemplateWhy),
    multilineNestedWhy: normalizeVolatile(multilineNestedWhy),
    nestedUnclosedInnerWhy: normalizeVolatile(nestedUnclosedInnerWhy),
    nestedOpenExpressionWhy: normalizeVolatile(nestedOpenExpressionWhy),
    nestedArrowWhy: normalizeVolatile(nestedArrowWhy),
    escapedBacktickWhy: normalizeVolatile(escapedBacktickWhy),
    escapedDollarWhy: normalizeVolatile(escapedDollarWhy),
    unterminatedClosedExpressionWhy: normalizeVolatile(unterminatedClosedExpressionWhy),
    unterminatedOpenExpressionWhy: normalizeVolatile(unterminatedOpenExpressionWhy),
    elseIfWhy: normalizeVolatile(elseIfWhy),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/template-block.ts.dlog",
      ".decisions/src/template-block.ts.dmap",
      ".decisions/src/nested-template-block.ts.dlog",
      ".decisions/src/nested-template-block.ts.dmap",
      ".decisions/src/template-regex-block.ts.dlog",
      ".decisions/src/template-regex-block.ts.dmap",
      ".decisions/src/template-division-block.ts.dlog",
      ".decisions/src/template-division-block.ts.dmap",
      ".decisions/src/template-multiline-nested.ts.dlog",
      ".decisions/src/template-multiline-nested.ts.dmap",
      ".decisions/src/template-nested-unclosed-inner.ts.dlog",
      ".decisions/src/template-nested-unclosed-inner.ts.dmap",
      ".decisions/src/template-nested-open-expression.ts.dlog",
      ".decisions/src/template-nested-open-expression.ts.dmap",
      ".decisions/src/nested-arrow.ts.dlog",
      ".decisions/src/nested-arrow.ts.dmap",
      ".decisions/src/template-escaped-backtick.ts.dlog",
      ".decisions/src/template-escaped-backtick.ts.dmap",
      ".decisions/src/template-escaped-dollar.ts.dlog",
      ".decisions/src/template-escaped-dollar.ts.dmap",
      ".decisions/src/template-unterminated-closed-expression.ts.dlog",
      ".decisions/src/template-unterminated-closed-expression.ts.dmap",
      ".decisions/src/template-unterminated-open-expression.ts.dlog",
      ".decisions/src/template-unterminated-open-expression.ts.dmap",
      ".decisions/src/else-if-blocks.ts.dlog",
      ".decisions/src/else-if-blocks.ts.dmap"
    ]))
  };
}));

results.push(await knownImprovementScenario(
  "mcp-initialize-tools",
  async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-initialize-tools");
  return {
    command: run(
      runtime,
      ["mcp"],
      [
        JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }),
        JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} })
      ].join("\n"),
      root
    )
  };
  },
  (typescript, rust) =>
    typescript.command.status === rust.command.status &&
    typescript.command.stderr === rust.command.stderr &&
    JSON.stringify(canonicalizeWhyToolImprovement(normalizeMcpResponses(typescript.command.stdout))) ===
      JSON.stringify(canonicalizeWhyToolImprovement(normalizeMcpResponses(rust.command.stdout))),
  WHY_LINE_IMPROVEMENT_REASON
));

results.push(await scenario("mcp-write-why-supersede-session", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-write-why-supersede-session");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(
    path.join(root, "src/mcp-flow.ts"),
    "export function first() {\n  return 1;\n}\nexport function second() {\n  return 2;\n}\n",
    "utf8"
  );
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: {
            file: "src/mcp-flow.ts",
            anchor: "fn:first",
            lines: [1, 3],
            chose: "first MCP decision",
            because: "MCP success flow fixture",
            rejected: [{ approach: "skip MCP", reason: "would not cover agent workflow" }],
            session: "mcp_session",
            expires_if: "MCP contract changes"
          }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "tools/call",
        params: {
          name: "why",
          arguments: { file: "src/mcp-flow.ts", anchor: "fn:first" }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 3,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: {
            file: "src/mcp-flow.ts",
            anchor: "fn:second",
            lines: [4, 6],
            chose: "second MCP decision",
            because: "MCP supersede flow fixture",
            rejected: [{ approach: "keep first", reason: "responsibility moved" }],
            supersedes: "dec_001",
            session: "mcp_session_2"
          }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 4,
        method: "tools/call",
        params: {
          name: "why",
          arguments: { file: "src/mcp-flow.ts", anchor: "fn:first" }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 5,
        method: "tools/call",
        params: {
          name: "why",
          arguments: { file: "src/mcp-flow.ts", anchor: "fn:second" }
        }
      })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    responses: normalizeMcpResponses(normalizeText(command.stdout)),
    stderr: normalizeText(command.stderr),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/mcp-flow.ts.dlog",
      ".decisions/src/mcp-flow.ts.dmap"
    ]))
  };
}));

results.push(await scenario("mcp-tools-call-envelope-quirks", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-tools-call-envelope-quirks");
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({ jsonrpc: "2.0", id: 9, method: "tools/call" }),
      JSON.stringify({ jsonrpc: "2.0", id: 10, method: "tools/call", params: null }),
      JSON.stringify({ jsonrpc: "2.0", id: 11, method: "tools/call", params: { name: "nope" } }),
      JSON.stringify({ jsonrpc: "2.0", id: 12, method: "tools/call", params: { name: ["why"], arguments: {} } }),
      JSON.stringify({ jsonrpc: "2.0", id: 13, method: "tools/call", params: { name: { tool: "why" }, arguments: {} } })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    stdout: normalizeMcpResponses(command.stdout),
    stderr: command.stderr
  };
}));

results.push(await scenario("mcp-tools-call-argument-envelope-quirks", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-tools-call-argument-envelope-quirks");
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({
        jsonrpc: "2.0",
        method: "tools/call",
        params: { name: "why", arguments: { file: "src/missing.ts" } }
      }),
      JSON.stringify({ jsonrpc: "2.0", id: "string-id", method: "tools/call", params: { name: "why", arguments: null } }),
      JSON.stringify({ jsonrpc: "2.0", id: false, method: "tools/call", params: { name: "why", arguments: [] } }),
      JSON.stringify({ jsonrpc: "2.0", id: 14, method: "tools/call", params: { name: "why", arguments: "nope" } }),
      JSON.stringify({ jsonrpc: "2.0", id: 15, method: "tools/call", params: { name: "why", arguments: { file: "src/missing.ts" } } }),
      JSON.stringify({ jsonrpc: "2.0", id: 16, method: "tools/list", params: {} })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    stdout: normalizeMcpEnvelopeResponses(command.stdout),
    stderr: command.stderr
  };
}));

results.push(await scenario("mcp-stdio-edge-cases", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-stdio-edge-cases");
  const command = run(
    runtime,
    ["mcp"],
    [
	      "{\"jsonrpc\":\"2.0\",\"id\":1}",
	      "{\"jsonrpc\":\"2.0\",\"id\":null}",
	      "{\"jsonrpc\":\"2.0\"}",
	      "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"\"}",
	      "{\"jsonrpc\":\"2.0\",\"method\":\"\"}",
	      "{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}",
	      "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"id\":99}",
	      "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"does/not/exist\"}",
      "{bad"
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    stdout: normalizeMcpResponses(command.stdout),
    stderr: command.stderr
  };
}));

results.push(await knownImprovementScenario(
  "mcp-notification-and-no-id-parity",
  async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-notification-and-no-id-parity");
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({ jsonrpc: "2.0", method: "initialize", params: {} }),
      JSON.stringify({ jsonrpc: "2.0", method: "tools/list", params: {} }),
      JSON.stringify({
        jsonrpc: "2.0",
        method: "tools/call",
        params: { name: "why", arguments: { file: "src/missing.ts" } }
      }),
      JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
      JSON.stringify({ jsonrpc: "2.0", id: 17, method: "notifications/initialized" }),
      JSON.stringify({ jsonrpc: "2.0", id: 18, method: "tools/list", params: {} })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    stdout: normalizeMcpResponses(command.stdout),
    stderr: command.stderr
  };
  },
  mcpToolsListImprovementAccepts,
  WHY_LINE_IMPROVEMENT_REASON
));

results.push(await knownImprovementScenario(
  "mcp-notification-suppresses-tool-side-effects",
  async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-notification-suppresses-tool-side-effects");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/notify.ts"), "export function notify() {\n  return 1;\n}\n", "utf8");
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "notifications/tools/call",
        params: {
          name: "write_decision",
          arguments: {
            file: "src/notify.ts",
            anchor: "fn:notify",
            lines: [1, 3],
            chose: "notification should not write",
            because: "MCP notification suppression fixture",
            rejected: []
          }
        }
      }),
      JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    stdout: normalizeMcpResponses(command.stdout),
    stderr: command.stderr,
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/notify.ts.dlog",
      ".decisions/src/notify.ts.dmap"
    ]))
  };
  },
  (typescript, rust) =>
    JSON.stringify(typescript.files) === JSON.stringify(rust.files) &&
    mcpToolsListImprovementAccepts(
      { status: typescript.status, stdout: typescript.stdout, stderr: typescript.stderr },
      { status: rust.status, stdout: rust.stdout, stderr: rust.stderr }
    ),
  WHY_LINE_IMPROVEMENT_REASON
));

results.push(await knownImprovementScenario(
  "mcp-invalid-utf8-stdio-resilience",
  async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-invalid-utf8-stdio-resilience");
  const input = Buffer.concat([
    Buffer.from([0xff, 0x0a]),
    Buffer.from(`${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} })}\n`, "utf8")
  ]);
  const command = runBinaryInput(runtime, ["mcp"], input, root);
  return {
    status: command.status,
    stdout: normalizeMcpResponses(command.stdout),
    stderr: command.stderr
  };
  },
  mcpToolsListImprovementAccepts,
  WHY_LINE_IMPROVEMENT_REASON
));

results.push(await knownImprovementScenario(
  "mcp-oversized-stdio-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "mcp-oversized-stdio-hardening-improvement");
    const command = runBinaryInput(
      runtime,
      ["mcp"],
      Buffer.concat([
        Buffer.alloc(10 * 1024 * 1024 + 1, "{"),
        Buffer.from(`\n${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} })}\n`, "utf8")
      ]),
      root
    );
    return {
      status: command.status,
      firstErrorMessage: firstMcpErrorMessage(command.stdout),
      stdout: normalizeMcpResponses(command.stdout),
      stderr: command.stderr
    };
  },
  (typescript, rust) => {
    const tsResponses = typescript.stdout as Array<{ id?: unknown; error?: { code?: number }; result?: { tools?: unknown[] } }>;
    const rustResponses = rust.stdout as Array<{ id?: unknown; error?: { code?: number; message?: string }; result?: { tools?: unknown[] } }>;
    return Boolean(
      typescript.status === 0 &&
      rust.status === 0 &&
      typescript.stderr === "" &&
      rust.stderr === "" &&
      tsResponses.length === 2 &&
      rustResponses.length === 2 &&
      tsResponses[0]?.error?.code === -32700 &&
      typeof typescript.firstErrorMessage === "string" &&
      tsResponses[1]?.id === 2 &&
      Array.isArray(tsResponses[1]?.result?.tools) &&
      rustResponses[0]?.id === null &&
      rustResponses[0]?.error?.code === -32700 &&
      rust.firstErrorMessage === "JSON input exceeds configured byte limit" &&
      rustResponses[1]?.id === 2 &&
      Array.isArray(rustResponses[1]?.result?.tools)
    );
  },
  "Rust intentionally bounds MCP stdio request lines before JSON parsing while preserving session recovery for the next valid request."
));

results.push(await knownImprovementScenario(
  "mcp-invalid-json-rpc-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "mcp-invalid-json-rpc-hardening-improvement");
    const command = run(
      runtime,
      ["mcp"],
      [
        "[]",
        "42",
        "\"text\"",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":42}"
      ].join("\n"),
      root
    );
    return {
      status: command.status,
      stdout: command.stdout ? normalizeMcpResponses(command.stdout) : [],
      stderr: command.stderr
    };
  },
  (typescript, rust) => {
    const rustResponses = rust.stdout as Array<{ id?: unknown; error?: { code?: number; message?: string } }>;
    return Boolean(
      typescript.status === 1 &&
      (typescript.stdout as unknown[]).length === 0 &&
      String(typescript.stderr).includes("startsWith is not a function") &&
      rust.status === 0 &&
      rust.stderr === "" &&
      rustResponses.length === 4 &&
      rustResponses.slice(0, 3).every((response) => response.id === null && response.error?.code === -32600 && response.error?.message === "Invalid request") &&
      rustResponses[3]?.id === 3 &&
      rustResponses[3]?.error?.code === -32600 &&
      rustResponses[3]?.error?.message === "Missing method"
    );
  },
  "Rust intentionally returns JSON-RPC Invalid request errors for non-object requests and non-string methods instead of ignoring primitives or crashing like the TypeScript server."
));

results.push(await scenario("mcp-write-decision-invalid-inputs", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-write-decision-invalid-inputs");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/bad.ts"), "export function bad() {\n  return 1;\n}\n", "utf8");
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: { file: "src/bad.ts", anchor: "fn:bad", lines: [1, 3], chose: "", because: "fixture", rejected: [] }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: { file: "src/bad.ts", anchor: "fn:bad", lines: [3, 2], chose: "bad lines", because: "fixture", rejected: [] }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 3,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: {
            file: "src/bad.ts",
            anchor: "fn:bad",
            lines: [1, 3],
            chose: "bad rejected",
            because: "fixture",
            rejected: [{ approach: "a" }]
          }
        }
      })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    errors: normalizeMcpErrorSummaries(command.stdout),
    stderr: command.stderr,
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/bad.ts.dlog", ".decisions/src/bad.ts.dmap"]))
  };
}));

results.push(await scenario("mcp-corrupt-dlog-session-resilience", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-corrupt-dlog-session-resilience");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/bad.ts"), "export function bad() {\n  return 1;\n}\n", "utf8");
  const corruptDlog = "schema: nope\nfile: src/bad.ts\ndecisions: {}\n";
  await fs.writeFile(path.join(root, ".decisions/src/bad.ts.dlog"), corruptDlog, "utf8");
  const command = run(
    runtime,
    ["mcp"],
    [
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: { name: "why", arguments: { file: "src/bad.ts" } }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: {
            file: "src/bad.ts",
            anchor: "fn:bad",
            lines: [1, 3],
            chose: "do not overwrite corruption over MCP",
            because: "MCP corrupt dlog fixture",
            rejected: []
          }
        }
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 3,
        method: "tools/call",
        params: { name: "ghost_check", arguments: { file: "src/bad.ts" } }
      }),
      JSON.stringify({ jsonrpc: "2.0", id: 4, method: "tools/list", params: {} })
    ].join("\n"),
    root
  );
  return {
    status: command.status,
    responses: normalizeMcpCorruptStorageResponses(command.stdout),
    stderr: normalizeText(command.stderr),
    files: normalizeFiles(await readProjectFiles(root, [
      ".decisions/src/bad.ts.dlog",
      ".decisions/src/bad.ts.dmap",
      ".decisions/src/bad.ts.lock"
    ])),
    tempSiblings: await decisionTempSiblings(root, "src")
  };
}));

results.push(await knownImprovementScenario(
  "mcp-write-decision-path-hardening-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "mcp-write-decision-path-hardening-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/x.ts"), "export function x() {\n  return 1;\n}\n", "utf8");
    const command = run(
      runtime,
      ["mcp"],
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: {
          name: "write_decision",
          arguments: {
            file: "src/../src/x.ts",
            anchor: "fn:x",
            lines: [1, 3],
            chose: "mcp path hardening",
            because: "fixture",
            rejected: []
          }
        }
      }),
      root
    );
    return {
      status: command.status,
      responses: normalizeMcpResponses(command.stdout),
      stderr: command.stderr,
      files: normalizeFiles(await readProjectFiles(root, [".decisions/src/x.ts.dlog", ".decisions/src/x.ts.dmap"]))
    };
  },
  (typescript, rust) => {
    const tsResponses = typescript.responses as Array<{ result?: { content?: Array<{ text?: string }> } }>;
    const rustResponses = rust.responses as Array<{ error?: { code?: number; message?: string } }>;
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.status === 0 &&
      rust.status === 0 &&
      tsResponses[0]?.result?.content?.[0]?.text === "Recorded dec_001." &&
      rustResponses[0]?.error?.code === -32000 &&
      rustResponses[0]?.error?.message?.includes("parent path segments are not allowed") &&
      tsFiles[".decisions/src/x.ts.dlog"]?.includes("mcp path hardening") &&
      tsFiles[".decisions/src/x.ts.dmap"] === "1-3:fn:x\n" &&
      rustFiles[".decisions/src/x.ts.dlog"] === null &&
      rustFiles[".decisions/src/x.ts.dmap"] === null
    );
  },
  "Rust intentionally applies hardened project-relative path validation to MCP write_decision while TypeScript normalizes parent segments."
));

results.push(await scenario("status-drift-side-effect", async (runtime) => {
  const root = await tempProject(runtime.name, "status-drift-side-effect");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function compute() {\n  return 1;\n}\n", "utf8");
  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/drift.ts",
      anchor: "fn:compute",
      lines: [1, 3],
      chose: "return one",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function compute() {\n  return 2;\n}\n", "utf8");
  const status = run(runtime, ["status"], "", root);
  return {
    write,
    status: normalizeVolatile(status),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/drift.ts.dlog", ".decisions/src/drift.ts.dmap"]))
  };
}));

results.push(await knownImprovementScenario(
  "read-repairs-stale-dmap-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "read-repairs-stale-dmap-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/map.ts"), "export function compute() {\n  return 1;\n}\n", "utf8");
    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/map.ts",
        anchor: "fn:compute",
        lines: [1, 3],
        chose: "repair derivative map",
        because: "stale dmap fixture",
        rejected: []
      })],
      "",
      root
    );
    await fs.writeFile(path.join(root, ".decisions/src/map.ts.dmap"), "99-100:fn:old\n", "utf8");
    const status = run(runtime, ["status"], "", root);
    const session = run(runtime, ["hooks", "session-start"], "", root);
    return {
      write: normalizeVolatile(write),
      status: normalizeVolatile(status),
      session: normalizeVolatile(session),
      files: normalizeFiles(await readProjectFiles(root, [".decisions/src/map.ts.dmap"]))
    };
  },
  (typescript, rust) => {
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.write.status === 0 &&
      rust.write.status === 0 &&
      typescript.status.stdout === rust.status.stdout &&
      typescript.session.stdout === rust.session.stdout &&
      tsFiles[".decisions/src/map.ts.dmap"] === "99-100:fn:old\n" &&
      rustFiles[".decisions/src/map.ts.dmap"] === "1-3:fn:compute\n"
    );
  },
  "Rust intentionally repairs stale derivative .dmap files during status and session-start while TypeScript leaves them untouched."
));

results.push(await knownImprovementScenario(
  "read-repairs-missing-corrupt-dmap-improvement",
  async (runtime) => {
    async function repairRoot(suffix: string): Promise<{
      root: string;
      missingWrite: Record<string, unknown>;
      corruptWrite: Record<string, unknown>;
    }> {
      const root = await tempProject(runtime.name, `read-repairs-missing-corrupt-dmap-improvement-${suffix}`);
      await fs.mkdir(path.join(root, "src"), { recursive: true });
      await fs.writeFile(path.join(root, "src/missing.ts"), "export function missing() {\n  return 1;\n}\n", "utf8");
      await fs.writeFile(path.join(root, "src/corrupt.ts"), "export function corrupt() {\n  return 1;\n}\n", "utf8");
      const missingWrite = run(
        runtime,
        ["write-decision", "--json", JSON.stringify({
          file: "src/missing.ts",
          anchor: "fn:missing",
          lines: [1, 3],
          chose: "repair missing derivative map",
          because: "missing dmap fixture",
          rejected: []
        })],
        "",
        root
      );
      const corruptWrite = run(
        runtime,
        ["write-decision", "--json", JSON.stringify({
          file: "src/corrupt.ts",
          anchor: "fn:corrupt",
          lines: [1, 3],
          chose: "repair corrupt derivative map",
          because: "corrupt dmap fixture",
          rejected: []
        })],
        "",
        root
      );
      await fs.rm(path.join(root, ".decisions/src/missing.ts.dmap"), { force: true });
      await fs.writeFile(path.join(root, ".decisions/src/corrupt.ts.dmap"), "not:dmap:valid:enough?\n", "utf8");
      return {
        root,
        missingWrite: normalizeVolatile(missingWrite),
        corruptWrite: normalizeVolatile(corruptWrite)
      };
    }

    const statusRoot = await repairRoot("status");
    const status = run(runtime, ["status"], "", statusRoot.root);
    const sessionRoot = await repairRoot("session-start");
    const session = run(runtime, ["hooks", "session-start"], "", sessionRoot.root);
    return {
      statusRepair: {
        missingWrite: statusRoot.missingWrite,
        corruptWrite: statusRoot.corruptWrite,
        command: normalizeVolatile(status),
        files: normalizeFiles(await readProjectFiles(statusRoot.root, [
          ".decisions/src/missing.ts.dmap",
          ".decisions/src/corrupt.ts.dmap"
        ]))
      },
      sessionRepair: {
        missingWrite: sessionRoot.missingWrite,
        corruptWrite: sessionRoot.corruptWrite,
        command: normalizeVolatile(session),
        files: normalizeFiles(await readProjectFiles(sessionRoot.root, [
          ".decisions/src/missing.ts.dmap",
          ".decisions/src/corrupt.ts.dmap"
        ]))
      }
    };
  },
  (typescript, rust) => {
    const tsStatus = typescript.statusRepair as {
      missingWrite: CommandResult;
      corruptWrite: CommandResult;
      command: CommandResult;
      files: Record<string, string | null>;
    };
    const rustStatus = rust.statusRepair as {
      missingWrite: CommandResult;
      corruptWrite: CommandResult;
      command: CommandResult;
      files: Record<string, string | null>;
    };
    const tsSession = typescript.sessionRepair as {
      missingWrite: CommandResult;
      corruptWrite: CommandResult;
      command: CommandResult;
      files: Record<string, string | null>;
    };
    const rustSession = rust.sessionRepair as {
      missingWrite: CommandResult;
      corruptWrite: CommandResult;
      command: CommandResult;
      files: Record<string, string | null>;
    };
    return Boolean(
      tsStatus.missingWrite.status === 0 &&
      rustStatus.missingWrite.status === 0 &&
      tsStatus.corruptWrite.status === 0 &&
      rustStatus.corruptWrite.status === 0 &&
      tsStatus.command.status === 0 &&
      rustStatus.command.status === 0 &&
      tsStatus.command.stdout === rustStatus.command.stdout &&
      tsStatus.command.stderr === rustStatus.command.stderr &&
      tsStatus.files[".decisions/src/missing.ts.dmap"] === null &&
      tsStatus.files[".decisions/src/corrupt.ts.dmap"] === "not:dmap:valid:enough?\n" &&
      rustStatus.files[".decisions/src/missing.ts.dmap"] === "1-3:fn:missing\n" &&
      rustStatus.files[".decisions/src/corrupt.ts.dmap"] === "1-3:fn:corrupt\n" &&
      tsSession.missingWrite.status === 0 &&
      rustSession.missingWrite.status === 0 &&
      tsSession.corruptWrite.status === 0 &&
      rustSession.corruptWrite.status === 0 &&
      tsSession.command.status === 0 &&
      rustSession.command.status === 0 &&
      tsSession.command.stdout === rustSession.command.stdout &&
      tsSession.command.stderr === rustSession.command.stderr &&
      tsSession.files[".decisions/src/missing.ts.dmap"] === null &&
      tsSession.files[".decisions/src/corrupt.ts.dmap"] === "not:dmap:valid:enough?\n" &&
      rustSession.files[".decisions/src/missing.ts.dmap"] === "1-3:fn:missing\n" &&
      rustSession.files[".decisions/src/corrupt.ts.dmap"] === "1-3:fn:corrupt\n"
    );
  },
  "Rust intentionally repairs missing or corrupt derivative .dmap files from valid .dlog source data during status and session-start while TypeScript leaves them absent or malformed."
));

results.push(await scenario("mcp-ghost-check", async (runtime) => {
  const root = await tempProject(runtime.name, "mcp-ghost-check");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function compute() {\n  return 1;\n}\n", "utf8");
  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/drift.ts",
      anchor: "fn:compute",
      lines: [1, 3],
      chose: "return one",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function compute() {\n  return 2;\n}\n", "utf8");
  const ghost = run(
    runtime,
    ["mcp"],
    JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "ghost_check", arguments: { file: "src/drift.ts" } }
    }),
    root
  );
  return {
    write,
    ghost: normalizeVolatile(ghost),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/drift.ts.dlog", ".decisions/src/drift.ts.dmap"]))
  };
}));

results.push(await knownImprovementScenario(
  "mcp-ghost-check-repairs-corrupt-dmap-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "mcp-ghost-check-repairs-corrupt-dmap-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/clean.ts"), "function clean() {\n  return 1;\n}\n", "utf8");
    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/clean.ts",
        anchor: "fn:clean",
        lines: [1, 3],
        chose: "clean derivative repair",
        because: "MCP dmap repair fixture",
        rejected: []
      })],
      "",
      root
    );
    await fs.writeFile(path.join(root, ".decisions/src/clean.ts.dmap"), "not:dmap:valid:enough?\n", "utf8");
    const ghost = run(
      runtime,
      ["mcp"],
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: { name: "ghost_check", arguments: { file: "src/clean.ts" } }
      }),
      root
    );
    return {
      write: normalizeVolatile(write),
      ghost: {
        status: ghost.status,
        responses: normalizeMcpResponses(ghost.stdout),
        stderr: ghost.stderr
      },
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/clean.ts.dlog",
        ".decisions/src/clean.ts.dmap",
        ".decisions/src/clean.ts.lock"
      ])),
      tempSiblings: await decisionTempSiblings(root, "src")
    };
  },
  (typescript, rust) => {
    const tsResponses = typescript.ghost.responses as Array<{ result?: { content?: Array<{ text?: string }> } }>;
    const rustResponses = rust.ghost.responses as Array<{ result?: { content?: Array<{ text?: string }> } }>;
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.write.status === 0 &&
      rust.write.status === 0 &&
      typescript.ghost.status === 0 &&
      rust.ghost.status === 0 &&
      tsResponses[0]?.result?.content?.[0]?.text === "No issues found for src/clean.ts." &&
      rustResponses[0]?.result?.content?.[0]?.text === "No issues found for src/clean.ts." &&
      tsFiles[".decisions/src/clean.ts.dlog"]?.includes("clean derivative repair") &&
      rustFiles[".decisions/src/clean.ts.dlog"]?.includes("clean derivative repair") &&
      tsFiles[".decisions/src/clean.ts.dmap"] === "not:dmap:valid:enough?\n" &&
      rustFiles[".decisions/src/clean.ts.dmap"] === "1-3:fn:clean\n" &&
      tsFiles[".decisions/src/clean.ts.lock"] === null &&
      rustFiles[".decisions/src/clean.ts.lock"] === null &&
      Array.isArray(typescript.tempSiblings) &&
      Array.isArray(rust.tempSiblings) &&
      typescript.tempSiblings.length === 0 &&
      rust.tempSiblings.length === 0
    );
  },
  "Rust intentionally repairs corrupt derivative .dmap files during file-scoped MCP ghost_check while TypeScript reports the clean result but leaves the derivative map malformed."
));

results.push(await knownImprovementScenario(
  "mcp-ghost-check-file-scoped-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "mcp-ghost-check-file-scoped-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/target.ts"), "export function target() {\n  return 1;\n}\n", "utf8");
    await fs.writeFile(path.join(root, "src/unrelated.ts"), "export function unrelated() {\n  return 1;\n}\n", "utf8");
    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/unrelated.ts",
        anchor: "fn:unrelated",
        lines: [1, 3],
        chose: "unrelated decision",
        because: "fixture",
        rejected: []
      })],
      "",
      root
    );
    await fs.writeFile(path.join(root, "src/unrelated.ts"), "export function unrelated() {\n  return 2;\n}\n", "utf8");
    const ghost = run(
      runtime,
      ["mcp"],
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: { name: "ghost_check", arguments: { file: "src/target.ts" } }
      }),
      root
    );
    return {
      write: normalizeVolatile(write),
      ghost: normalizeVolatile(ghost),
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/unrelated.ts.dlog",
        ".decisions/src/unrelated.ts.dmap"
      ]))
    };
  },
  (typescript, rust) => {
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.ghost.stdout.includes("No issues found for src/target.ts.") &&
      rust.ghost.stdout.includes("No issues found for src/target.ts.") &&
      tsFiles[".decisions/src/unrelated.ts.dlog"]?.includes("status: STALE") &&
      !rustFiles[".decisions/src/unrelated.ts.dlog"]?.includes("status: STALE")
    );
  },
  "Rust intentionally scopes MCP ghost_check to the requested file so it does not mutate unrelated stale dlogs like the TypeScript full-project lint path."
));

results.push(await scenario("lint-fix-orphan", async (runtime) => {
  const root = await tempProject(runtime.name, "lint-fix-orphan");
  await fs.mkdir(path.join(root, ".decisions/src"), { recursive: true });
  await fs.writeFile(
    path.join(root, ".decisions/src/missing.ts.dlog"),
    `file: src/missing.ts
schema: 1
decisions:
  fn:gone:
    id: dec_001
    lines_hint:
      - 1
      - 2
    fingerprint: deadbeef
    chose: missing source
    because: fixture
    rejected: []
    timestamp: '2026-06-26T20:31:18.340Z'
    history: []
`,
    "utf8"
  );
  const lint = run(runtime, ["lint", "--fix"], "", root);
  return {
    lint,
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/missing.ts.dlog", ".decisions/src/missing.ts.dmap"]))
  };
}));

results.push(await scenario("post-tool-use-shift", async (runtime) => {
  const root = await tempProject(runtime.name, "post-tool-use-shift");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/shift.ts"), "function kept() {\n  return 1;\n}\n", "utf8");
  git(root, ["init"]);
  git(root, ["add", "src/shift.ts"]);
  git(root, [
    "-c",
    "user.name=Archiva Test",
    "-c",
    "user.email=archiva@example.invalid",
    "commit",
    "-m",
    "initial"
  ]);

  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/shift.ts",
      anchor: "fn:kept",
      lines: [1, 3],
      chose: "keep function body",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );
  await fs.writeFile(path.join(root, "src/shift.ts"), "// inserted\nfunction kept() {\n  return 1;\n}\n", "utf8");
  const post = run(runtime, ["hooks", "post-tool-use", "src/shift.ts"], "", root);
  return {
    write,
    post: normalizeVolatile(post),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/shift.ts.dlog", ".decisions/src/shift.ts.dmap"]))
  };
}));

results.push(await scenario("post-tool-use-sha256-git", postToolUseSha256GitScenario));

results.push(await knownImprovementScenario(
  "post-tool-use-rename-improvement",
  async (runtime) => {
    const root = await tempProject(runtime.name, "post-tool-use-rename-improvement");
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/old.ts"), "function moved() {\n  return 1;\n}\n", "utf8");
    git(root, ["init"]);
    git(root, ["add", "src/old.ts"]);
    git(root, [
      "-c",
      "user.name=Archiva Test",
      "-c",
      "user.email=archiva@example.invalid",
      "commit",
      "-m",
      "initial rename"
    ]);

    const write = run(
      runtime,
      ["write-decision", "--json", JSON.stringify({
        file: "src/old.ts",
        anchor: "fn:moved",
        lines: [1, 3],
        chose: "preserve moved function",
        because: "rename fixture",
        rejected: []
      })],
      "",
      root
    );
    git(root, ["mv", "src/old.ts", "src/new.ts"]);
    const post = run(runtime, ["hooks", "post-tool-use", "src/new.ts"], "", root);
    return {
      write: normalizeVolatile(write),
      post: normalizeVolatile(post),
      files: normalizeFiles(await readProjectFiles(root, [
        ".decisions/src/old.ts.dlog",
        ".decisions/src/old.ts.dmap",
        ".decisions/src/new.ts.dlog",
        ".decisions/src/new.ts.dmap"
      ]))
    };
  },
  (typescript, rust) => {
    const tsFiles = typescript.files as Record<string, string | null>;
    const rustFiles = rust.files as Record<string, string | null>;
    return Boolean(
      typescript.post.stdout.includes("No decisions for src/new.ts") &&
      rust.post.stdout.includes("Re-anchored src/new.ts: 0 stale, 0 orphan.") &&
      tsFiles[".decisions/src/old.ts.dlog"]?.includes("file: src/old.ts") &&
      tsFiles[".decisions/src/new.ts.dlog"] === null &&
      rustFiles[".decisions/src/old.ts.dlog"] === null &&
      rustFiles[".decisions/src/new.ts.dlog"]?.includes("file: src/new.ts") &&
      rustFiles[".decisions/src/new.ts.dlog"]?.includes("preserve moved function")
    );
  },
  "Rust intentionally recovers git-renamed decision logs while the TypeScript oracle no-ops when the target path has no dlog."
));

results.push(await knownImprovementScenario(
  "post-tool-use-empty-env",
  async (runtime) => {
  const root = await tempProject(runtime.name, "post-tool-use-empty-env");
  return {
    command: run(runtime, ["hooks", "post-tool-use"], "", root, { ARCHIVA_FILE: "" })
  };
  },
  (typescript, rust) => {
    // With no positional path, no stdin payload, and an empty ARCHIVA_FILE,
    // both runtimes fail with a "missing file path" error and exit 1. Rust's
    // message additionally mentions the stdin hook payload path it now supports
    // (audit blocker B2); the observable contract (exit 1, empty stdout, an
    // error naming the missing file path) is identical.
    const ts = typescript.command;
    const rs = rust.command;
    return (
      ts.status === 1 &&
      rs.status === 1 &&
      ts.stdout === "" &&
      rs.stdout === "" &&
      /Missing file path/.test(ts.stderr) &&
      /Missing file path/.test(rs.stderr)
    );
  },
  "Rust `post-tool-use` now also reads the Claude Code hook payload from stdin (audit blocker B2), so its missing-file-path error additionally mentions the stdin path; the exit code, empty stdout, and missing-file-path error contract are otherwise identical to TypeScript."
));

results.push(await scenario("post-tool-use-orphan-return", async (runtime) => {
  const root = await tempProject(runtime.name, "post-tool-use-orphan-return");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const withBoth = "function kept() {\n  return 1;\n}\nfunction removed() {\n  return 2;\n}\n";
  await fs.writeFile(path.join(root, "src/orphan-return.ts"), withBoth, "utf8");
  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/orphan-return.ts",
      anchor: "fn:removed",
      lines: [4, 6],
      chose: "keep removed",
      because: "fixture",
      rejected: []
    })],
    "",
    root
  );

  await fs.writeFile(path.join(root, "src/orphan-return.ts"), "function kept() {\n  return 1;\n}\n", "utf8");
  const orphan = run(runtime, ["hooks", "post-tool-use", "src/orphan-return.ts"], "", root);
  await fs.writeFile(path.join(root, "src/orphan-return.ts"), withBoth, "utf8");
  const recovered = run(runtime, ["hooks", "post-tool-use", "src/orphan-return.ts"], "", root);

  return {
    write: normalizeVolatile(write),
    orphan: normalizeVolatile(orphan),
    recovered: normalizeVolatile(recovered),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/orphan-return.ts.dlog", ".decisions/src/orphan-return.ts.dmap"]))
  };
}));

results.push(await scenario("deterministic-mutation-stress", async (runtime) => {
  const root = await tempProject(runtime.name, "deterministic-mutation-stress");
  await fs.mkdir(path.join(root, "src"), { recursive: true });

  const fileCount = 4;
  const functionCount = 3;
  const sourceFiles = Array.from({ length: fileCount }, (_, fileIndex) => `src/stress-${fileIndex}.ts`);
  for (let fileIndex = 0; fileIndex < fileCount; fileIndex += 1) {
    await fs.writeFile(path.join(root, sourceFiles[fileIndex]), stressSource(fileIndex, functionCount), "utf8");
  }

  git(root, ["init"]);
  git(root, ["add", "src"]);
  git(root, [
    "-c",
    "user.name=Archiva Test",
    "-c",
    "user.email=archiva@example.invalid",
    "commit",
    "-m",
    "initial stress"
  ]);

  const writes: Record<string, CommandResult> = {};
  for (let fileIndex = 0; fileIndex < fileCount; fileIndex += 1) {
    for (let functionIndex = 0; functionIndex < functionCount; functionIndex += 1) {
      const lineStart = functionIndex * 6 + 1;
      const anchor = `fn:task_${fileIndex}_${functionIndex}`;
      writes[`${sourceFiles[fileIndex]}:${anchor}`] = normalizeVolatile(
        run(
          runtime,
          ["write-decision", "--json", JSON.stringify({
            file: sourceFiles[fileIndex],
            anchor,
            lines: [lineStart, lineStart + 5],
            chose: `stress decision ${fileIndex}.${functionIndex}`,
            because: "deterministic mutation stress fixture",
            rejected: [{ approach: "skip stress coverage", reason: "would not exercise bulk reanchor behavior" }]
          })],
          "",
          root
        )
      );
    }
  }

  for (let fileIndex = 0; fileIndex < fileCount; fileIndex += 1) {
    await fs.writeFile(
      path.join(root, sourceFiles[fileIndex]),
      mutatedStressSource(fileIndex, functionCount),
      "utf8"
    );
  }

  const postToolUse: Record<string, CommandResult> = {};
  for (const file of sourceFiles) {
    postToolUse[file] = normalizeVolatile(run(runtime, ["hooks", "post-tool-use", file], "", root));
  }

  const lint = normalizeVolatile(run(runtime, ["lint"], "", root));
  const status = normalizeVolatile(run(runtime, ["status"], "", root));
  const whyShifted = normalizeVolatile(run(runtime, ["why", "src/stress-0.ts", "fn:task_0_0"], "", root));
  const whyOrphan = normalizeVolatile(run(runtime, ["why", "src/stress-1.ts", "fn:task_1_2"], "", root));

  return {
    writes,
    postToolUse,
    lint,
    status,
    whyShifted,
    whyOrphan,
    files: normalizeFiles(await readProjectFiles(root, stressDecisionFiles(fileCount)))
  };
}));

async function postToolUseSha256GitScenario(runtime: Runtime): Promise<unknown> {
  const root = await tempProject(runtime.name, "post-tool-use-sha256-git");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/sha256.ts"), "function kept() {\n  return 1;\n}\n", "utf8");
  git(root, ["init", "--object-format=sha256"]);
  git(root, ["add", "src/sha256.ts"]);
  git(root, [
    "-c",
    "user.name=Archiva Test",
    "-c",
    "user.email=archiva@example.invalid",
    "commit",
    "-m",
    "initial sha256"
  ]);

  const write = run(
    runtime,
    ["write-decision", "--json", JSON.stringify({
      file: "src/sha256.ts",
      anchor: "fn:kept",
      lines: [1, 3],
      chose: "keep sha256 function body",
      because: "sha256 fixture",
      rejected: []
    })],
    "",
    root
  );
  await fs.writeFile(path.join(root, "src/sha256.ts"), "// inserted\nfunction kept() {\n  return 1;\n}\n", "utf8");
  const post = run(runtime, ["hooks", "post-tool-use", "src/sha256.ts"], "", root);
  return {
    write,
    post: normalizeVolatile(post),
    files: normalizeFiles(await readProjectFiles(root, [".decisions/src/sha256.ts.dlog", ".decisions/src/sha256.ts.dmap"]))
  };
}

const ok = results.every((result) => result.ok);
console.log(JSON.stringify({ tool: "archiva-differential", status: ok ? "passed" : "failed", results }, null, 2));
process.exit(ok ? 0 : 1);

async function scenario(name: string, runOne: (runtime: Runtime) => Promise<unknown>): Promise<ScenarioResult> {
  const [leftRuntime, rightRuntime] = runtimes;
  activeScenario = name;
  try {
    const left = await runOne(leftRuntime);
    const right = await runOne(rightRuntime);
    const ok = JSON.stringify(left) === JSON.stringify(right);
    return ok ? { name, ok } : { name, ok, details: { [leftRuntime.name]: left, [rightRuntime.name]: right } };
  } finally {
    activeScenario = "idle";
  }
}

async function knownImprovementScenario<T>(
  name: string,
  runOne: (runtime: Runtime) => Promise<T>,
  acceptsDivergence: (typescript: T, rust: T) => boolean,
  reason: string
): Promise<ScenarioResult> {
  const [leftRuntime, rightRuntime] = runtimes;
  activeScenario = name;
  try {
    const left = await runOne(leftRuntime);
    const right = await runOne(rightRuntime);
    const ok = acceptsDivergence(left, right);
    return ok
      ? { name, ok, details: { knownImprovement: reason } }
      : { name, ok, details: { knownImprovement: reason, [leftRuntime.name]: left, [rightRuntime.name]: right } };
  } finally {
    activeScenario = "idle";
  }
}

function run(runtime: Runtime, args: string[], input: string, cwd: string, extraEnv: Record<string, string> = {}): CommandResult {
  const result = spawnSync(runtime.command, [...runtime.prefixArgs, ...args], {
    cwd,
    input: input.length > 0 && !input.endsWith("\n") ? `${input}\n` : input,
    encoding: "utf8",
    env: { ...process.env, ARCHIVA_SESSION: "diff_session", ...extraEnv },
    timeout: commandTimeoutMs
  });
  const timeoutError =
    result.error && "code" in result.error && result.error.code === "ETIMEDOUT"
      ? `Command timed out after ${commandTimeoutMs}ms in scenario ${activeScenario}: ${runtime.name} ${[...runtime.prefixArgs, ...args].join(" ")}`
      : undefined;
  const stderr = result.stderr && result.stderr.length > 0 ? result.stderr : timeoutError ?? result.error?.message ?? "";
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr
  };
}

function runBinaryInput(runtime: Runtime, args: string[], input: Buffer, cwd: string, extraEnv: Record<string, string> = {}): CommandResult {
  const result = spawnSync(runtime.command, [...runtime.prefixArgs, ...args], {
    cwd,
    input,
    encoding: "utf8",
    env: { ...process.env, ARCHIVA_SESSION: "diff_session", ...extraEnv },
    timeout: commandTimeoutMs
  });
  const timeoutError =
    result.error && "code" in result.error && result.error.code === "ETIMEDOUT"
      ? `Command timed out after ${commandTimeoutMs}ms in scenario ${activeScenario}: ${runtime.name} ${[...runtime.prefixArgs, ...args].join(" ")}`
      : undefined;
  const stderr = result.stderr && result.stderr.length > 0 ? result.stderr : timeoutError ?? result.error?.message ?? "";
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr
  };
}

async function readProjectFiles(root: string, files: string[]): Promise<Record<string, string | null>> {
  const output: Record<string, string | null> = {};
  for (const file of files) {
    try {
      output[file] = await fs.readFile(path.join(root, file), "utf8");
    } catch {
      output[file] = null;
    }
  }
  return output;
}

function normalizeFiles(files: Record<string, string | null>): Record<string, string | null> {
  const output: Record<string, string | null> = {};
  for (const [file, content] of Object.entries(files)) {
    output[file] = content === null ? null : normalizeText(content);
  }
  return output;
}

function normalizeVolatile(result: CommandResult): CommandResult {
  return {
    ...result,
    stdout: normalizeText(result.stdout),
    stderr: normalizeText(result.stderr)
  };
}

function normalizeCorruptDlogFailure(result: CommandResult): Record<string, unknown> {
  return {
    status: result.status,
    stdout: result.stdout,
    stderrMentionsSchema: result.stderr.includes("schema")
  };
}

function normalizeMcpCorruptStorageResponses(stdout: string): Array<Record<string, unknown>> {
  return normalizeMcpResponses(stdout).map((response) => {
    const item = response as {
      id?: unknown;
      error?: {
        code?: unknown;
        message?: string;
      };
      result?: {
        tools?: Array<{ name?: unknown }>;
      };
    };
    if (item.error) {
      return {
        id: item.id ?? null,
        code: item.error.code ?? null,
        errorMentionsSchema: (item.error.message ?? "").includes("schema")
      };
    }
    return {
      id: item.id ?? null,
      toolNames: (item.result?.tools ?? []).map((tool) => tool.name)
    };
  });
}

function normalizeText(value: string): string {
  return value
    .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/g, "<timestamp>")
    .replace(/stale_since: '<timestamp>'/g, "stale_since: '<timestamp>'");
}

function countOccurrences(value: string, needle: string): number {
  if (needle.length === 0) {
    return 0;
  }
  let count = 0;
  let start = 0;
  while (true) {
    const index = value.indexOf(needle, start);
    if (index === -1) {
      return count;
    }
    count += 1;
    start = index + needle.length;
  }
}

function normalizeMcpResponses(stdout: string): unknown[] {
  return stdout
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      const response = JSON.parse(line) as {
        error?: { code?: number; message?: string };
      };
      if (response.error?.code === -32700) {
        response.error.message = "<parse-error>";
      }
      return response;
    });
}

// The Rust `why` MCP tool intentionally adds a `line` property and an updated
// description (audit blocker B12: line-based lookup that TypeScript silently
// dropped). This canonicalizes the `why` tool entry in any tools/list response
// so the shared surface (write_decision, ghost_check, and why's file/anchor
// props) is still compared exactly, while the documented B12 addition is
// accepted. Mutates in place and returns the same array for chaining.
function canonicalizeWhyToolImprovement(responses: unknown[]): unknown[] {
  for (const response of responses) {
    const tools = (response as { result?: { tools?: unknown[] } })?.result?.tools;
    if (!Array.isArray(tools)) continue;
    for (const tool of tools as Array<{
      name?: string;
      description?: string;
      inputSchema?: { properties?: Record<string, unknown> };
    }>) {
      if (tool?.name === "why") {
        if (tool.inputSchema?.properties) {
          delete tool.inputSchema.properties.line;
        }
        tool.description = "<why-description>";
      }
    }
  }
  return responses;
}

function mcpToolsListImprovementAccepts(
  typescript: { status: number | null; stdout: unknown[]; stderr: string },
  rust: { status: number | null; stdout: unknown[]; stderr: string }
): boolean {
  return (
    typescript.status === rust.status &&
    typescript.stderr === rust.stderr &&
    JSON.stringify(canonicalizeWhyToolImprovement(typescript.stdout)) ===
      JSON.stringify(canonicalizeWhyToolImprovement(rust.stdout))
  );
}


function firstMcpErrorMessage(stdout: string): string | null {
  for (const line of stdout.trim().split(/\r?\n/).filter(Boolean)) {
    const response = JSON.parse(line) as { error?: { message?: unknown } };
    if (typeof response.error?.message === "string") return response.error.message;
  }
  return null;
}

function normalizeMcpErrorSummaries(stdout: string): Array<Record<string, unknown>> {
  return normalizeMcpResponses(stdout).map((response) => {
    const item = response as {
      id?: unknown;
      error?: {
        code?: unknown;
        message?: string;
      };
    };
    const message = item.error?.message ?? "";
    return {
      id: item.id ?? null,
      code: item.error?.code ?? null,
      field: mcpErrorField(message),
      mentionsLineOrder: message.includes("lines end must be >= start")
    };
  });
}

function normalizeMcpEnvelopeResponses(stdout: string): Array<Record<string, unknown>> {
  return normalizeMcpResponses(stdout).map((response) => {
    const item = response as {
      id?: unknown;
      error?: { code?: unknown; message?: string };
      result?: {
        content?: Array<{ text?: string }>;
        tools?: Array<{ name?: string }>;
      };
    };
    const id = Object.prototype.hasOwnProperty.call(item, "id") ? item.id : "<missing>";
    if (item.error) {
      return {
        id,
        kind: "error",
        code: item.error.code ?? null,
        field: mcpEnvelopeErrorField(item.error.message ?? "")
      };
    }
    if (item.result?.tools) {
      return {
        id,
        kind: "tools",
        tools: item.result.tools.map((tool) => tool.name).sort()
      };
    }
    if (item.result?.content) {
      return {
        id,
        kind: "content",
        text: item.result.content.map((content) => content.text ?? "").join("\n")
      };
    }
    return { id, kind: "other" };
  });
}

function mcpEnvelopeErrorField(message: string): string | null {
  if (message.includes("\"file\"") || message.startsWith("file:")) {
    return "file";
  }
  if (message.includes("Expected object") || message.includes("expected object")) {
    return "arguments-object";
  }
  return mcpErrorField(message);
}

function mcpErrorField(message: string): string | null {
  if (message.startsWith("chose:") || message.includes("\"chose\"")) {
    return "chose";
  }
  if (message.startsWith("lines:") || message.includes("\"lines\"")) {
    return "lines";
  }
  if (message.startsWith("rejected.0.reason:") || (message.includes("\"rejected\"") && message.includes("\"reason\""))) {
    return "rejected.0.reason";
  }
  return null;
}

async function tempProject(runtime: string, scenarioName: string): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), `archiva-diff-${scenarioName}-${runtime}-`));
}

async function decisionTempSiblings(root: string, relativeDecisionDir: string): Promise<string[]> {
  const dir = path.join(root, ".decisions", relativeDecisionDir);
  try {
    const entries = await fs.readdir(dir);
    return entries.filter((entry) => entry.includes(".archiva-tmp-")).sort();
  } catch {
    return [];
  }
}

function stressSource(fileIndex: number, functionCount: number): string {
  let source = "";
  for (let functionIndex = 0; functionIndex < functionCount; functionIndex += 1) {
    source += stressFunction(fileIndex, functionIndex);
  }
  return source;
}

function mutatedStressSource(fileIndex: number, functionCount: number): string {
  let source = `// deterministic stress mutation ${fileIndex}\n`;
  for (let functionIndex = 0; functionIndex < functionCount; functionIndex += 1) {
    if (fileIndex % 2 === 1 && functionIndex === functionCount - 1) {
      continue;
    }
    if (functionIndex === 1) {
      source += `// inserted line before task_${fileIndex}_${functionIndex}\n`;
    }
    source += stressFunction(fileIndex, functionIndex);
  }
  return source;
}

function stressFunction(fileIndex: number, functionIndex: number): string {
  return `export function task_${fileIndex}_${functionIndex}(input: number) {
  if (input > ${functionIndex} && input < ${fileIndex + functionIndex + 10}) {
    return input + ${fileIndex + functionIndex};
  }
  return input - ${functionIndex};
}
`;
}

function stressDecisionFiles(fileCount: number): string[] {
  const files: string[] = [];
  for (let fileIndex = 0; fileIndex < fileCount; fileIndex += 1) {
    files.push(`.decisions/src/stress-${fileIndex}.ts.dlog`);
    files.push(`.decisions/src/stress-${fileIndex}.ts.dmap`);
  }
  return files;
}

function git(root: string, args: string[]): void {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
  }
}
