import { spawnSync } from "node:child_process";
import fsSync from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

type CommandResult = {
  status: number | null;
  stdout: string;
  stderr: string;
  timedOut: boolean;
};

type Runtime = {
  name: string;
  command: string;
  prefixArgs: string[];
};

type StressConfig = {
  files: number;
  functions: number;
  cycles: number;
};

const repoRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const rustBinInput = process.env.ARCHIVA_RUST_BIN ?? "target/release/archiva";
const rustBin = path.isAbsolute(rustBinInput) ? rustBinInput : path.resolve(repoRoot, rustBinInput);
const commandTimeoutMs = positiveIntEnv("ARCHIVA_STRESS_COMMAND_TIMEOUT_MS", 120_000);
const config: StressConfig = {
  files: positiveIntEnv("ARCHIVA_STRESS_FILES", 8),
  functions: positiveIntEnv("ARCHIVA_STRESS_FUNCTIONS", 5),
  cycles: positiveIntEnv("ARCHIVA_STRESS_CYCLES", 20)
};

await fs.access(rustBin).catch(() => {
  throw new Error(`Missing Rust binary at ${rustBin}. Run npm run build:rust or set ARCHIVA_RUST_BIN.`);
});

const runtimes: [Runtime, Runtime] = [
  { name: "typescript", command: process.execPath, prefixArgs: [path.join(repoRoot, "bin", "archiva.js")] },
  { name: "rust", command: rustBin, prefixArgs: [] }
];

const [typescriptResult, rustResult] = await Promise.all(runtimes.map((runtime) => runStress(runtime, config)));
const comparison = compareStressResults(typescriptResult.result, rustResult.result);
const ok = comparison.ok;

console.log(
  JSON.stringify(
    {
      tool: "archiva-stress-rust-port",
      status: ok ? "passed" : "failed",
      config,
      commandTimeoutMs,
      runtimes: {
        typescript: typescriptResult.root,
        rust: rustResult.root
      },
      details: ok
        ? {
            cycles: config.cycles,
            files: config.files,
            functions: config.functions,
            comparison: comparison.summary
          }
        : {
            comparison,
            typescript: typescriptResult.result,
            rust: rustResult.result
          }
    },
    null,
    2
  )
);
process.exit(ok ? 0 : 1);

async function runStress(runtime: Runtime, stressConfig: StressConfig): Promise<{ root: string; result: StressResult }> {
  const root = await tempProject(runtime.name, "rust-port-stress");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const sourceFiles = sourceFileNames(stressConfig.files);

  for (let fileIndex = 0; fileIndex < stressConfig.files; fileIndex += 1) {
    await fs.writeFile(
      path.join(root, sourceFiles[fileIndex]),
      renderSource(fileIndex, stressConfig.functions, 0),
      "utf8"
    );
  }

  git(root, ["init"]);
  git(root, ["add", "src"]);
  git(root, [
    "-c",
    "user.name=Archiva Stress",
    "-c",
    "user.email=archiva@example.invalid",
    "commit",
    "-m",
    "initial stress"
  ]);

  const initialWrites: Record<string, CommandResult> = {};
  for (let fileIndex = 0; fileIndex < stressConfig.files; fileIndex += 1) {
    const source = await fs.readFile(path.join(root, sourceFiles[fileIndex]), "utf8");
    for (let functionIndex = 0; functionIndex < stressConfig.functions; functionIndex += 1) {
      const anchor = taskAnchor(fileIndex, functionIndex);
      const range = lineRangeFor(source, taskName(fileIndex, functionIndex));
      initialWrites[`${sourceFiles[fileIndex]}:${anchor}`] = normalizeVolatile(
        run(
          runtime,
          ["write-decision", "--json", JSON.stringify({
            file: sourceFiles[fileIndex],
            anchor,
            lines: range,
            chose: `initial stress decision ${fileIndex}.${functionIndex}`,
            because: "seed deterministic stress baseline",
            rejected: [{ approach: "skip baseline write", reason: "stress needs persistent decisions to mutate" }]
          })],
          "",
          root
        )
      );
    }
  }

  const cycles: StressCycle[] = [];
  let superseded = false;
  for (let cycle = 1; cycle <= stressConfig.cycles; cycle += 1) {
    for (let fileIndex = 0; fileIndex < stressConfig.files; fileIndex += 1) {
      await fs.writeFile(
        path.join(root, sourceFiles[fileIndex]),
        renderSource(fileIndex, stressConfig.functions, cycle),
        "utf8"
      );
    }

    const postToolUse: Record<string, CommandResult> = {};
    for (const file of sourceFiles) {
      postToolUse[file] = normalizeVolatile(run(runtime, ["hooks", "post-tool-use", file], "", root));
    }

    let supersede: CommandResult | null = null;
    if (!superseded && cycle >= 2 && !omitsFunction(0, 0, cycle)) {
      const file = sourceFiles[0];
      const source = await fs.readFile(path.join(root, file), "utf8");
      supersede = normalizeVolatile(
        run(
          runtime,
          ["write-decision", "--json", JSON.stringify({
            file,
            anchor: taskAnchor(0, 0),
            lines: lineRangeFor(source, taskName(0, 0)),
            supersedes: "dec_001",
            chose: "superseded stress decision",
            because: "stress exercises history carry-forward after mutation",
            rejected: [{ approach: "leave original decision", reason: "would not cover supersession under stress" }]
          })],
          "",
          root
        )
      );
      superseded = true;
    }

    const lint = normalizeVolatile(run(runtime, ["lint"], "", root));
    const status = normalizeVolatile(run(runtime, ["status"], "", root));
    const whyStable = normalizeVolatile(run(runtime, ["why", sourceFiles[0], taskAnchor(0, 0)], "", root));
    const orphanFileIndex = Math.min(1, stressConfig.files - 1);
    const orphanFunctionIndex = Math.min(stressConfig.functions - 1, (cycle + orphanFileIndex) % stressConfig.functions);
    const whyMaybeOrphan = normalizeVolatile(
      run(runtime, ["why", sourceFiles[orphanFileIndex], taskAnchor(orphanFileIndex, orphanFunctionIndex)], "", root)
    );
    const ghostCheck =
      cycle % 5 === 0 || cycle === stressConfig.cycles
        ? normalizeVolatile(
            run(
              runtime,
              ["mcp"],
              JSON.stringify({
                jsonrpc: "2.0",
                id: cycle,
                method: "tools/call",
                params: { name: "ghost_check", arguments: { file: sourceFiles[0] } }
              }),
              root
            )
          )
        : null;

    cycles.push({
      cycle,
      postToolUse,
      supersede,
      lint,
      status,
      whyStable,
      whyMaybeOrphan,
      ghostCheck
    });
  }

  const result: StressResult = {
    initialWrites,
    cycles,
    files: normalizeFiles(await readProjectFiles(root, decisionFiles(stressConfig.files))),
    residue: countDecisionResidue(root)
  };
  assertStressInvariants(runtime.name, stressConfig, result);
  return { root, result };
}

function renderSource(fileIndex: number, functions: number, cycle: number): string {
  const lines: string[] = [];
  if (cycle > 0) {
    lines.push(`// deterministic stress cycle ${cycle} file ${fileIndex}`);
    for (let index = 0; index < (cycle + fileIndex) % 3; index += 1) {
      lines.push(`// inserted ${cycle}.${fileIndex}.${index}`);
    }
  }
  for (let functionIndex = 0; functionIndex < functions; functionIndex += 1) {
    if (omitsFunction(fileIndex, functionIndex, cycle)) {
      continue;
    }
    if (cycle > 0 && (cycle + fileIndex + functionIndex) % 4 === 0) {
      lines.push(`// shifted before ${taskName(fileIndex, functionIndex)}`);
    }
    lines.push(...renderFunctionLines(fileIndex, functionIndex, cycle));
  }
  return `${lines.join("\n")}\n`;
}

function renderFunctionLines(fileIndex: number, functionIndex: number, cycle: number): string[] {
  const changed = cycle > 0 && (cycle + fileIndex + functionIndex) % 6 === 0;
  const addition = fileIndex + functionIndex + (changed ? cycle : 0);
  return [
    `export function ${taskName(fileIndex, functionIndex)}(input: number) {`,
    `  if (input > ${functionIndex} && input < ${fileIndex + functionIndex + 10}) {`,
    `    return input + ${addition};`,
    "  }",
    `  return input - ${functionIndex};`,
    "}"
  ];
}

function omitsFunction(fileIndex: number, functionIndex: number, cycle: number): boolean {
  return cycle > 0 && cycle % 4 === 1 && (cycle + fileIndex + functionIndex) % 5 === 0;
}

function sourceFileNames(files: number): string[] {
  return Array.from({ length: files }, (_, index) => `src/stress-${index}.ts`);
}

function decisionFiles(files: number): string[] {
  return sourceFileNames(files).flatMap((file) => [`.decisions/${file}.dlog`, `.decisions/${file}.dmap`]);
}

function taskName(fileIndex: number, functionIndex: number): string {
  return `task_${fileIndex}_${functionIndex}`;
}

function taskAnchor(fileIndex: number, functionIndex: number): string {
  return `fn:${taskName(fileIndex, functionIndex)}`;
}

function lineRangeFor(source: string, functionName: string): [number, number] {
  const lines = source.split(/\n/);
  const start = lines.findIndex((line) => line.startsWith(`export function ${functionName}(`));
  if (start < 0) {
    throw new Error(`Missing function ${functionName}`);
  }
  return [start + 1, start + 6];
}

function run(runtime: Runtime, args: string[], input: string, cwd: string): CommandResult {
  const result = spawnSync(runtime.command, [...runtime.prefixArgs, ...args], {
    cwd,
    input: input.length > 0 && !input.endsWith("\n") ? `${input}\n` : input,
    encoding: "utf8",
    timeout: commandTimeoutMs,
    env: { ...process.env, ARCHIVA_SESSION: "stress_session" }
  });
  const timedOut = result.error?.message.includes("ETIMEDOUT") ?? false;
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: `${result.stderr ?? ""}${result.error?.message ?? ""}`,
    timedOut
  };
}

type StressCycle = {
  cycle: number;
  postToolUse: Record<string, CommandResult>;
  supersede: CommandResult | null;
  lint: CommandResult;
  status: CommandResult;
  whyStable: CommandResult;
  whyMaybeOrphan: CommandResult;
  ghostCheck: CommandResult | null;
};

type StressResult = {
  initialWrites: Record<string, CommandResult>;
  cycles: StressCycle[];
  files: Record<string, string | null>;
  residue: { lockArtifacts: number; tempArtifacts: number };
};

function assertStressInvariants(runtime: string, stressConfig: StressConfig, result: StressResult): void {
  assertCommands(runtime, "initial write", Object.values(result.initialWrites), [0]);
  if (Object.keys(result.initialWrites).length !== stressConfig.files * stressConfig.functions) {
    throw new Error(`${runtime} stress wrote ${Object.keys(result.initialWrites).length} initial decisions.`);
  }
  if (result.cycles.length !== stressConfig.cycles) {
    throw new Error(`${runtime} stress ran ${result.cycles.length} cycles, expected ${stressConfig.cycles}.`);
  }

  let supersedeCount = 0;
  let ghostCheckCount = 0;
  for (const cycle of result.cycles) {
    assertCommands(runtime, `cycle ${cycle.cycle} post-tool-use`, Object.values(cycle.postToolUse), [0]);
    assertCommands(runtime, `cycle ${cycle.cycle} lint`, [cycle.lint], [0, 1]);
    assertCommands(runtime, `cycle ${cycle.cycle} status`, [cycle.status], [0]);
    assertCommands(runtime, `cycle ${cycle.cycle} why stable`, [cycle.whyStable], [0]);
    assertCommands(runtime, `cycle ${cycle.cycle} why maybe orphan`, [cycle.whyMaybeOrphan], [0]);
    if (cycle.supersede) {
      supersedeCount += 1;
      assertCommands(runtime, `cycle ${cycle.cycle} supersede`, [cycle.supersede], [0]);
    }
    if (cycle.ghostCheck) {
      ghostCheckCount += 1;
      assertCommands(runtime, `cycle ${cycle.cycle} ghost_check`, [cycle.ghostCheck], [0]);
      if (!cycle.ghostCheck.stdout.includes("\"jsonrpc\":\"2.0\"")) {
        throw new Error(`${runtime} stress cycle ${cycle.cycle} ghost_check did not return JSON-RPC output.`);
      }
    }
  }

  if (supersedeCount !== 1) {
    throw new Error(`${runtime} stress superseded ${supersedeCount} decisions, expected 1.`);
  }
  if (ghostCheckCount === 0) {
    throw new Error(`${runtime} stress did not run any MCP ghost_check cycles.`);
  }

  const contents = Object.values(result.files);
  if (contents.some((content) => content === null)) {
    throw new Error(`${runtime} stress did not produce every expected dlog/dmap file.`);
  }
  const joined = contents.join("\n");
  const lifecycleOutputs = result.cycles.flatMap((cycle) => [
    cycle.lint.stdout,
    cycle.status.stdout,
    cycle.whyStable.stdout,
    cycle.whyMaybeOrphan.stdout,
    cycle.ghostCheck?.stdout ?? ""
  ]);
  const lifecycle = [joined, ...lifecycleOutputs].join("\n");
  if (!hasPositiveLifecycleCount(lifecycleOutputs, "stale")) {
    throw new Error(`${runtime} stress lifecycle did not observe a positive STALE count.`);
  }
  if (!hasPositiveLifecycleCount(lifecycleOutputs, "orphan") && !joined.includes(":ORPHAN")) {
    throw new Error(`${runtime} stress lifecycle did not observe a positive ORPHAN count.`);
  }
  if (!joined.includes("superseded stress decision")) {
    throw new Error(`${runtime} stress final decision files do not include superseded history.`);
  }
  if (result.residue.lockArtifacts !== 0 || result.residue.tempArtifacts !== 0) {
    throw new Error(
      `${runtime} stress left decision residue: locks=${result.residue.lockArtifacts} temp=${result.residue.tempArtifacts}`
    );
  }
}

function hasPositiveLifecycleCount(outputs: string[], label: "stale" | "orphan"): boolean {
  const pattern = new RegExp(`\\b([1-9][0-9]*)\\s+${label}\\b`, "i");
  return outputs.some((output) => pattern.test(output));
}

type StressComparison = {
  ok: boolean;
  summary: string;
  differences: string[];
};

function compareStressResults(left: StressResult, right: StressResult): StressComparison {
  const differences: string[] = [];
  compareRecordKeys("initial writes", left.initialWrites, right.initialWrites, differences);
  for (const key of sortedSharedKeys(left.initialWrites, right.initialWrites)) {
    compareCommandShape(`initial write ${key}`, left.initialWrites[key], right.initialWrites[key], differences);
  }

  if (left.cycles.length !== right.cycles.length) {
    differences.push(`cycle count differs: ${left.cycles.length} !== ${right.cycles.length}`);
  }
  const cycleCount = Math.min(left.cycles.length, right.cycles.length);
  for (let index = 0; index < cycleCount; index += 1) {
    const leftCycle = left.cycles[index];
    const rightCycle = right.cycles[index];
    const label = `cycle ${index + 1}`;
    if (leftCycle.cycle !== rightCycle.cycle) {
      differences.push(`${label} number differs: ${leftCycle.cycle} !== ${rightCycle.cycle}`);
    }
    compareRecordKeys(`${label} post-tool-use`, leftCycle.postToolUse, rightCycle.postToolUse, differences);
    for (const key of sortedSharedKeys(leftCycle.postToolUse, rightCycle.postToolUse)) {
      compareCommandShape(
        `${label} post-tool-use ${key}`,
        leftCycle.postToolUse[key],
        rightCycle.postToolUse[key],
        differences
      );
    }
    compareOptionalCommand(`${label} supersede`, leftCycle.supersede, rightCycle.supersede, differences);
    compareCommandShape(`${label} lint`, leftCycle.lint, rightCycle.lint, differences);
    compareCommandShape(`${label} status`, leftCycle.status, rightCycle.status, differences);
    compareCommandShape(`${label} why stable`, leftCycle.whyStable, rightCycle.whyStable, differences);
    compareCommandShape(`${label} why maybe orphan`, leftCycle.whyMaybeOrphan, rightCycle.whyMaybeOrphan, differences);
    compareOptionalCommand(`${label} ghost_check`, leftCycle.ghostCheck, rightCycle.ghostCheck, differences);
  }

  compareRecordKeys("final decision files", left.files, right.files, differences);
  if (left.residue.lockArtifacts !== right.residue.lockArtifacts) {
    differences.push(
      `lock residue differs: ${left.residue.lockArtifacts} !== ${right.residue.lockArtifacts}`
    );
  }
  if (left.residue.tempArtifacts !== right.residue.tempArtifacts) {
    differences.push(
      `temp residue differs: ${left.residue.tempArtifacts} !== ${right.residue.tempArtifacts}`
    );
  }

  return {
    ok: differences.length === 0,
    summary:
      "compared command status, timeout, cycle shape, file coverage, and cleanup residue; final dlog/dmap bytes may differ for documented Rust reanchor hardening",
    differences
  };
}

function compareOptionalCommand(
  label: string,
  left: CommandResult | null,
  right: CommandResult | null,
  differences: string[]
): void {
  if ((left === null) !== (right === null)) {
    differences.push(`${label} presence differs`);
    return;
  }
  if (left && right) {
    compareCommandShape(label, left, right, differences);
  }
}

function compareCommandShape(
  label: string,
  left: CommandResult,
  right: CommandResult,
  differences: string[]
): void {
  if (left.status !== right.status) {
    differences.push(`${label} status differs: ${left.status} !== ${right.status}`);
  }
  if (left.timedOut !== right.timedOut) {
    differences.push(`${label} timeout differs: ${left.timedOut} !== ${right.timedOut}`);
  }
}

function compareRecordKeys(
  label: string,
  left: Record<string, unknown>,
  right: Record<string, unknown>,
  differences: string[]
): void {
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(right).sort();
  if (leftKeys.join("\n") !== rightKeys.join("\n")) {
    differences.push(`${label} keys differ`);
  }
}

function sortedSharedKeys<T>(
  left: Record<string, T>,
  right: Record<string, T>
): string[] {
  const rightKeys = new Set(Object.keys(right));
  return Object.keys(left).filter((key) => rightKeys.has(key)).sort();
}

function assertCommands(runtime: string, phase: string, commands: CommandResult[], allowedStatuses: number[]): void {
  for (const command of commands) {
    if (command.timedOut) {
      throw new Error(`${runtime} ${phase} timed out after ${commandTimeoutMs}ms.`);
    }
    if (!allowedStatuses.includes(command.status ?? -1)) {
      throw new Error(`${runtime} ${phase} exited ${command.status}; expected ${allowedStatuses.join(" or ")}.`);
    }
  }
}

function countDecisionResidue(root: string): { lockArtifacts: number; tempArtifacts: number } {
  const decisionRoot = path.join(root, ".decisions");
  const summary = { lockArtifacts: 0, tempArtifacts: 0 };
  const pending = [decisionRoot];
  while (pending.length > 0) {
    const current = pending.pop()!;
    let entries: fsSync.Dirent[];
    try {
      entries = fsSync.readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const name = entry.name;
      if (entry.isDirectory()) {
        pending.push(path.join(current, name));
      } else {
        if (name.endsWith(".lock")) {
          summary.lockArtifacts += 1;
        }
        if (name.includes(".archiva-tmp-") || name.endsWith(".tmp")) {
          summary.tempArtifacts += 1;
        }
      }
    }
  }
  return summary;
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

function normalizeText(value: string): string {
  return value
    .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/g, "<timestamp>")
    .replace(/stale_since: '<timestamp>'/g, "stale_since: '<timestamp>'");
}

async function tempProject(runtime: string, scenarioName: string): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), `archiva-${scenarioName}-${runtime}-`));
}

function git(root: string, args: string[]): void {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
  }
}

function positiveIntEnv(name: string, fallback: number): number {
  const value = process.env[name];
  if (!value) {
    return fallback;
  }
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}
