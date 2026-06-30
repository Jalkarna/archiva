import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { extractAnchors } from "../src/core/anchor.js";
import { writeDecision, why } from "../src/core/decision.js";
import { applyDiffToRange, postToolUse } from "../src/core/reanchor.js";
import { lintProject } from "../src/lint/rules.js";

type BenchResult = {
  name: string;
  iterations: number;
  totalMs: number;
  meanMs: number;
  details?: Record<string, unknown>;
};

const repoRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const scale = Number(process.env.ARCHIVA_BASELINE_SCALE ?? "1");
const commandTimeoutMs = positiveIntEnv(
  "ARCHIVA_BASELINE_COMMAND_TIMEOUT_MS",
  positiveIntEnv("ARCHIVA_BENCHMARK_COMMAND_TIMEOUT_MS", 300_000)
);

const results: BenchResult[] = [];

await bench("startup.version", 10, () => {
	const result = spawnSync(process.execPath, [path.join(repoRoot, "bin/archiva.js"), "--version"], {
	  cwd: repoRoot,
	  encoding: "utf8",
	  timeout: commandTimeoutMs,
	  killSignal: "SIGKILL"
	});
	if (result.status !== 0) {
	  throwCommandError(
	    "node",
	    [path.join(repoRoot, "bin/archiva.js"), "--version"],
	    result.status,
	    result.signal,
	    result.stdout ?? "",
	    result.stderr ?? "",
	    result.error
	  );
	}
});

await bench("decision.write", 25 * scale, async (iteration) => {
  const root = await tempProject(`write-${iteration}`);
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  await writeDecision(root, {
    file: "src/a.ts",
    anchor: "fn:makeThing",
    lines: [1, 3],
    chose: "baseline write",
    because: "measure decision write path",
    rejected: []
  });
});

await bench("decision.write.cli", 25 * scale, async (iteration) => {
  const root = await tempProject(`write-cli-${iteration}`);
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  runCli(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline write cli",
        because: "measure TypeScript CLI decision write path",
        rejected: []
      })
    ],
    "",
    root
  );
});

{
  const root = await tempProject("why");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  await writeDecision(root, {
    file: "src/a.ts",
    anchor: "fn:makeThing",
    lines: [1, 3],
    chose: "baseline why",
    because: "measure decision read path",
    rejected: []
  });
  await bench("decision.why", 100 * scale, async () => {
    await why(root, "src/a.ts", "fn:makeThing");
  });
}

{
  const root = await tempProject("why-cli");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  runCli(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline why cli",
        because: "measure TypeScript CLI decision read path",
        rejected: []
      })
    ],
    "",
    root
  );
  await bench("decision.why.cli", 100 * scale, () => {
    runCli(["why", "src/a.ts", "fn:makeThing"], "", root);
  });
}

{
  const source = makeSource(200 * scale);
  await bench("anchor.extract", 50, () => {
    extractAnchors("src/large.ts", source);
  }, { lines: source.split(/\r?\n/).length });
}

{
  const oldContent = Array.from({ length: 1000 * scale }, (_, index) => `line ${index}`).join("\n");
  const newContent = `${Array.from({ length: 500 * scale }, (_, index) => `line ${index}`).join("\n")}\ninserted\n${Array.from(
    { length: 500 * scale },
    (_, index) => `line ${index + 500 * scale}`
  ).join("\n")}`;
  await bench("reanchor.applyDiffToRange", 100, () => {
    applyDiffToRange(oldContent, newContent, [750 * scale, 900 * scale]);
  }, { oldLines: 1000 * scale });
}

{
  const root = await tempProject("post-tool-use-git");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  git(root, ["init"]);
  git(root, ["add", "src/a.ts"]);
  git(root, ["-c", "user.name=Archiva Baseline", "-c", "user.email=archiva@example.invalid", "commit", "-m", "initial"]);
  await writeDecision(root, {
    file: "src/a.ts",
    anchor: "fn:makeThing",
    lines: [1, 3],
    chose: "baseline reanchor",
    because: "measure postToolUse with git HEAD",
    rejected: []
  });
  await bench("reanchor.postToolUse.git", 25 * scale, async (iteration) => {
    await fs.writeFile(
      path.join(root, "src/a.ts"),
      `${Array.from({ length: iteration + 1 }, (_, index) => `// inserted ${index}`).join("\n")}\nexport function makeThing() {\n  return 1;\n}\n`,
      "utf8"
    );
    await postToolUse(root, "src/a.ts");
  }, { iterationsMutateSameRepo: true });
}

{
  const root = await tempProject("post-tool-use-git-cli");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  git(root, ["init"]);
  git(root, ["add", "src/a.ts"]);
  git(root, ["-c", "user.name=Archiva Baseline", "-c", "user.email=archiva@example.invalid", "commit", "-m", "initial"]);
  runCli(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline reanchor cli",
        because: "measure TypeScript CLI post-tool-use with git HEAD",
        rejected: []
      })
    ],
    "",
    root
  );
  await bench("reanchor.postToolUse.git.cli", 25 * scale, async (iteration) => {
    await fs.writeFile(
      path.join(root, "src/a.ts"),
      `${Array.from({ length: iteration + 1 }, (_, index) => `// inserted ${index}`).join("\n")}\nexport function makeThing() {\n  return 1;\n}\n`,
      "utf8"
    );
    runCli(["hooks", "post-tool-use", "src/a.ts"], "", root);
  }, { iterationsMutateSameRepo: true });
}

{
  const root = await tempProject("lint");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const files = 50 * scale;
  for (let index = 0; index < files; index += 1) {
    await fs.writeFile(path.join(root, "src", `file-${index}.ts`), makeSource(20), "utf8");
  }
  await bench("lint.clean-scan", 5, async () => {
    await lintProject(root);
  }, { files });
}

{
  const root = await tempProject("lint-cli");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const files = 50 * scale;
  for (let index = 0; index < files; index += 1) {
    await fs.writeFile(path.join(root, "src", `file-${index}.ts`), makeSource(20), "utf8");
  }
  await bench("lint.clean-scan.cli", 5, () => {
    runCli(["lint"], "", root);
  }, { files });
}

{
  const root = await tempProject("status-cli");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const files = 10 * scale;
  for (let index = 0; index < files; index += 1) {
    const file = `src/file-${index}.ts`;
    await fs.writeFile(path.join(root, file), `export function fn${index}() {\n  return ${index};\n}\n`, "utf8");
    runCli(
      [
        "write-decision",
        "--json",
        JSON.stringify({
          file,
          anchor: `fn:fn${index}`,
          lines: [1, 3],
          chose: "baseline status cli",
          because: "measure TypeScript CLI status over decisions",
          rejected: []
        })
      ],
      "",
      root
    );
  }
  await bench("status.cli", 25 * scale, () => {
    runCli(["status"], "", root);
  }, { files });
}

{
  const root = await tempProject("mcp-ghost-check-cli");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function drift() {\n  return 1;\n}\n", "utf8");
  runCli(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/drift.ts",
        anchor: "fn:drift",
        lines: [1, 3],
        chose: "baseline mcp ghost_check",
        because: "measure TypeScript MCP ghost_check path",
        rejected: []
      })
    ],
    "",
    root
  );
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function drift() {\n  return 2;\n}\n", "utf8");
  const request = `${JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "tools/call",
    params: { name: "ghost_check", arguments: { file: "src/drift.ts" } }
  })}\n`;
  await bench("mcp.ghost_check.cli", 25 * scale, () => {
    runCli(["mcp"], request, root);
  });
}

console.log(
  JSON.stringify(
    {
      tool: "archiva-ts-baseline",
      version: JSON.parse(await fs.readFile(path.join(repoRoot, "package.json"), "utf8")).version,
      node: process.version,
	      platform: process.platform,
	      arch: process.arch,
	      scale,
	      commandTimeoutMs,
	      results
    },
    null,
    2
  )
);

async function bench(
  name: string,
  iterations: number,
  fn: (iteration: number) => void | Promise<void>,
  details?: Record<string, unknown>
): Promise<void> {
  const start = performance.now();
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    await fn(iteration);
  }
  const totalMs = performance.now() - start;
  results.push({
    name,
    iterations,
    totalMs: round(totalMs),
    meanMs: round(totalMs / iterations),
    details
  });
}

function makeSource(functions: number): string {
  return Array.from({ length: functions }, (_, index) => {
    return `export function fn${index}(a: boolean, b: boolean) {\n  if (a && b) return ${index};\n  return ${index + 1};\n}`;
  }).join("\n\n");
}

function runCli(args: string[], input: string, cwd: string): void {
	const result = spawnSync(process.execPath, [path.join(repoRoot, "bin", "archiva.js"), ...args], {
	  cwd,
	  input,
	  encoding: "utf8",
	  timeout: commandTimeoutMs,
	  killSignal: "SIGKILL",
	  env: { ...process.env, ARCHIVA_SESSION: "ts_baseline_session" }
	});
	if (result.status !== 0) {
	  throwCommandError("node bin/archiva.js", args, result.status, result.signal, result.stdout ?? "", result.stderr ?? "", result.error);
	}
}

function throwCommandError(
  command: string,
  args: string[],
  status: number | null,
  signal: NodeJS.Signals | null,
  stdout: string,
  stderr: string,
  error: Error | undefined
): never {
  if (isTimeoutError(error) || status === 124) {
	  throw new Error(
	    `${command} ${args.join(" ")} timed out after ${commandTimeoutMs}ms: ${formatFailure(status, signal, stdout, stderr, error)}`
	  );
	}
  throw new Error(`${command} ${args.join(" ")} failed: ${formatFailure(status, signal, stdout, stderr, error)}`);
}

function isTimeoutError(error: Error | undefined): boolean {
  const code = (error as NodeJS.ErrnoException | undefined)?.code;
  return code === "ETIMEDOUT" || error?.message.includes("ETIMEDOUT") === true;
}

function formatFailure(
  status: number | null,
  signal: NodeJS.Signals | null,
  stdout: string,
  stderr: string,
  error: Error | undefined
): string {
  const code = (error as NodeJS.ErrnoException | undefined)?.code;
  return [
    `status=${status}`,
    `signal=${signal ?? "none"}`,
    code ? `errorCode=${code}` : undefined,
    error?.message ? `error=${error.message}` : undefined,
    stderr ? `stderr=${tail(stderr)}` : undefined,
    stdout ? `stdout=${tail(stdout)}` : undefined
  ]
    .filter((part): part is string => part !== undefined)
    .join(" ");
}

function tail(value: string): string {
  const normalized = value.trim();
  return JSON.stringify(normalized.length > 2000 ? normalized.slice(-2000) : normalized);
}

async function tempProject(name: string): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), `archiva-ts-baseline-${name}-`));
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}

function git(cwd: string, args: string[]): void {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", timeout: commandTimeoutMs, killSignal: "SIGKILL" });
  if (result.status !== 0) {
    throwCommandError("git", args, result.status, result.signal, result.stdout ?? "", result.stderr ?? "", result.error);
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
