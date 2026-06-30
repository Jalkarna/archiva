import { spawnSync } from "node:child_process";
import fsSync from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

type BenchResult = {
  name: string;
  iterations: number;
  totalMs: number;
  meanMs: number;
  peakRss?: PeakRss;
  details?: Record<string, unknown>;
};

type PeakRss =
  | { status: "measured"; peakRssKb: number }
  | { status: "unavailable"; reason: string };

const repoRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const rustBin = process.env.ARCHIVA_RUST_BIN ?? path.join(repoRoot, "target", "debug", "archiva");
const scale = Number(process.env.ARCHIVA_BASELINE_SCALE ?? "1");
const commandTimeoutMs = positiveIntEnv(
  "ARCHIVA_BASELINE_COMMAND_TIMEOUT_MS",
  positiveIntEnv("ARCHIVA_BENCHMARK_COMMAND_TIMEOUT_MS", 300_000)
);
const results: BenchResult[] = [];
let rssFileCounter = 0;
const processTimeoutTool = detectProcessTimeoutTool();
const rssTool = detectRssTool();

await fs.access(rustBin);

await bench(
  "startup.version",
  25,
  () => {
    run(["--version"], "", repoRoot);
  },
  undefined,
  () => runMeasured(["--version"], "", repoRoot)
);

await bench("decision.write.cli", 25 * scale, async (iteration) => {
  const root = await tempProject(`write-${iteration}`);
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  run(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline write",
        because: "measure native decision write path",
        rejected: []
      })
    ],
    "",
    root
  );
}, undefined, async () => {
  const root = await tempProject("write-rss");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  return runMeasured(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline write rss",
        because: "measure native decision write memory",
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
  run(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline why",
        because: "measure native decision read path",
        rejected: []
      })
    ],
    "",
    root
  );
  await bench("decision.why.cli", 100 * scale, () => {
    run(["why", "src/a.ts", "fn:makeThing"], "", root);
  }, undefined, () => runMeasured(["why", "src/a.ts", "fn:makeThing"], "", root));
}

{
  const root = await tempProject("post-tool-use-git");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/a.ts"), "export function makeThing() {\n  return 1;\n}\n", "utf8");
  git(root, ["init"]);
  git(root, ["add", "src/a.ts"]);
  git(root, ["-c", "user.name=Archiva Baseline", "-c", "user.email=archiva@example.invalid", "commit", "-m", "initial"]);
  run(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/a.ts",
        anchor: "fn:makeThing",
        lines: [1, 3],
        chose: "baseline reanchor",
        because: "measure native post-tool-use with git HEAD",
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
    run(["hooks", "post-tool-use", "src/a.ts"], "", root);
  }, { iterationsMutateSameRepo: true }, async () => {
    await fs.writeFile(
      path.join(root, "src/a.ts"),
      "// rss sample\nexport function makeThing() {\n  return 1;\n}\n",
      "utf8"
    );
    return runMeasured(["hooks", "post-tool-use", "src/a.ts"], "", root);
  });
}

{
  const root = await tempProject("lint");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const files = 50 * scale;
  for (let index = 0; index < files; index += 1) {
    await fs.writeFile(path.join(root, "src", `file-${index}.ts`), makeSource(20), "utf8");
  }
  await bench("lint.clean-scan.cli", 5, () => {
    run(["lint"], "", root);
  }, { files }, () => runMeasured(["lint"], "", root));
}

{
  const root = await tempProject("status");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  const files = 10 * scale;
  for (let index = 0; index < files; index += 1) {
    const file = `src/file-${index}.ts`;
    await fs.writeFile(path.join(root, file), `export function fn${index}() {\n  return ${index};\n}\n`, "utf8");
    run(
      [
        "write-decision",
        "--json",
        JSON.stringify({
          file,
          anchor: `fn:fn${index}`,
          lines: [1, 3],
          chose: "baseline status cli",
          because: "measure native CLI status over decisions",
          rejected: []
        })
      ],
      "",
      root
    );
  }
  await bench("status.cli", 25 * scale, () => {
    run(["status"], "", root);
  }, { files }, () => runMeasured(["status"], "", root));
}

{
  const root = await tempProject("mcp-ghost-check");
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  await fs.writeFile(path.join(root, "src/drift.ts"), "export function drift() {\n  return 1;\n}\n", "utf8");
  run(
    [
      "write-decision",
      "--json",
      JSON.stringify({
        file: "src/drift.ts",
        anchor: "fn:drift",
        lines: [1, 3],
        chose: "baseline mcp ghost_check",
        because: "measure native MCP ghost_check path",
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
    run(["mcp"], request, root);
  }, undefined, () => runMeasured(["mcp"], request, root));
}

console.log(
  JSON.stringify(
    {
      tool: "archiva-rust-baseline",
      binary: rustBin,
      version: JSON.parse(await fs.readFile(path.join(repoRoot, "package.json"), "utf8")).version,
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
  details?: Record<string, unknown>,
  rssSample?: () => PeakRss | Promise<PeakRss>
): Promise<void> {
  const start = performance.now();
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    await fn(iteration);
  }
  const totalMs = performance.now() - start;
  const peakRss = rssSample ? await rssSample() : undefined;
  results.push({
    name,
    iterations,
    totalMs: round(totalMs),
    meanMs: round(totalMs / iterations),
    peakRss,
    details
  });
}

function run(args: string[], input: string, cwd: string): void {
	const result = spawnSync(rustBin, args, {
	  cwd,
	  input,
	  encoding: "utf8",
	  timeout: commandTimeoutMs,
	  killSignal: "SIGKILL",
	  env: { ...process.env, ARCHIVA_SESSION: "rust_baseline_session" }
	});
	if (result.status !== 0) {
	  throwCommandError(rustBin, args, result.status, result.signal, result.stdout ?? "", result.stderr ?? "", result.error);
	}
}

function runMeasured(args: string[], input: string, cwd: string): PeakRss {
  if (rssTool.status === "unavailable") {
    run(args, input, cwd);
    return rssTool;
  }
  const rssFile = path.join(os.tmpdir(), `archiva-rust-baseline-rss-${process.pid}-${rssFileCounter++}.txt`);
  const measured = measuredCommand(rssTool.command, ["-f", "%M", "-o", rssFile, rustBin, ...args]);
  const result = spawnSync(measured.command, measured.args, {
    cwd,
    input,
    encoding: "utf8",
    timeout: measured.nodeTimeoutMs,
    killSignal: "SIGKILL",
    env: { ...process.env, ARCHIVA_SESSION: "rust_baseline_session" }
  });
  const peakRss = readPeakRss(rssFile);
  removeFileIfExists(rssFile);
	if (result.status !== 0) {
	  throwCommandError(rustBin, args, result.status, result.signal, result.stdout ?? "", result.stderr ?? "", result.error);
	}
	return peakRss;
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

function measuredCommand(command: string, args: string[]): { command: string; args: string[]; nodeTimeoutMs: number } {
  if (processTimeoutTool) {
    return {
      command: processTimeoutTool,
      args: ["--kill-after=5s", `${commandTimeoutMs / 1000}s`, command, ...args],
      nodeTimeoutMs: commandTimeoutMs + 6_000
    };
  }
  return { command, args, nodeTimeoutMs: commandTimeoutMs };
}

function detectProcessTimeoutTool(): string | undefined {
  if (process.platform !== "linux") {
    return undefined;
  }
  const command = "/usr/bin/timeout";
  return fsSync.existsSync(command) ? command : undefined;
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

function makeSource(functions: number): string {
  return Array.from({ length: functions }, (_, index) => {
    return `export function fn${index}(a: boolean, b: boolean) {\n  if (a && b) return ${index};\n  return ${index + 1};\n}`;
  }).join("\n\n");
}

async function tempProject(name: string): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), `archiva-rust-baseline-${name}-`));
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}

function detectRssTool(): { status: "measured"; command: string } | { status: "unavailable"; reason: string } {
  if (process.platform !== "linux") {
    return { status: "unavailable", reason: "RSS measurement is currently implemented for Linux only" };
  }
  const command = "/usr/bin/time";
  if (!fsSync.existsSync(command)) {
    return { status: "unavailable", reason: "/usr/bin/time is not available" };
  }
  const rssFile = path.join(os.tmpdir(), `archiva-rust-baseline-rss-probe-${process.pid}-${rssFileCounter++}.txt`);
  const result = spawnSync(command, ["-f", "%M", "-o", rssFile, "true"], {
    encoding: "utf8",
    timeout: commandTimeoutMs,
    killSignal: "SIGKILL"
  });
  const peakRss = readPeakRss(rssFile);
  removeFileIfExists(rssFile);
  if (result.status !== 0) {
    return { status: "unavailable", reason: `/usr/bin/time probe failed: ${result.stderr || result.stdout}`.trim() };
  }
  if (peakRss.status === "unavailable") {
    return peakRss;
  }
  return { status: "measured", command };
}

function readPeakRss(rssFile: string): PeakRss {
  try {
    const content = fsSync.readFileSync(rssFile, "utf8").trim();
    const numericLine = content
      .split(/\r?\n/)
      .map((line) => line.trim())
      .reverse()
      .find((line) => /^\d+$/.test(line));
    const value = numericLine === undefined ? Number.NaN : Number(numericLine);
    if (!Number.isFinite(value) || value <= 0) {
      return { status: "unavailable", reason: `unparsable RSS output: ${JSON.stringify(content)}` };
    }
    return { status: "measured", peakRssKb: value };
  } catch (error) {
    return { status: "unavailable", reason: `unable to read RSS output: ${String(error)}` };
  }
}

function removeFileIfExists(file: string): void {
  try {
    fsSync.unlinkSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }
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
