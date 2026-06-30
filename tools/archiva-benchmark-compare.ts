import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

type BaselineOutput = {
  tool: string;
  results: BenchResult[];
};

type BenchResult = {
  name: string;
  iterations: number;
  totalMs: number;
  meanMs: number;
  peakRss?: PeakRss;
};

type PeakRss =
  | { status: "measured"; peakRssKb: number }
  | { status: "unavailable"; reason: string };

type Comparison = {
  name: string;
  tsMeanMs: number;
  rustMeanMs: number;
  ratio: number;
  maxRatio: number;
  ratioOk: boolean;
  rustPeakRss?: PeakRss;
  maxRssKb?: number;
  rssOk: boolean;
  rssReason?: string;
  ok: boolean;
};

const repoRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const tsxBin = path.join(repoRoot, "node_modules", ".bin", process.platform === "win32" ? "tsx.cmd" : "tsx");
const rustBinInput = process.env.ARCHIVA_RUST_BIN ?? "target/release/archiva";
const rustBin = path.isAbsolute(rustBinInput) ? rustBinInput : path.resolve(repoRoot, rustBinInput);
const tsBaselineScript = process.env.ARCHIVA_BENCHMARK_TS_BASELINE_SCRIPT ?? "tools/archiva-ts-baseline.ts";
const rustBaselineScript = process.env.ARCHIVA_BENCHMARK_RUST_BASELINE_SCRIPT ?? "tools/archiva-rust-baseline.ts";
const scale = process.env.ARCHIVA_BENCHMARK_SCALE ?? process.env.ARCHIVA_BASELINE_SCALE ?? "1";
const compareTimeoutMs = positiveIntEnv(
  "ARCHIVA_BENCHMARK_COMPARE_TIMEOUT_MS",
  positiveIntEnv("ARCHIVA_BENCHMARK_COMMAND_TIMEOUT_MS", 900_000)
);
const baselineTimeoutMs = positiveIntEnv(
  "ARCHIVA_BASELINE_COMMAND_TIMEOUT_MS",
  positiveIntEnv("ARCHIVA_BENCHMARK_COMMAND_TIMEOUT_MS", 300_000)
);
const defaultMaxRatio = numberEnv("ARCHIVA_BENCHMARK_MAX_RATIO", 1.2);
const enforceRss = booleanEnv("ARCHIVA_BENCHMARK_ENFORCE_RSS", true);
const requireMeasuredRss = booleanEnv("ARCHIVA_BENCHMARK_REQUIRE_RSS", process.platform === "linux");
const defaultMaxRssKb = positiveIntEnv("ARCHIVA_BENCHMARK_MAX_RSS_KB", 128 * 1024);
const metricBudgets = new Map<string, number>([
  ["startup.version", numberEnv("ARCHIVA_BENCHMARK_STARTUP_MAX_RATIO", 0.5)]
]);
const comparedMetrics = [
  "startup.version",
  "decision.write.cli",
  "decision.why.cli",
  "reanchor.postToolUse.git.cli",
  "lint.clean-scan.cli",
  "status.cli",
  "mcp.ghost_check.cli"
];

const ts = runBaseline("archiva-ts-baseline", tsBaselineScript, {});
const rust = runBaseline("archiva-rust-baseline", rustBaselineScript, {
  ARCHIVA_RUST_BIN: rustBin
});

const comparisons = comparedMetrics.map((name) => compareMetric(name, ts, rust));
const ok = comparisons.every((comparison) => comparison.ok);
const rustPeakRss = summarizePeakRss(comparisons);

console.log(
  JSON.stringify(
    {
      tool: "archiva-benchmark-compare",
	      status: ok ? "passed" : "failed",
	      scale: Number(scale),
	      rustBinary: rustBin,
	      compareTimeoutMs,
	      baselineTimeoutMs,
	      defaultMaxRatio,
	      enforceRss,
	      requireMeasuredRss,
	      defaultMaxRssKb,
	      rustPeakRss,
	      comparisons
    },
    null,
    2
  )
);
process.exit(ok ? 0 : 1);

function runBaseline(tool: string, script: string, env: Record<string, string>): BaselineOutput {
	const result = spawnSync(tsxBin, [script], {
	  cwd: repoRoot,
	  encoding: "utf8",
	  timeout: compareTimeoutMs,
	  killSignal: "SIGKILL",
	  env: {
	    ...process.env,
	    ARCHIVA_BASELINE_SCALE: scale,
	    ARCHIVA_BASELINE_COMMAND_TIMEOUT_MS: String(baselineTimeoutMs),
	    ...env
	  }
	});
	if (result.status !== 0) {
	  throwCommandError(tool, [script], result.status, result.signal, result.stdout ?? "", result.stderr ?? "", result.error);
	}
  const output = JSON.parse(result.stdout) as BaselineOutput;
  if (output.tool !== tool) {
    throw new Error(`Expected ${tool} output, got ${output.tool}`);
  }
  return output;
}

function compareMetric(name: string, ts: BaselineOutput, rust: BaselineOutput): Comparison {
  const tsMetric = findMetric(ts, name);
  const rustMetric = findMetric(rust, name);
  const maxRatio = metricBudgets.get(name) ?? defaultMaxRatio;
  const ratio = round(rustMetric.meanMs / tsMetric.meanMs);
  const ratioOk = ratio <= maxRatio;
  const rss = evaluateRss(name, rustMetric.peakRss);
  return {
    name,
    tsMeanMs: tsMetric.meanMs,
    rustMeanMs: rustMetric.meanMs,
    ratio,
    maxRatio,
    ratioOk,
    rustPeakRss: rustMetric.peakRss,
    maxRssKb: rss.maxRssKb,
    rssOk: rss.ok,
    rssReason: rss.reason,
    ok: ratioOk && rss.ok
  };
}

function evaluateRss(name: string, rustPeakRss: PeakRss | undefined): { ok: boolean; maxRssKb?: number; reason?: string } {
  if (!enforceRss) {
    return { ok: true, reason: "RSS enforcement disabled" };
  }
  const maxRssKb = maxRssKbForMetric(name);
  if (!rustPeakRss) {
    return requireMeasuredRss
      ? { ok: false, maxRssKb, reason: "Rust benchmark did not report peak RSS" }
      : { ok: true, maxRssKb, reason: "Rust benchmark did not report peak RSS; measurement not required on this platform" };
  }
  if (rustPeakRss.status === "unavailable") {
    return requireMeasuredRss
      ? { ok: false, maxRssKb, reason: rustPeakRss.reason }
      : { ok: true, maxRssKb, reason: rustPeakRss.reason };
  }
  if (rustPeakRss.peakRssKb > maxRssKb) {
    return {
      ok: false,
      maxRssKb,
      reason: `Rust peak RSS ${rustPeakRss.peakRssKb} KB exceeded budget ${maxRssKb} KB`
    };
  }
  return { ok: true, maxRssKb };
}

function maxRssKbForMetric(name: string): number {
  return positiveIntEnv(`ARCHIVA_BENCHMARK_${metricEnvKey(name)}_MAX_RSS_KB`, defaultMaxRssKb);
}

function metricEnvKey(name: string): string {
  return name.replace(/[^A-Za-z0-9]+/g, "_").replace(/^_+|_+$/g, "").toUpperCase();
}

function summarizePeakRss(comparisons: Comparison[]): PeakRss | undefined {
  const measured = comparisons
    .map((comparison) => comparison.rustPeakRss)
    .filter((peakRss): peakRss is { status: "measured"; peakRssKb: number } => peakRss?.status === "measured");
  if (measured.length > 0) {
    return {
      status: "measured",
      peakRssKb: Math.max(...measured.map((peakRss) => peakRss.peakRssKb))
    };
  }
  return comparisons.find((comparison) => comparison.rustPeakRss !== undefined)?.rustPeakRss;
}

function findMetric(output: BaselineOutput, name: string): BenchResult {
  const metric = output.results.find((result) => result.name === name);
  if (!metric) {
    throw new Error(`${output.tool} did not report ${name}`);
  }
  if (metric.meanMs <= 0) {
    throw new Error(`${output.tool} reported non-positive mean for ${name}`);
  }
  return metric;
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
	    `${command} ${args.join(" ")} timed out after ${compareTimeoutMs}ms: ${formatFailure(status, signal, stdout, stderr, error)}`
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

function numberEnv(name: string, fallback: number): number {
  const value = process.env[name];
  if (!value) {
    return fallback;
  }
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive number`);
  }
  return parsed;
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

function booleanEnv(name: string, fallback: boolean): boolean {
  const value = process.env[name];
  if (!value) {
    return fallback;
  }
  if (["1", "true", "yes", "on"].includes(value.toLowerCase())) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(value.toLowerCase())) {
    return false;
  }
  throw new Error(`${name} must be a boolean`);
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}
