import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import {
  packageBinaryRelativePath,
  requireTarget
} from "./native-targets.mjs";

const packageRoot = path.resolve(import.meta.dirname, "..");
const requireFromPackage = createRequire(import.meta.url);

async function readJson(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function executableMagic(bytes, target) {
  if (target.platform === "win32") {
    return bytes[0] === 0x4d && bytes[1] === 0x5a;
  }
  if (target.platform === "darwin") {
    const magic = bytes.readUInt32BE(0);
    return magic === 0xcafebabe || magic === 0xcafed00d || magic === 0xfeedface || magic === 0xfeedfacf || magic === 0xcefaedfe || magic === 0xcffaedfe;
  }
  return bytes[0] === 0x7f && bytes[1] === 0x45 && bytes[2] === 0x4c && bytes[3] === 0x46;
}

async function assertNativeExecutable(file, target) {
  const handle = await fs.open(file, "r");
  try {
    const buffer = Buffer.alloc(8);
    await handle.read(buffer, 0, buffer.length, 0);
    if (!executableMagic(buffer, target)) {
      throw new Error(`Selected Archiva binary at ${file} does not look native for ${target.key}.`);
    }
  } finally {
    await handle.close();
  }
}

async function pathExists(file) {
  try {
    await fs.access(file);
    return true;
  } catch {
    return false;
  }
}

function powershellSingleQuote(value) {
  return `'${value.replaceAll("'", "''")}'`;
}

async function rewriteWindowsNpmShims(output) {
  if (process.platform !== "win32") {
    return;
  }

  const nodeModules = path.resolve(packageRoot, "..", "..");
  const candidates = [
    path.join(nodeModules, ".bin", "archiva.cmd"),
    path.join(nodeModules, ".bin", "archiva.ps1"),
    path.join(nodeModules, "..", "archiva.cmd"),
    path.join(nodeModules, "..", "archiva.ps1")
  ];
  const absoluteOutput = path.resolve(output);
  const cmdShim = [
    "@ECHO off",
    `"${absoluteOutput}" %*`,
    "EXIT /b %ERRORLEVEL%"
  ].join("\r\n") + "\r\n";
  const ps1Shim = [
    `& ${powershellSingleQuote(absoluteOutput)} @args`,
    "exit $LASTEXITCODE"
  ].join("\r\n") + "\r\n";

  for (const candidate of candidates) {
    if (!(await pathExists(candidate))) {
      continue;
    }
    await fs.writeFile(candidate, candidate.endsWith(".ps1") ? ps1Shim : cmdShim);
  }
}

function resolveNativePackage(target) {
  try {
    return requireFromPackage.resolve(`${target.packageName}/package.json`, { paths: [packageRoot] });
  } catch (error) {
    const detail = error?.code === "MODULE_NOT_FOUND" ? "" : ` ${error.message}`;
    throw new Error(
      `Missing optional native package ${target.packageName} for ${target.key}.${detail} ` +
        "Reinstall @jalkarna/archiva without --omit=optional and without --ignore-scripts."
    );
  }
}

async function isSourceCheckout() {
  try {
    await fs.access(path.join(packageRoot, "Cargo.toml"));
    return !packageRoot.split(path.sep).includes("node_modules");
  } catch {
    return false;
  }
}

async function installNative() {
  const target = requireTarget(process.env.ARCHIVA_NATIVE_TARGET);
  const rootPackage = await readJson(path.join(packageRoot, "package.json"));
  let nativePackageJsonPath;
  try {
    nativePackageJsonPath = resolveNativePackage(target);
  } catch (error) {
    if (await isSourceCheckout()) {
      console.warn(`Skipping Archiva native package selection in source checkout: ${error.message}`);
      return;
    }
    throw error;
  }
  const nativePackageRoot = path.dirname(nativePackageJsonPath);
  const nativePackage = await readJson(nativePackageJsonPath);
  const source = path.join(nativePackageRoot, nativePackage.bin?.archiva ?? `bin/${target.binaryName}`);
  const output = path.join(packageRoot, packageBinaryRelativePath);

  await assertNativeExecutable(source, target);
  await fs.mkdir(path.dirname(output), { recursive: true });
  await fs.copyFile(source, output);
  if (process.platform !== "win32") {
    await fs.chmod(output, 0o755);
  }
  await rewriteWindowsNpmShims(output);

  const result = spawnSync(output, ["--version"], { encoding: "utf8" });
  if (result.error || result.status !== 0) {
    throw new Error(`Installed Archiva binary failed --version: ${result.error?.message ?? result.stderr ?? result.stdout}`);
  }

  const versionOutput = `${result.stdout}${result.stderr}`.trim();
  if (!versionOutput.includes(rootPackage.version)) {
    throw new Error(`Installed Archiva binary reported '${versionOutput}', expected version ${rootPackage.version}.`);
  }

  const metadata = {
    name: "archiva",
    package: rootPackage.name,
    version: rootPackage.version,
    selectedPackage: nativePackage.name,
    target: target.rustTarget,
    key: target.key,
    libc: target.libc,
    source: path.relative(packageRoot, source),
    output: path.relative(packageRoot, output)
  };
  await fs.writeFile(path.join(packageRoot, "dist-native", "package-manifest.json"), `${JSON.stringify(metadata, null, 2)}\n`);
}

await installNative();
