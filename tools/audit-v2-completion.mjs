import fs from "node:fs/promises";
import path from "node:path";

const repoRoot = path.resolve(process.env.ARCHIVA_AUDIT_REPO_ROOT ?? path.join(import.meta.dirname, ".."));

const requiredScripts = [
  "build",
  "build:rust",
  "check",
  "check:rust",
  "check:ts",
  "check:package",
  "test",
  "differential:release",
  "property:soak",
  "stress:soak",
  "benchmark:compare",
  "scale:smoke",
  "scale:corpus",
  "scale:corpus:rust",
  "smoke:package"
];

const criticalScriptCommands = {
  check: "npm run check:ts && npm run check:rust && npm run check:package && npm run audit:v2",
  "check:package":
    "node tools/validate-native-package-metadata.mjs && node tools/stage-native-package.mjs --meta-only && node tools/validate-native-package-metadata.mjs --meta-package",
  "differential:release": "ARCHIVA_RUST_BIN=target/release/archiva tsx tools/archiva-differential.ts",
  "property:soak": "cargo test --quiet --lib property_extended_serialization_and_diff -- --ignored",
  "stress:rust-port": "ARCHIVA_RUST_BIN=target/release/archiva tsx tools/archiva-stress-rust-port.ts",
  "stress:soak": "ARCHIVA_STRESS_FILES=10 ARCHIVA_STRESS_FUNCTIONS=6 ARCHIVA_STRESS_CYCLES=30 npm run --silent stress:rust-port",
  "benchmark:compare": "ARCHIVA_RUST_BIN=target/release/archiva tsx tools/archiva-benchmark-compare.ts",
  "scale:smoke": "ARCHIVA_RUST_BIN=dist-native/archiva tsx tools/archiva-scale-smoke.ts",
  "scale:corpus": "ARCHIVA_RUST_BIN=dist-native/archiva ARCHIVA_SCALE_CORPUS=1 tsx tools/archiva-scale-smoke.ts",
  "scale:corpus:rust":
    "ARCHIVA_SCALE_CORPUS_ROOT=src ARCHIVA_SCALE_CORPUS_FILES=40 ARCHIVA_SCALE_CORPUS_DECISIONS=24 ARCHIVA_SCALE_CORPUS_MUTATE_FILES=16 ARCHIVA_SCALE_CORPUS_LANGUAGE=rust npm run --silent scale:corpus",
  "smoke:package": "node tools/smoke-native-package.mjs",
  "audit:v2": "node tools/audit-v2-completion.mjs"
};

const requiredCliCommands = [
  "init",
  "why",
  "history",
  "lint",
  "status",
  "hooks",
  "mcp",
  "write-decision"
];

const requiredCoreFiles = [
  "src/main.rs",
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
  "src/core/yaml.rs"
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

const nativeTargets = [
  "linux-x64-gnu",
  "linux-x64-musl",
  "linux-arm64-gnu",
  "linux-arm64-musl",
  "darwin-x64",
  "darwin-arm64",
  "win32-x64-msvc"
];

const archivedEvidenceNeedles = [
  "macOS and Windows Rust build/test results",
  "Linux arm64 and musl native package build/smoke results",
  "full heavy-validation workflow artifacts",
  "scheduled or manually triggered long-horizon corpus artifacts",
  "npm publish and post-publish install smoke artifacts"
];

const remainingReleaseEvidenceNeedles = [];

const hasJson = process.argv.includes("--json");
const hasStrictComplete = process.argv.includes("--strict-complete");
const evidenceDir = readArg("--evidence-dir");

const checks = [];

function readArg(name) {
  const index = process.argv.indexOf(name);
  if (index === -1) {
    return undefined;
  }
  const value = process.argv[index + 1];
  if (!value || value.startsWith("--")) {
    throw new Error(`Missing value for ${name}.`);
  }
  return value;
}

function record(name, evidence, passed, detail = "") {
  checks.push({ name, evidence, passed, detail });
}

function assertCheck(name, evidence, condition, detail = "") {
  record(name, evidence, Boolean(condition), detail);
}

async function readText(relative) {
  return fs.readFile(path.join(repoRoot, relative), "utf8");
}

async function readJson(relative) {
  return JSON.parse(await readText(relative));
}

async function readJsonAbsolute(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

async function findEvidenceArtifact(root, artifact) {
  const matches = [];
  const pending = [root];
  while (pending.length > 0) {
    const current = pending.pop();
    let entries;
    try {
      entries = await fs.readdir(current, { withFileTypes: true });
    } catch (error) {
      return {
        error: error instanceof Error ? error.message : String(error)
      };
    }
    for (const entry of entries) {
      const child = path.join(current, entry.name);
      if (entry.isDirectory()) {
        pending.push(child);
      } else if (entry.isFile() && entry.name === artifact) {
        matches.push(child);
      }
    }
  }
  if (matches.length === 0) {
    return { error: "artifact not found" };
  }
  if (matches.length > 1) {
    return { error: `artifact matched multiple files: ${matches.join(", ")}` };
  }
  return { file: matches[0] };
}

async function exists(relative) {
  try {
    await fs.access(path.join(repoRoot, relative));
    return true;
  } catch {
    return false;
  }
}

function includesAll(text, values) {
  return values.every((value) => text.includes(value));
}

function tomlSectionBody(text, section) {
  const header = `[${section}]`;
  const start = text.indexOf(header);
  if (start === -1) {
    return undefined;
  }
  const after = start + header.length;
  const next = text.slice(after).search(/\n\[[^\]]+\]/);
  return (next === -1 ? text.slice(after) : text.slice(after, after + next)).trim();
}

function missingValues(text, values) {
  return values.filter((value) => !text.includes(value));
}

async function auditPackageAndToolchain() {
  const packageJson = await readJson("package.json");
  const cargoToml = await readText("Cargo.toml");
  const dependenciesBody = tomlSectionBody(cargoToml, "dependencies");

  assertCheck(
    "root npm package has no runtime dependencies",
    "package.json dependencies",
    JSON.stringify(packageJson.dependencies ?? {}) === "{}"
  );
  assertCheck("root package exposes all validation scripts", "package.json scripts", requiredScripts.every((name) => packageJson.scripts?.[name]));
  for (const [name, expected] of Object.entries(criticalScriptCommands)) {
    assertCheck(`critical script ${name} has expected command`, `package.json scripts.${name}`, packageJson.scripts?.[name] === expected, `actual=${JSON.stringify(packageJson.scripts?.[name])}`);
  }
  assertCheck("Rust crate pins 1.96.0", "Cargo.toml rust-version", cargoToml.includes('rust-version = "1.96.0"'));
  assertCheck("Rust crate has no dependencies", "Cargo.toml [dependencies]", dependenciesBody === "");
}

async function auditBehaviorSurface() {
  const cli = await readText("src/cli.rs");
  const mcp = await readText("src/mcp.rs");
  const missingCommands = missingValues(cli, requiredCliCommands.map((command) => `"${command}"`));

  assertCheck("core Rust files exist", "src/core/*.rs", (await Promise.all(requiredCoreFiles.map(exists))).every(Boolean));
  assertCheck("CLI command dispatch covers required commands", "src/cli.rs", missingCommands.length === 0, missingCommands.join(", "));
  assertCheck("MCP supports initialize, tools/list, and tools/call", "src/mcp.rs", includesAll(mcp, ["initialize", "tools/list", "tools/call"]));
}

async function auditLanguageAndGitCoverage() {
  const anchor = await readText("src/core/anchor.rs");
  const project = await readText("src/core/project.rs");
  const git = await readText("src/core/git.rs");
  const differential = await readText("tools/archiva-differential.ts");
  const scale = await readText("tools/archiva-scale-smoke.ts");

  assertCheck("native C/C++ extractor is present", "src/core/anchor.rs", includesAll(anchor, ["extract_c_family_anchors", "collect_c_family_type_anchors", "collect_c_family_function_anchors"]));
  assertCheck("source discovery includes C/C++ extensions", "src/core/project.rs", includesAll(project, ['"cpp"', '"hpp"', '"c"', '"h"', '"inc"']));
  assertCheck("native Git reader includes SHA-1 and SHA-256 object-format support", "src/core/git.rs", includesAll(git, ["GitObjectFormat", "Sha1", "Sha256"]));
  assertCheck("differential suite includes SHA-256 Git scenario", "tools/archiva-differential.ts", differential.includes("post-tool-use-sha256-git"));
  assertCheck("corpus scale supports Rust and C/C++ native-only modes", "tools/archiva-scale-smoke.ts", includesAll(scale, ['"rust-native-only"', '"c/cpp-native-only"', "assertCxxCorpusCoverage"]));
}

async function auditWorkflowEvidenceProducers() {
  const validation = await readText(".github/workflows/validation.yml");
  const publish = await readText(".github/workflows/publish.yml");
  const ci = await readText(".github/workflows/ci.yml");
  const metadataValidator = await readText("tools/validate-native-package-metadata.mjs");

  assertCheck("CI workflow names all native targets", ".github/workflows/ci.yml", includesAll(ci, nativeTargets));
  assertCheck("validation workflow has complete long-horizon matrix", ".github/workflows/validation.yml", includesAll(validation, longHorizonCorpora));
  assertCheck("publish workflow gates native publish on long-horizon matrix", ".github/workflows/publish.yml", publish.includes("needs: [heavy-validation, long-horizon-corpus]"));
  assertCheck("validation workflow writes JSON artifacts with silent npm producers", ".github/workflows/validation.yml", includesAll(validation, ["npm run --silent differential:release | tee archiva-differential.json", "npm run --silent stress:soak | tee archiva-stress-soak.json"]));
  assertCheck("publish workflow writes JSON artifacts with silent npm producers", ".github/workflows/publish.yml", includesAll(publish, ["npm run --silent differential:release | tee archiva-differential.json", "npm run --silent stress:soak | tee archiva-stress-soak.json"]));
  assertCheck("metadata validator enforces long-horizon matrix", "tools/validate-native-package-metadata.mjs", includesAll(metadataValidator, longHorizonCorpora));
  assertCheck("metadata validator requires C/C++ corpus language", "tools/validate-native-package-metadata.mjs", metadataValidator.includes('languages.has("c/cpp")'));
}

async function auditDocumentationHonesty() {
  const architecture = await readText("docs/archiva-v2-architecture.md");
  const review = await readText("docs/archiva-v2-review-status.md");
  const readme = await readText("README.md");

  assertCheck("architecture document exists and covers future extension points", "docs/archiva-v2-architecture.md", architecture.includes("## Future Extension Points"));
  assertCheck("review status marks release complete after publish artifacts passed", "docs/archiva-v2-review-status.md", review.includes("release v2 objective is complete"));
  assertCheck("review status lists archived external validation evidence", "docs/archiva-v2-review-status.md", includesAll(review, archivedEvidenceNeedles));
  assertCheck("review status says no release evidence remains", "docs/archiva-v2-review-status.md", review.includes("No release evidence remains outstanding."));
  assertCheck("README links v2 architecture and review status", "README.md", includesAll(readme, ["docs/archiva-v2-architecture.md", "docs/archiva-v2-review-status.md"]));
  assertCheck("README heavy-validation commands include release stress soak", "README.md", readme.includes("npm run stress:soak"));
}

async function auditEvidenceArtifacts(dir) {
  const heavyArtifacts = [
    "archiva-differential.json",
    "archiva-stress-soak.json",
    "archiva-benchmark.json",
    "archiva-scale-smoke.json",
    "archiva-scale-seeded.json",
    "archiva-scale-corpus.json",
    "archiva-scale-rust-corpus.json"
  ];
  const longHorizonArtifacts = longHorizonCorpora.map((name) => `archiva-long-horizon-${name}.json`);

  for (const artifact of [...heavyArtifacts, ...longHorizonArtifacts]) {
    const found = await findEvidenceArtifact(path.resolve(dir), artifact);
    const file = found.file ?? path.resolve(dir, artifact);
    if (!found.file) {
      record(`evidence artifact ${artifact} exists and is valid JSON`, file, false, found.error ?? "artifact not found");
      continue;
    }
    let parsed;
    try {
      parsed = await readJsonAbsolute(file);
    } catch (error) {
      record(`evidence artifact ${artifact} exists and is valid JSON`, file, false, error instanceof Error ? error.message : String(error));
      continue;
    }
    assertCheck(`evidence artifact ${artifact} passed`, file, parsed?.status === "passed", `status=${JSON.stringify(parsed?.status)}`);
    const requiresCxx = artifact === "archiva-long-horizon-linux-kernel.json" || artifact === "archiva-long-horizon-llvm.json";
    if (requiresCxx || parsed?.corpus?.validation === "c/cpp-native-only") {
      const anchorKinds = parsed.corpus?.rust?.semanticSummary?.anchorKinds ?? {};
      const structuralKinds = ["class", "enum", "method", "struct"];
      assertCheck(
        `C/C++ evidence artifact ${artifact} used native-only validation`,
        file,
        parsed.corpus?.language === "c/cpp" && parsed.corpus?.validation === "c/cpp-native-only",
        `language=${JSON.stringify(parsed.corpus?.language)} validation=${JSON.stringify(parsed.corpus?.validation)}`
      );
      assertCheck(`C/C++ evidence artifact ${artifact} covered function anchors`, file, (anchorKinds.function ?? 0) > 0);
      assertCheck(
        `C/C++ evidence artifact ${artifact} covered structural or method anchors`,
        file,
        structuralKinds.some((kind) => (anchorKinds[kind] ?? 0) > 0),
        `anchorKinds=${JSON.stringify(anchorKinds)}`
      );
    }
  }
}

function printHuman() {
  const failed = checks.filter((check) => !check.passed);
  for (const check of checks) {
    const status = check.passed ? "ok" : "FAIL";
    const detail = check.detail ? ` (${check.detail})` : "";
    console.log(`${status} ${check.name} - ${check.evidence}${detail}`);
  }
  if (failed.length > 0) {
    console.error(`Archiva v2 completion audit failed: ${failed.length} local evidence check${failed.length === 1 ? "" : "s"} failed.`);
    process.exitCode = 1;
    return;
  }
  console.log(`Archiva v2 completion audit OK (${checks.length} local evidence checks). Published release evidence is archived; strict completion is allowed.`);
}

async function main() {
  await auditPackageAndToolchain();
  await auditBehaviorSurface();
  await auditLanguageAndGitCoverage();
  await auditWorkflowEvidenceProducers();
  await auditDocumentationHonesty();
  if (evidenceDir) {
    await auditEvidenceArtifacts(evidenceDir);
  }

  if (hasJson) {
    const failed = checks.filter((check) => !check.passed);
    const strictFailure = hasStrictComplete && remainingReleaseEvidenceNeedles.length > 0;
    console.log(
      JSON.stringify(
        {
          status: failed.length === 0 && !strictFailure ? "passed" : "failed",
          localChecks: checks,
          evidenceDir: evidenceDir ?? null,
          externalEvidenceArchived: archivedEvidenceNeedles,
          externalEvidenceStillRequired: remainingReleaseEvidenceNeedles
        },
        null,
        2
      )
    );
    process.exitCode = failed.length === 0 && !strictFailure ? 0 : 1;
    return;
  }
  printHuman();
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});
