import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";

const repoRoot = path.dirname(path.dirname(new URL(import.meta.url).pathname));
const auditScript = path.join(repoRoot, "tools/audit-v2-completion.mjs");

const heavyArtifacts = [
  "archiva-differential.json",
  "archiva-stress-soak.json",
  "archiva-benchmark.json",
  "archiva-scale-smoke.json",
  "archiva-scale-seeded.json",
  "archiva-scale-corpus.json",
  "archiva-scale-rust-corpus.json"
];

const longHorizonCorpora = [
  "rust-compiler",
  "cargo",
  "ripgrep",
  "tokio",
  "linux-kernel",
  "llvm",
  "typescript",
  "node",
  "react",
  "next"
];

const auditFixtureFiles = [
  "package.json",
  "Cargo.toml",
  "README.md",
  ".github/workflows/ci.yml",
  ".github/workflows/publish.yml",
  ".github/workflows/validation.yml",
  "docs/archiva-v2-architecture.md",
  "docs/archiva-v2-review-status.md",
  "src/cli.rs",
  "src/mcp.rs",
  "src/core/anchor.rs",
  "src/core/decision.rs",
  "src/core/diff.rs",
  "src/core/dlog.rs",
  "src/core/dmap.rs",
  "src/core/fingerprint.rs",
  "src/core/fs.rs",
  "src/core/git.rs",
  "src/core/gitignore.rs",
  "src/core/json.rs",
  "src/core/paths.rs",
  "src/core/project.rs",
  "src/core/settings.rs",
  "src/core/storage.rs",
  "src/core/yaml.rs",
  "tools/archiva-differential.ts",
  "tools/archiva-scale-smoke.ts",
  "tools/validate-native-package-metadata.mjs"
];

describe("v2 completion audit", () => {
  it("passes local checks and verifies critical script wiring", () => {
    const result = runAudit(["--json"]);

    expect(result.status).toBe(0);
    const output = parseAuditOutput(result.stdout);
    expect(output.status).toBe("passed");
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ name: "critical script check has expected command", passed: true }),
        expect.objectContaining({ name: "critical script audit:v2 has expected command", passed: true }),
        expect.objectContaining({ name: "validation workflow writes JSON artifacts with silent npm producers", passed: true })
      ])
    );
  });

  it("fails strict completion until external evidence is supplied and accepted", () => {
    const result = runAudit(["--json", "--strict-complete"]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.status).toBe("failed");
    expect(output.externalEvidenceStillRequired).toContain("npm publish and post-publish install smoke artifacts");
  });

  it("fails strict completion even with a passing evidence bundle", async () => {
    const evidenceDir = await evidenceBundle();
    const result = runAudit(["--json", "--strict-complete", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.status).toBe("failed");
    expect(output.externalEvidenceStillRequired).toContain("scheduled or manually triggered long-horizon corpus artifacts");
  });

  it("fails when a release soak script is weakened", async () => {
    const fixture = await auditFixture(async (root) => {
      const packagePath = path.join(root, "package.json");
      const packageJson = JSON.parse(await fs.readFile(packagePath, "utf8")) as {
        scripts: Record<string, string>;
      };
      packageJson.scripts["stress:soak"] = "npm run stress:rust-port";
      await fs.writeFile(packagePath, JSON.stringify(packageJson, null, 2), "utf8");
    });
    const result = runAudit(["--json"], { ARCHIVA_AUDIT_REPO_ROOT: fixture });

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "critical script stress:soak has expected command",
          passed: false
        })
      ])
    );
  });

  it("fails when workflow JSON artifact producers are not silent", async () => {
    const fixture = await auditFixture(async (root) => {
      const workflowPath = path.join(root, ".github/workflows/validation.yml");
      const workflow = await fs.readFile(workflowPath, "utf8");
      await fs.writeFile(
        workflowPath,
        workflow.replace("npm run --silent differential:release | tee archiva-differential.json", "npm run differential:release | tee archiva-differential.json"),
        "utf8"
      );
    });
    const result = runAudit(["--json"], { ARCHIVA_AUDIT_REPO_ROOT: fixture });

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "validation workflow writes JSON artifacts with silent npm producers",
          passed: false
        })
      ])
    );
  });

  it("accepts a complete evidence bundle with C/C++ semantic coverage", async () => {
    const evidenceDir = await evidenceBundle({
      cxxAnchorKinds: { function: 3, struct: 1 }
    });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(0);
    const output = parseAuditOutput(result.stdout);
    expect(output.status).toBe("passed");
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "C/C++ evidence artifact archiva-long-horizon-linux-kernel.json covered function anchors",
          passed: true
        }),
        expect.objectContaining({
          name: "C/C++ evidence artifact archiva-long-horizon-llvm.json covered structural or method anchors",
          passed: true
        })
      ])
    );
  });

  it("accepts GitHub-style nested artifact download directories", async () => {
    const evidenceDir = await evidenceBundle({
      nested: true,
      cxxAnchorKinds: { function: 3, struct: 1 }
    });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(0);
    const output = parseAuditOutput(result.stdout);
    expect(output.status).toBe("passed");
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "evidence artifact archiva-long-horizon-next.json passed",
          passed: true
        })
      ])
    );
  });

  it("fails when an expected evidence artifact is missing", async () => {
    const evidenceDir = await evidenceBundle({ omit: "archiva-long-horizon-next.json" });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.status).toBe("failed");
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "evidence artifact archiva-long-horizon-next.json exists and is valid JSON",
          passed: false
        })
      ])
    );
  });

  it("fails when an evidence artifact is invalid JSON", async () => {
    const evidenceDir = await evidenceBundle({ invalid: "archiva-benchmark.json" });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "evidence artifact archiva-benchmark.json exists and is valid JSON",
          passed: false
        })
      ])
    );
  });

  it("fails when evidence artifact lookup is ambiguous", async () => {
    const evidenceDir = await evidenceBundle({ duplicate: "archiva-benchmark.json" });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "evidence artifact archiva-benchmark.json exists and is valid JSON",
          passed: false
        })
      ])
    );
  });

  it("fails when an evidence artifact reports failed status", async () => {
    const evidenceDir = await evidenceBundle({ failed: "archiva-stress-soak.json" });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "evidence artifact archiva-stress-soak.json passed",
          passed: false
        })
      ])
    );
  });

  it("fails when required C/C++ long-horizon artifacts are not native-only", async () => {
    const evidenceDir = await evidenceBundle({ cxxNativeOnly: false });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "C/C++ evidence artifact archiva-long-horizon-linux-kernel.json used native-only validation",
          passed: false
        })
      ])
    );
  });

  it("fails C/C++ evidence artifacts without function coverage", async () => {
    const evidenceDir = await evidenceBundle({
      cxxAnchorKinds: { struct: 2 }
    });
    const result = runAudit(["--json", "--evidence-dir", evidenceDir]);

    expect(result.status).toBe(1);
    const output = parseAuditOutput(result.stdout);
    expect(output.localChecks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "C/C++ evidence artifact archiva-long-horizon-linux-kernel.json covered function anchors",
          passed: false
        })
      ])
    );
  });
});

function runAudit(args: string[], env: Record<string, string> = {}) {
  return spawnSync(process.execPath, [auditScript, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    env: { ...process.env, ...env }
  });
}

function parseAuditOutput(stdout: string): {
  status: "passed" | "failed";
  localChecks: Array<{ name: string; passed: boolean }>;
  externalEvidenceStillRequired: string[];
} {
  return JSON.parse(stdout);
}

async function evidenceBundle(options: {
  omit?: string;
  invalid?: string;
  duplicate?: string;
  failed?: string;
  nested?: boolean;
  cxxNativeOnly?: boolean;
  cxxAnchorKinds?: Record<string, number>;
} = {}): Promise<string> {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-v2-evidence-"));
  const writeArtifact = async (file: string, value: unknown) => {
    if (file === options.omit) {
      return;
    }
    const directory = options.nested ? path.join(root, file.replace(/\.json$/, "")) : root;
    await fs.mkdir(directory, { recursive: true });
    const target = path.join(directory, file);
    if (file === options.invalid) {
      await fs.writeFile(target, "{ invalid json", "utf8");
      return;
    }
    const artifact = file === options.failed ? { status: "failed" } : value;
    await fs.writeFile(target, JSON.stringify(artifact, null, 2), "utf8");
    if (file === options.duplicate) {
      const duplicateDirectory = path.join(root, "duplicate");
      await fs.mkdir(duplicateDirectory, { recursive: true });
      await fs.writeFile(path.join(duplicateDirectory, file), JSON.stringify(artifact, null, 2), "utf8");
    }
  };

  for (const file of heavyArtifacts) {
    await writeArtifact(file, { status: "passed" });
  }
  for (const name of longHorizonCorpora) {
    const file = `archiva-long-horizon-${name}.json`;
    const value =
      name === "linux-kernel" || name === "llvm"
        ? options.cxxNativeOnly === false
          ? { status: "passed" }
          : cxxArtifact(options.cxxAnchorKinds ?? { function: 2, struct: 1 })
        : { status: "passed" };
    await writeArtifact(file, value);
  }
  return root;
}

async function auditFixture(mutator: (root: string) => Promise<void>): Promise<string> {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-audit-fixture-"));
  for (const file of auditFixtureFiles) {
    const source = path.join(repoRoot, file);
    const target = path.join(root, file);
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.copyFile(source, target);
  }
  await mutator(root);
  return root;
}

function cxxArtifact(anchorKinds: Record<string, number>) {
  return {
    status: "passed",
    corpus: {
      language: "c/cpp",
      validation: "c/cpp-native-only",
      rust: {
        semanticSummary: { anchorKinds }
      }
    }
  };
}
