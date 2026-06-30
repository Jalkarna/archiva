import fs from "node:fs/promises";
import path from "node:path";
import {
  detectHostTarget,
  localBinaryRelativePath,
  metaPackageName,
  nativePackageRoot,
  packageBinaryRelativePath,
  requireTarget,
  supportedTargets
} from "./native-targets.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const outputDir = path.join(repoRoot, "dist-native");
const placeholderMessage = "Archiva native binary was not installed. Reinstall without --ignore-scripts and with optional dependencies enabled.";

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

async function assertReleaseBinaryExists(binaryPath) {
  try {
    await fs.access(binaryPath);
  } catch {
    throw new Error(`Missing native binary at ${binaryPath}. Run cargo build --release first.`);
  }
}

function defaultBinaryPath(target) {
  const hostRelease = path.join(repoRoot, "target", "release", target.binaryName);
  const targetRelease = path.join(repoRoot, "target", target.rustTarget, "release", target.binaryName);
  return detectHostTarget()?.key === target.key ? hostRelease : targetRelease;
}

async function copyExecutable(source, destination, target) {
  await fs.mkdir(path.dirname(destination), { recursive: true });
  await fs.copyFile(source, destination);
  if (target.platform !== "win32") {
    await fs.chmod(destination, 0o755);
  }
}

async function writePackageJson(target, version, packageDir) {
  const packageJson = {
    name: target.packageName,
    version,
    description: `Native Archiva binary for ${target.key}.`,
    type: "module",
    license: "MIT",
    author: "Jalkarna",
    homepage: "https://github.com/Jalkarna/archiva#readme",
    repository: {
      type: "git",
      url: "git+https://github.com/Jalkarna/archiva.git"
    },
    bugs: {
      url: "https://github.com/Jalkarna/archiva/issues"
    },
    os: [target.os],
    cpu: [target.cpu],
    bin: {
      archiva: `bin/${target.binaryName}`
    },
    files: ["bin", "package-manifest.json", "README.md", "LICENSE"]
  };

  if (target.libc) {
    packageJson.libc = [target.libc];
  }

  await fs.writeFile(path.join(packageDir, "package.json"), `${JSON.stringify(packageJson, null, 2)}\n`);
}

async function writeNativeReadme(target, packageDir) {
  const readme = [
    `# ${target.packageName}`,
    "",
    `Native Archiva binary for \`${target.key}\` (\`${target.rustTarget}\`).`,
    "",
    `This package is installed automatically by \`${metaPackageName}\` on matching platforms.`
  ].join("\n");
  await fs.writeFile(path.join(packageDir, "README.md"), `${readme}\n`);
}

async function copyLicense(packageDir) {
  try {
    await fs.copyFile(path.join(repoRoot, "LICENSE"), path.join(packageDir, "LICENSE"));
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }
}

async function writeNativeManifest(target, packageDir, source, output) {
  const metadata = {
    name: "archiva",
    package: target.packageName,
    target: target.rustTarget,
    key: target.key,
    libc: target.libc,
    source: path.relative(repoRoot, source),
    output: path.relative(repoRoot, output),
    platform: target.platform,
    arch: target.arch
  };

  await fs.writeFile(path.join(packageDir, "package-manifest.json"), `${JSON.stringify(metadata, null, 2)}\n`);
}

async function copyLocalBinary(source, target) {
  const localOutput = path.join(repoRoot, localBinaryRelativePath);
  await copyExecutable(source, localOutput, target);

  const packageOutput = path.join(repoRoot, packageBinaryRelativePath);
  await copyExecutable(source, packageOutput, target);

  const metadata = {
    name: "archiva",
    package: target.packageName,
    target: target.rustTarget,
    key: target.key,
    libc: target.libc,
    source: path.relative(repoRoot, source),
    output: path.relative(repoRoot, localOutput),
    packageOutput: path.relative(repoRoot, packageOutput),
    platform: target.platform,
    arch: target.arch
  };
  await fs.writeFile(path.join(outputDir, "package-manifest.json"), `${JSON.stringify(metadata, null, 2)}\n`);
}

async function stageNativePackage() {
  const rootPackage = await readJson(path.join(repoRoot, "package.json"));
  const target = requireTarget(readArg("--target"));
  const source = path.resolve(repoRoot, readArg("--binary") ?? defaultBinaryPath(target));
  const packageDir = path.resolve(repoRoot, readArg("--package-dir") ?? path.join(nativePackageRoot, target.key));
  const output = path.join(packageDir, "bin", target.binaryName);

  await assertReleaseBinaryExists(source);
  await fs.rm(packageDir, { recursive: true, force: true });
  await fs.mkdir(path.dirname(output), { recursive: true });
  await copyExecutable(source, output, target);
  await writePackageJson(target, rootPackage.version, packageDir);
  await writeNativeReadme(target, packageDir);
  await copyLicense(packageDir);
  await writeNativeManifest(target, packageDir, source, output);

  if (hasFlag("--copy-local")) {
    await copyLocalBinary(source, target);
  }

  console.log(
    JSON.stringify(
      {
        package: target.packageName,
        target: target.key,
        rustTarget: target.rustTarget,
        packageDir: path.relative(repoRoot, packageDir)
      },
      null,
      2
    )
  );
}

async function stageMetaPackage() {
  await fs.mkdir(outputDir, { recursive: true });

  const placeholder = [
    "#!/usr/bin/env node",
    `console.error(${JSON.stringify(placeholderMessage)});`,
    "process.exit(1);"
  ].join("\n");
  const output = path.join(repoRoot, packageBinaryRelativePath);
  await fs.writeFile(output, `${placeholder}\n`);
  if (process.platform !== "win32") {
    await fs.chmod(output, 0o755);
  }
  await fs.writeFile(path.join(outputDir, "package.json"), `${JSON.stringify({ type: "commonjs" }, null, 2)}\n`);

  const metadata = {
    name: "archiva",
    package: metaPackageName,
    output: packageBinaryRelativePath,
    targets: supportedTargets.map((target) => ({
      key: target.key,
      package: target.packageName,
      rustTarget: target.rustTarget,
      platform: target.platform,
      arch: target.arch,
      libc: target.libc
    }))
  };
  await fs.writeFile(path.join(outputDir, "package-manifest.json"), `${JSON.stringify(metadata, null, 2)}\n`);
}

if (hasFlag("--meta-only")) {
  await stageMetaPackage();
} else {
  await stageNativePackage();
}
