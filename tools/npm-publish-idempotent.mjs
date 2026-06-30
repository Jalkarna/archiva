import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { requireTarget, supportedTargets } from "./native-targets.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const rootPackage = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"));

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

function hasFlag(name) {
  return process.argv.includes(name);
}

function positiveIntEnv(name, fallback) {
  const value = Number.parseInt(process.env[name] ?? "", 10);
  return Number.isFinite(value) && value > 0 ? value : fallback;
}

function nonNegativeIntEnv(name, fallback) {
  const value = Number.parseInt(process.env[name] ?? "", 10);
  return Number.isFinite(value) && value >= 0 ? value : fallback;
}

function npmCommand() {
  return process.env.ARCHIVA_NPM_COMMAND ?? (process.platform === "win32" ? "npm.cmd" : "npm");
}

function npmCommandShell(command) {
  return process.env.ARCHIVA_NPM_COMMAND_SHELL === "1" || (process.platform === "win32" && command.toLowerCase().endsWith(".cmd"));
}

function npmRun(args, stdio = "pipe") {
  const command = npmCommand();
  return spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    shell: npmCommandShell(command),
    stdio,
    env: process.env
  });
}

function expectedVersion(spec) {
  const separator = spec.lastIndexOf("@");
  if (separator <= 0 || separator === spec.length - 1) {
    throw new Error(`Published package spec must include an exact version: ${spec}`);
  }
  return spec.slice(separator + 1);
}

function viewPublishedVersion(spec) {
  const result = npmRun(["view", spec, "version"]);
  if (result.status !== 0) {
    return undefined;
  }
  return result.stdout.trim();
}

async function sleep(ms) {
  if (ms > 0) {
    await new Promise((resolve) => setTimeout(resolve, ms));
  }
}

async function waitForPublishedSpec(spec, attempts, delayMs) {
  const version = expectedVersion(spec);
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const visible = viewPublishedVersion(spec);
    if (visible === version) {
      return true;
    }
    if (visible !== undefined && visible !== version) {
      throw new Error(`npm view ${spec} returned version ${visible}, expected ${version}.`);
    }
    await sleep(delayMs);
  }
  return false;
}

async function verifyPublishedSpec(spec) {
  const attempts = positiveIntEnv("ARCHIVA_NPM_VIEW_ATTEMPTS", 12);
  const delayMs = nonNegativeIntEnv("ARCHIVA_NPM_VIEW_RETRY_DELAY_MS", 5000);
  if (!(await waitForPublishedSpec(spec, attempts, delayMs))) {
    throw new Error(`Published package ${spec} was not visible after ${attempts} npm view attempts.`);
  }
  console.log(`${spec} is published.`);
}

async function publishIdempotently(spec, packagePath, access) {
  const attempts = positiveIntEnv("ARCHIVA_NPM_VIEW_ATTEMPTS", 12);
  const delayMs = nonNegativeIntEnv("ARCHIVA_NPM_VIEW_RETRY_DELAY_MS", 5000);
  if (await waitForPublishedSpec(spec, 1, 0)) {
    console.log(`${spec} is already published; skipping.`);
    return;
  }

  const args = ["publish"];
  if (packagePath) {
    args.push(packagePath);
  }
  args.push("--access", access);
  const published = npmRun(args, "inherit");
  if (published.status === 0) {
    await verifyPublishedSpec(spec);
    return;
  }

  if (await waitForPublishedSpec(spec, attempts, delayMs)) {
    console.log(`${spec} became visible after publish failure; treating as already published.`);
    return;
  }
  process.exit(published.status ?? 1);
}

function nativePublishTarget() {
  const targetKey = readArg("--native-target");
  if (!targetKey) {
    return undefined;
  }
  const target = requireTarget(targetKey);
  return {
    spec: `${target.packageName}@${rootPackage.version}`,
    packagePath: path.join("target", "npm-packages", target.key)
  };
}

async function main() {
  const access = readArg("--access") ?? "public";
  if (hasFlag("--verify-native-targets")) {
    for (const target of supportedTargets) {
      await verifyPublishedSpec(`${target.packageName}@${rootPackage.version}`);
    }
    return;
  }

  const native = nativePublishTarget();
  const root = hasFlag("--root")
    ? { spec: `${rootPackage.name}@${rootPackage.version}`, packagePath: undefined }
    : undefined;
  const genericSpec = readArg("--spec");
  const spec = genericSpec ?? native?.spec ?? root?.spec;
  if (!spec) {
    throw new Error("Provide --spec, --native-target, --root, or --verify-native-targets.");
  }
  const packagePath = readArg("--package") ?? native?.packagePath ?? root?.packagePath;
  if (hasFlag("--verify-only")) {
    await verifyPublishedSpec(spec);
  } else {
    await publishIdempotently(spec, packagePath, access);
  }
}

await main();
