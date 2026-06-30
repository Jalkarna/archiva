import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import {
  detectHostTarget,
  metaPackageName,
  nativePackageRoot,
  nativeTarballRoot,
  packageBinaryRelativePath,
  packagePathSegments,
  requireTarget
} from "./native-targets.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const commandTimeoutMs = 300000;
const placeholderMessage = "Archiva native binary was not installed. Reinstall without --ignore-scripts and with optional dependencies enabled.";
const npmCommand = process.platform === "win32" ? "npm.cmd" : "npm";

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

async function readJson(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function spawnCommand(command, args, options = {}) {
  const { capture = false, timeoutMs = commandTimeoutMs, ...spawnOptions } = options;
  const shell = process.platform === "win32" && command.toLowerCase().endsWith(".cmd");
  return spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: capture ? "pipe" : "inherit",
    shell,
    timeout: timeoutMs,
    ...spawnOptions
  });
}

function commandOutput(result) {
  return `${result.stdout ?? ""}${result.stderr ?? ""}${result.error?.message ?? ""}`;
}

function run(command, args, options = {}) {
  const result = spawnCommand(command, args, options);
  if (result.error || result.status !== 0) {
    const output = options.capture ? `\n${commandOutput(result)}` : "";
    throw new Error(`${command} ${args.join(" ")} failed.${output}`);
  }
  return result;
}

function runFailure(command, args, options = {}) {
  const result = spawnCommand(command, args, { ...options, capture: true });
  if (!result.error && result.status === 0) {
    throw new Error(`${command} ${args.join(" ")} unexpectedly succeeded.\n${commandOutput(result)}`);
  }
  return result;
}

async function assertExists(file) {
  try {
    await fs.access(file);
  } catch {
    throw new Error(`Missing expected file: ${file}`);
  }
}

async function stageTarget(target) {
  const packageDir = path.join(repoRoot, nativePackageRoot, target.key);
  const packageJson = path.join(packageDir, "package.json");
  try {
    await fs.access(packageJson);
  } catch {
    run(process.execPath, ["tools/stage-native-package.mjs", "--target", target.key]);
  }
  return packageDir;
}

async function packDirectory(packageDir, destination) {
  await fs.mkdir(destination, { recursive: true });
  const before = new Set(await fs.readdir(destination).catch(() => []));
  const result = run(npmCommand, ["pack", packageDir, "--pack-destination", destination, "--silent"], { capture: true });
  const packedName = result.stdout.trim().split(/\r?\n/).filter(Boolean).pop();
  if (packedName) {
    const packed = path.resolve(destination, packedName);
    await assertExists(packed);
    return packed;
  }

  const after = await fs.readdir(destination);
  const created = after.find((entry) => entry.endsWith(".tgz") && !before.has(entry));
  if (!created) {
    throw new Error(`npm pack did not produce a tarball in ${destination}.`);
  }
  return path.join(destination, created);
}

function globalBin(prefix) {
  return process.platform === "win32" ? path.join(prefix, "archiva.cmd") : path.join(prefix, "bin", "archiva");
}

function globalPackageRoot(prefix, packageName) {
  const base = process.platform === "win32" ? path.join(prefix, "node_modules") : path.join(prefix, "lib", "node_modules");
  return path.join(base, ...packagePathSegments(packageName));
}

function installedBin(prefix) {
  return globalBin(prefix);
}

function runInstalled(prefix, args, options = {}) {
  const command = globalBin(prefix);
  const result = run(command, args, { capture: true, ...options });
  return `${result.stdout}${result.stderr}`.trim();
}

function installNeedsForce(target) {
  return detectHostTarget()?.key !== target.key;
}

async function installAndRun(tarball, packageName, target, env = {}) {
  const prefix = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-install-"));
  const args = [
    "install",
    "-g",
    "--prefix",
    prefix,
    tarball,
    "--foreground-scripts",
    "--include=optional",
    "--ignore-scripts=false",
    "--loglevel=warn"
  ];
  if (installNeedsForce(target)) {
    args.push("--force");
  }
  run(npmCommand, args, { env: { ...process.env, ...env } });
  const output = runInstalled(prefix, ["--version"]);
  const installedRoot = globalPackageRoot(prefix, packageName);
  return { prefix, output, installedRoot };
}

function isRegistryMetaSpec(spec) {
  return spec.startsWith(`${metaPackageName}@`);
}

async function sleep(ms) {
  await new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

async function waitForPublishedSpec(spec) {
  if (!isRegistryMetaSpec(spec)) {
    return;
  }

  let lastOutput = "";
  for (let attempt = 1; attempt <= 6; attempt += 1) {
    const result = spawnCommand(npmCommand, ["view", spec, "version", "--silent"], { capture: true });
    if (!result.error && result.status === 0) {
      return;
    }
    lastOutput = commandOutput(result);
    if (attempt < 6) {
      await sleep(attempt * 10_000);
    }
  }
  throw new Error(`Published package ${spec} was not visible in npm after retries.\n${lastOutput}`);
}

async function installPublishedMetaPackage(spec, target, additionalSpecs = []) {
  await waitForPublishedSpec(spec);
  const prefix = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-published-"));
  const args = [
    "install",
    "-g",
    "--prefix",
    prefix,
    spec,
    ...additionalSpecs,
    "--foreground-scripts",
    "--include=optional",
    "--ignore-scripts=false",
    "--loglevel=warn"
  ];
  if (installNeedsForce(target)) {
    args.push("--force");
  }
  run(npmCommand, args, { env: { ...process.env, ARCHIVA_NATIVE_TARGET: target.key } });
  return {
    prefix,
    installedRoot: globalPackageRoot(prefix, metaPackageName)
  };
}

async function assertPublishedSpecBehavior(spec, expectedVersion, target, additionalSpecs = []) {
  const install = await installPublishedMetaPackage(spec, target, additionalSpecs);
  await assertInstalledMetaManifest(install.installedRoot, target, expectedVersion);
  await assertInstalledMetaBinary(install.installedRoot);
  await assertInstalledCliBehavior(install.prefix, expectedVersion);
}

async function installWithoutRunningScripts(tarball, target) {
  const prefix = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-install-"));
  const args = ["install", "-g", "--prefix", prefix, tarball, "--ignore-scripts", "--loglevel=warn"];
  if (installNeedsForce(target)) {
    args.push("--force");
  }
  run(npmCommand, args);
  return prefix;
}

async function writePlaceholder(root) {
  const output = path.join(root, packageBinaryRelativePath);
  await fs.mkdir(path.dirname(output), { recursive: true });
  await fs.writeFile(
    output,
    [
      "#!/usr/bin/env node",
      `console.error(${JSON.stringify(placeholderMessage)});`,
      "process.exit(1);"
    ].join("\n") + "\n"
  );
  if (process.platform !== "win32") {
    await fs.chmod(output, 0o755);
  }
  await fs.writeFile(path.join(root, "dist-native", "package.json"), `${JSON.stringify({ type: "commonjs" }, null, 2)}\n`);
  await fs.writeFile(
    path.join(root, "dist-native", "package-manifest.json"),
    `${JSON.stringify({ name: "archiva", package: metaPackageName, output: packageBinaryRelativePath }, null, 2)}\n`
  );
}

async function copyIfExists(source, destination) {
  try {
    await fs.cp(source, destination, { recursive: true });
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }
}

async function createMetaFixture(target, nativeTarball, options = {}) {
  const rootPackage = await readJson(path.join(repoRoot, "package.json"));
  const fixture = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-meta-"));
  const includeNative = options.includeNative ?? true;
  const localNativeDependency = includeNative ? { [target.packageName]: `file:${nativeTarball}` } : {};
  const packageJson = {
    ...rootPackage,
    optionalDependencies: installNeedsForce(target) ? {} : localNativeDependency,
    dependencies: installNeedsForce(target) ? localNativeDependency : rootPackage.dependencies,
    scripts: {
      postinstall: "node tools/install-native.mjs"
    }
  };

  await fs.writeFile(path.join(fixture, "package.json"), `${JSON.stringify(packageJson, null, 2)}\n`);
  await copyIfExists(path.join(repoRoot, "README.md"), path.join(fixture, "README.md"));
  await copyIfExists(path.join(repoRoot, "LICENSE"), path.join(fixture, "LICENSE"));
  await copyIfExists(path.join(repoRoot, "schema"), path.join(fixture, "schema"));
  await fs.mkdir(path.join(fixture, "tools"), { recursive: true });
  await fs.copyFile(path.join(repoRoot, "tools", "install-native.mjs"), path.join(fixture, "tools", "install-native.mjs"));
  await fs.copyFile(path.join(repoRoot, "tools", "native-targets.mjs"), path.join(fixture, "tools", "native-targets.mjs"));
  await writePlaceholder(fixture);
  return fixture;
}

async function assertInstalledMetaBinary(installedRoot) {
  const installedBinary = path.join(installedRoot, packageBinaryRelativePath);
  await assertExists(installedBinary);
  const handle = await fs.open(installedBinary, "r");
  try {
    const buffer = Buffer.alloc(4);
    await handle.read(buffer, 0, buffer.length, 0);
    const isNative =
      (buffer[0] === 0x7f && buffer[1] === 0x45 && buffer[2] === 0x4c && buffer[3] === 0x46) ||
      (buffer[0] === 0x4d && buffer[1] === 0x5a) ||
      buffer.readUInt32BE(0) === 0xcafebabe ||
      buffer.readUInt32BE(0) === 0xfeedface ||
      buffer.readUInt32BE(0) === 0xfeedfacf ||
      buffer.readUInt32BE(0) === 0xcefaedfe ||
      buffer.readUInt32BE(0) === 0xcffaedfe;
    if (!isNative) {
      throw new Error(`Installed meta-package binary is not native: ${installedBinary}`);
    }
  } finally {
    await handle.close();
  }
}

function normalizeManifestPath(value) {
  return String(value ?? "").replaceAll("\\", "/");
}

async function assertInstalledMetaManifest(installedRoot, target, expectedVersion) {
  const manifest = await readJson(path.join(installedRoot, "dist-native", "package-manifest.json"));
  const packageLeaf = packagePathSegments(target.packageName).at(-1);
  const source = normalizeManifestPath(manifest.source);
  const output = normalizeManifestPath(manifest.output);

  const expected = {
    name: "archiva",
    package: metaPackageName,
    version: expectedVersion,
    selectedPackage: target.packageName,
    target: target.rustTarget,
    key: target.key,
    libc: target.libc
  };
  for (const [field, value] of Object.entries(expected)) {
    if (manifest[field] !== value) {
      throw new Error(`Installed meta manifest ${field} was ${JSON.stringify(manifest[field])}, expected ${JSON.stringify(value)}.`);
    }
  }
  if (output !== normalizeManifestPath(packageBinaryRelativePath)) {
    throw new Error(`Installed meta manifest output was ${JSON.stringify(manifest.output)}, expected ${packageBinaryRelativePath}.`);
  }
  if (!source.includes(packageLeaf) || !source.endsWith(`/bin/${target.binaryName}`)) {
    throw new Error(`Installed meta manifest source ${JSON.stringify(manifest.source)} does not point at ${target.packageName}.`);
  }
}

async function assertInstalledCliBehavior(prefix, expectedVersion) {
  const project = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-installed-cli-"));
  await fs.mkdir(path.join(project, "src"), { recursive: true });
  await fs.writeFile(
    path.join(project, "src", "a.ts"),
    "export function makeThing() {\n  return 1;\n}\n",
    "utf8"
  );

  const version = runInstalled(prefix, ["--version"]);
  if (!version.includes(expectedVersion)) {
    throw new Error(`Installed package reported '${version}', expected ${expectedVersion}.`);
  }

  runInstalled(prefix, ["init"], { cwd: project });
  const decision = {
    file: "src/a.ts",
    anchor: "fn:makeThing",
    lines: [1, 3],
    chose: "installed package smoke decision",
    because: "package smoke must prove real CLI behavior",
    rejected: [{ approach: "version-only smoke", reason: "does not exercise storage or source parsing" }]
  };
  const write = runInstalled(prefix, ["write-decision"], { cwd: project, input: JSON.stringify(decision) });
  if (!write.includes("Recorded dec_001")) {
    throw new Error(`Installed write-decision returned '${write}'.`);
  }
  const why = runInstalled(prefix, ["why", "src/a.ts", "fn:makeThing"], { cwd: project });
  if (!why.includes("installed package smoke decision")) {
    throw new Error(`Installed why did not return the recorded decision: '${why}'.`);
  }
  const lint = runInstalled(prefix, ["lint"], { cwd: project });
  if (lint !== "No decision issues found.") {
    throw new Error(`Installed lint returned '${lint}'.`);
  }

  const mcpInput = [
    JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2024-11-05",
        capabilities: {},
        clientInfo: { name: "archiva-package-smoke", version: "0" }
      }
    }),
    JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }),
    ""
  ].join("\n");
  const mcp = run(installedBin(prefix), ["mcp"], { cwd: project, capture: true, input: mcpInput });
  const responses = `${mcp.stdout ?? ""}`
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  const toolsList = responses.find((response) => response.id === 2);
  const tools = toolsList?.result?.tools ?? [];
  const toolNames = new Set(tools.map((tool) => tool.name));
  for (const expected of ["why", "write_decision", "ghost_check"]) {
    if (!toolNames.has(expected)) {
      throw new Error(`Installed MCP tools/list missing ${expected}: ${mcp.stdout}`);
    }
  }
}

async function assertIgnoredScriptsPlaceholder(metaTarball, target) {
  const prefix = await installWithoutRunningScripts(metaTarball, target);
  const result = runFailure(installedBin(prefix), ["--version"]);
  const output = commandOutput(result);
  if (!output.includes(placeholderMessage)) {
    throw new Error(`Ignored-scripts install did not report the placeholder message: '${output}'.`);
  }
}

async function assertMissingNativePackageFails(target, tarballDir) {
  const fixture = await createMetaFixture(target, undefined, { includeNative: false });
  const tarball = await packDirectory(fixture, tarballDir);
  const prefix = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-missing-native-"));
  const args = ["install", "-g", "--prefix", prefix, tarball, "--foreground-scripts", "--loglevel=warn"];
  if (installNeedsForce(target)) {
    args.push("--force");
  }
  const result = runFailure(npmCommand, args, { env: { ...process.env, ARCHIVA_NATIVE_TARGET: target.key } });
  const output = commandOutput(result);
  for (const expected of ["Missing optional native package", target.packageName]) {
    if (!output.includes(expected)) {
      throw new Error(`Missing-native install failure did not include '${expected}': '${output}'.`);
    }
  }
}

async function smokePublishedPackage() {
  const spec = readArg("--published-spec");
  if (!spec) {
    return false;
  }

  const rootPackage = await readJson(path.join(repoRoot, "package.json"));
  const expectedVersion = readArg("--expected-version") ?? rootPackage.version;
  const target = requireTarget(readArg("--target") ?? process.env.ARCHIVA_NATIVE_TARGET);
  await assertPublishedSpecBehavior(spec, expectedVersion, target);

  console.log(
    JSON.stringify(
      {
        status: "passed",
        package: spec,
        target: target.key,
        mode: "published"
      },
      null,
      2
    )
  );
  return true;
}

async function smoke() {
  if (await smokePublishedPackage()) {
    return;
  }

  const target = requireTarget(readArg("--target"));
  const packageDir = await stageTarget(target);
  run(process.execPath, ["tools/validate-native-package-metadata.mjs", "--staged-target", target.key]);
  const tarballDir = path.join(repoRoot, nativeTarballRoot);
  const nativeTarball = await packDirectory(packageDir, tarballDir);
  const fixtureTarballDir = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-fixture-tarballs-"));
  const rootPackage = await readJson(path.join(repoRoot, "package.json"));

  const nativeInstall = await installAndRun(nativeTarball, target.packageName, target);
  if (!nativeInstall.output.includes(rootPackage.version)) {
    throw new Error(`Native package reported '${nativeInstall.output}', expected ${rootPackage.version}.`);
  }
  await assertInstalledCliBehavior(nativeInstall.prefix, rootPackage.version);

  if (!hasFlag("--native-only")) {
    const rootTarball = await packDirectory(repoRoot, fixtureTarballDir);
    await assertPublishedSpecBehavior(rootTarball, rootPackage.version, target, [path.resolve(nativeTarball)]);

    const metaFixture = await createMetaFixture(target, nativeTarball);
    const metaTarball = await packDirectory(metaFixture, fixtureTarballDir);
    const metaInstall = await installAndRun(metaTarball, metaPackageName, target, { ARCHIVA_NATIVE_TARGET: target.key });
    if (!metaInstall.output.includes(rootPackage.version)) {
      throw new Error(`Meta package reported '${metaInstall.output}', expected ${rootPackage.version}.`);
    }
    await assertInstalledMetaManifest(metaInstall.installedRoot, target, rootPackage.version);
    await assertInstalledMetaBinary(metaInstall.installedRoot);
    await assertInstalledCliBehavior(metaInstall.prefix, rootPackage.version);
    await assertPublishedSpecBehavior(metaTarball, rootPackage.version, target);
    await assertIgnoredScriptsPlaceholder(metaTarball, target);
    await assertMissingNativePackageFails(target, fixtureTarballDir);
  }

  console.log(
    JSON.stringify(
      {
        status: "passed",
        target: target.key,
        nativeTarball: path.relative(repoRoot, nativeTarball)
      },
      null,
      2
    )
  );
}

await smoke();
