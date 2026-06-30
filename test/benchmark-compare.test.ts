import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";

const repoRoot = path.dirname(path.dirname(new URL(import.meta.url).pathname));
const compareScript = path.join(repoRoot, "tools/archiva-benchmark-compare.ts");
const tsxBin = path.join(repoRoot, "node_modules", ".bin", process.platform === "win32" ? "tsx.cmd" : "tsx");

describe("benchmark RSS gating", () => {
  it("passes when measured Rust RSS stays within the configured budget", async () => {
    const fixture = await fakeBaselineFixture();
    const result = runCompare(fixture, {
      ARCHIVA_FAKE_RSS_MODE: "measured",
      ARCHIVA_FAKE_RSS_KB: "4096",
      ARCHIVA_BENCHMARK_MAX_RSS_KB: "8192"
    });

    expect(result.status).toBe(0);
    const output = parseOutput(result.stdout);
    expect(output.status).toBe("passed");
    expect(output.defaultMaxRssKb).toBe(8192);
    expect(output.comparisons.every((comparison) => comparison.rssOk)).toBe(true);
  });

  it("fails when measured Rust RSS exceeds the configured budget", async () => {
    const fixture = await fakeBaselineFixture();
    const result = runCompare(fixture, {
      ARCHIVA_FAKE_RSS_MODE: "measured",
      ARCHIVA_FAKE_RSS_KB: "16384",
      ARCHIVA_BENCHMARK_MAX_RSS_KB: "8192"
    });

    expect(result.status).toBe(1);
    const output = parseOutput(result.stdout);
    expect(output.status).toBe("failed");
    expect(output.comparisons.some((comparison) => !comparison.rssOk && comparison.rssReason?.includes("exceeded budget") === true)).toBe(true);
  });

  it("fails on Linux-style required RSS when Rust RSS is unavailable", async () => {
    const fixture = await fakeBaselineFixture();
    const result = runCompare(fixture, {
      ARCHIVA_FAKE_RSS_MODE: "unavailable",
      ARCHIVA_BENCHMARK_REQUIRE_RSS: "1"
    });

    expect(result.status).toBe(1);
    const output = parseOutput(result.stdout);
    expect(output.status).toBe("failed");
    expect(output.comparisons.every((comparison) => comparison.rssOk === false)).toBe(true);
  });

  it("allows RSS enforcement to be disabled explicitly", async () => {
    const fixture = await fakeBaselineFixture();
    const result = runCompare(fixture, {
      ARCHIVA_FAKE_RSS_MODE: "measured",
      ARCHIVA_FAKE_RSS_KB: "16384",
      ARCHIVA_BENCHMARK_MAX_RSS_KB: "8192",
      ARCHIVA_BENCHMARK_ENFORCE_RSS: "0"
    });

    expect(result.status).toBe(0);
    const output = parseOutput(result.stdout);
    expect(output.status).toBe("passed");
    expect(output.comparisons.every((comparison) => comparison.rssReason === "RSS enforcement disabled")).toBe(true);
  });
});

function runCompare(fixture: string, env: Record<string, string>) {
  return spawnSync(tsxBin, [compareScript], {
    cwd: repoRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      ARCHIVA_BENCHMARK_TS_BASELINE_SCRIPT: path.join(fixture, "fake-ts-baseline.mjs"),
      ARCHIVA_BENCHMARK_RUST_BASELINE_SCRIPT: path.join(fixture, "fake-rust-baseline.mjs"),
      ARCHIVA_BENCHMARK_REQUIRE_RSS: "1",
      ...env
    }
  });
}

async function fakeBaselineFixture(): Promise<string> {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-benchmark-compare-"));
  await fs.writeFile(
    path.join(root, "fake-ts-baseline.mjs"),
    `console.log(JSON.stringify({ tool: "archiva-ts-baseline", results: metrics() }));
function metrics() {
  return ${JSON.stringify(metricNames().map((name) => ({ name, iterations: 1, totalMs: 100, meanMs: 100 })))};
}
`,
    "utf8"
  );
  await fs.writeFile(
    path.join(root, "fake-rust-baseline.mjs"),
    `console.log(JSON.stringify({ tool: "archiva-rust-baseline", results: metrics() }));
function metrics() {
  const mode = process.env.ARCHIVA_FAKE_RSS_MODE;
  const peakRss =
    mode === "measured"
      ? { status: "measured", peakRssKb: Number(process.env.ARCHIVA_FAKE_RSS_KB) }
      : mode === "unavailable"
        ? { status: "unavailable", reason: "fake RSS unavailable" }
        : undefined;
  return ${JSON.stringify(metricNames().map((name) => ({ name, iterations: 1, totalMs: 10, meanMs: 10 })))}.map((metric) => ({ ...metric, peakRss }));
}
`,
    "utf8"
  );
  return root;
}

function metricNames(): string[] {
  return [
    "startup.version",
    "decision.write.cli",
    "decision.why.cli",
    "reanchor.postToolUse.git.cli",
    "lint.clean-scan.cli",
    "status.cli",
    "mcp.ghost_check.cli"
  ];
}

function parseOutput(stdout: string): {
  status: string;
  defaultMaxRssKb: number;
  comparisons: Array<{ rssOk: boolean; rssReason?: string }>;
} {
  return JSON.parse(stdout);
}
