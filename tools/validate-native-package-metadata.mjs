import fs from "node:fs/promises";
import path from "node:path";
import {
  findTarget,
  metaPackageName,
  nativePackageRoot,
  optionalDependencyMap,
  packageBinaryRelativePath,
  packagePathSegments,
  requireTarget,
  supportedTargets
} from "./native-targets.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const expectedRootFiles = [
  "dist-native/archiva.exe",
  "dist-native/package.json",
  "dist-native/package-manifest.json",
  "schema",
  "tools/install-native.mjs",
  "tools/native-targets.mjs",
  "README.md",
  "LICENSE"
];
const expectedPrepackScript = "node tools/stage-native-package.mjs --meta-only && node tools/validate-native-package-metadata.mjs --meta-package";
const expectedCheckPackageScript = `node tools/validate-native-package-metadata.mjs && ${expectedPrepackScript}`;
const expectedPlaceholderMessage = "Archiva native binary was not installed. Reinstall without --ignore-scripts and with optional dependencies enabled.";

function hasFlag(name) {
  return process.argv.includes(name);
}

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

async function readJson(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function assertEqual(actual, expected, label) {
  if (stableJson(actual) !== stableJson(expected)) {
    throw new Error(`${label} mismatch.\nExpected: ${JSON.stringify(expected)}\nActual:   ${JSON.stringify(actual)}`);
  }
}

function stableJson(value) {
  if (Array.isArray(value)) {
    return `[${value.map(stableJson).join(",")}]`;
  }
  if (value && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
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

function lockPackagePath(packageName) {
  return path.join("node_modules", ...packagePathSegments(packageName)).replaceAll(path.sep, "/");
}

function readUInt16(buffer, offset, littleEndian) {
  return littleEndian ? buffer.readUInt16LE(offset) : buffer.readUInt16BE(offset);
}

function readUInt32(buffer, offset, littleEndian) {
  return littleEndian ? buffer.readUInt32LE(offset) : buffer.readUInt32BE(offset);
}

function readUInt64Number(buffer, offset, littleEndian) {
  const value = littleEndian ? buffer.readBigUInt64LE(offset) : buffer.readBigUInt64BE(offset);
  assert(value <= BigInt(Number.MAX_SAFE_INTEGER), `ELF offset ${value} exceeds JavaScript safe integer range.`);
  return Number(value);
}

function elfMachineForTarget(target) {
  if (target.arch === "x64") {
    return 62;
  }
  if (target.arch === "arm64") {
    return 183;
  }
  throw new Error(`No ELF machine expectation for ${target.key}.`);
}

function peMachineForTarget(target) {
  if (target.arch === "x64") {
    return 0x8664;
  }
  if (target.arch === "arm64") {
    return 0xaa64;
  }
  throw new Error(`No PE machine expectation for ${target.key}.`);
}

function machoCpuTypeForTarget(target) {
  if (target.arch === "x64") {
    return 0x01000007;
  }
  if (target.arch === "arm64") {
    return 0x0100000c;
  }
  throw new Error(`No Mach-O CPU type expectation for ${target.key}.`);
}

function hex(value) {
  return `0x${value.toString(16)}`;
}

function elfInterpreter(buffer) {
  assert(buffer.length >= 64, "ELF binary is too small to contain a 64-bit header.");
  assert(buffer[4] === 2, "ELF binary must be 64-bit.");
  const littleEndian = buffer[5] === 1;
  assert(littleEndian || buffer[5] === 2, "ELF binary must declare byte order.");

  const programHeaderOffset = readUInt64Number(buffer, 32, littleEndian);
  const programHeaderEntrySize = readUInt16(buffer, 54, littleEndian);
  const programHeaderCount = readUInt16(buffer, 56, littleEndian);

  for (let index = 0; index < programHeaderCount; index += 1) {
    const headerOffset = programHeaderOffset + index * programHeaderEntrySize;
    if (headerOffset + programHeaderEntrySize > buffer.length) {
      break;
    }
    const programType = readUInt32(buffer, headerOffset, littleEndian);
    if (programType !== 3) {
      continue;
    }
    const interpreterOffset = readUInt64Number(buffer, headerOffset + 8, littleEndian);
    const interpreterSize = readUInt64Number(buffer, headerOffset + 32, littleEndian);
    assert(interpreterOffset + interpreterSize <= buffer.length, "ELF interpreter path extends past binary size.");
    return buffer.subarray(interpreterOffset, interpreterOffset + interpreterSize).toString("utf8").replace(/\0.*$/s, "");
  }

  return null;
}

function peMachine(buffer) {
  assert(buffer.length >= 0x40, "PE binary is too small to contain a DOS header.");
  assert(buffer[0] === 0x4d && buffer[1] === 0x5a, "PE binary must start with an MZ header.");
  const peOffset = buffer.readUInt32LE(0x3c);
  assert(peOffset + 26 <= buffer.length, "PE header extends past binary size.");
  assert(buffer.toString("latin1", peOffset, peOffset + 4) === "PE\0\0", "PE binary must contain a PE signature.");
  const machine = buffer.readUInt16LE(peOffset + 4);
  const optionalHeaderSize = buffer.readUInt16LE(peOffset + 20);
  const optionalHeaderOffset = peOffset + 24;
  assert(optionalHeaderSize >= 2, "PE binary must contain an optional header.");
  assert(optionalHeaderOffset + optionalHeaderSize <= buffer.length, "PE optional header extends past binary size.");
  const optionalHeaderMagic = buffer.readUInt16LE(optionalHeaderOffset);
  assert(optionalHeaderMagic === 0x20b, `PE binary must be PE32+, got optional header ${hex(optionalHeaderMagic)}.`);
  return machine;
}

function machoThinCpuType(buffer, offset, magic) {
  const littleEndian = magic === 0xcefaedfe || magic === 0xcffaedfe;
  const is64Bit = magic === 0xfeedfacf || magic === 0xcffaedfe;
  assert(is64Bit, "Mach-O binary must be 64-bit for Archiva native targets.");
  assert(offset + 8 <= buffer.length, "Mach-O header is truncated before cputype.");
  return readUInt32(buffer, offset + 4, littleEndian);
}

function machoCpuTypes(buffer) {
  assert(buffer.length >= 8, "Mach-O binary is too small to contain a header.");
  const magic = buffer.readUInt32BE(0);
  if ([0xfeedface, 0xfeedfacf, 0xcefaedfe, 0xcffaedfe].includes(magic)) {
    return [machoThinCpuType(buffer, 0, magic)];
  }

  const fatEndianByMagic = new Map([
    [0xcafebabe, false],
    [0xbebafeca, true],
    [0xcafebabf, false],
    [0xbfbafeca, true]
  ]);
  assert(fatEndianByMagic.has(magic), `Mach-O binary has unsupported magic ${hex(magic)}.`);
  const littleEndian = fatEndianByMagic.get(magic);
  const isFat64 = magic === 0xcafebabf || magic === 0xbfbafeca;
  const entrySize = isFat64 ? 32 : 20;
  const count = readUInt32(buffer, 4, littleEndian);
  assert(count > 0, "Fat Mach-O binary must contain at least one architecture slice.");
  assert(8 + count * entrySize <= buffer.length, "Fat Mach-O architecture table extends past binary size.");

  const cpuTypes = [];
  for (let index = 0; index < count; index += 1) {
    const entryOffset = 8 + index * entrySize;
    const entryCpuType = readUInt32(buffer, entryOffset, littleEndian);
    const sliceOffset = isFat64 ? readUInt64Number(buffer, entryOffset + 8, littleEndian) : readUInt32(buffer, entryOffset + 8, littleEndian);
    const sliceSize = isFat64 ? readUInt64Number(buffer, entryOffset + 16, littleEndian) : readUInt32(buffer, entryOffset + 12, littleEndian);
    assert(sliceSize >= 8, "Fat Mach-O slice is too small to contain a nested header.");
    assert(sliceOffset + sliceSize <= buffer.length, "Fat Mach-O slice extends past binary size.");
    const nestedMagic = buffer.readUInt32BE(sliceOffset);
    const nestedCpuType = machoThinCpuType(buffer, sliceOffset, nestedMagic);
    assert(
      nestedCpuType === entryCpuType,
      `Fat Mach-O slice CPU ${hex(nestedCpuType)} does not match table CPU ${hex(entryCpuType)}.`
    );
    cpuTypes.push(nestedCpuType);
  }
  return cpuTypes;
}

function minimalPe(machine) {
  const buffer = Buffer.alloc(0x110);
  buffer[0] = 0x4d;
  buffer[1] = 0x5a;
  buffer.writeUInt32LE(0x80, 0x3c);
  buffer.write("PE\0\0", 0x80, "latin1");
  buffer.writeUInt16LE(machine, 0x84);
  buffer.writeUInt16LE(0x70, 0x94);
  buffer.writeUInt16LE(0x20b, 0x98);
  return buffer;
}

function minimalMachO(cpuType) {
  const buffer = Buffer.alloc(32);
  buffer.writeUInt32BE(0xcffaedfe, 0);
  buffer.writeUInt32LE(cpuType, 4);
  return buffer;
}

function minimalFatMachO(cpuTypes) {
  const entrySize = 20;
  const headerSize = 8 + cpuTypes.length * entrySize;
  const sliceSize = 32;
  const buffer = Buffer.alloc(headerSize + cpuTypes.length * sliceSize);
  buffer.writeUInt32BE(0xcafebabe, 0);
  buffer.writeUInt32BE(cpuTypes.length, 4);
  for (let index = 0; index < cpuTypes.length; index += 1) {
    const entryOffset = 8 + index * entrySize;
    const sliceOffset = headerSize + index * sliceSize;
    buffer.writeUInt32BE(cpuTypes[index], entryOffset);
    buffer.writeUInt32BE(sliceOffset, entryOffset + 8);
    buffer.writeUInt32BE(sliceSize, entryOffset + 12);
    minimalMachO(cpuTypes[index]).copy(buffer, sliceOffset);
  }
  return buffer;
}

function validateBinaryFormatParsers() {
  assert(peMachine(minimalPe(0x8664)) === 0x8664, "PE parser self-check failed for x64.");
  assert(peMachine(minimalPe(0xaa64)) === 0xaa64, "PE parser self-check failed for arm64.");
  assert(machoCpuTypes(minimalMachO(0x01000007))[0] === 0x01000007, "Mach-O parser self-check failed for thin x64.");
  assert(machoCpuTypes(minimalMachO(0x0100000c))[0] === 0x0100000c, "Mach-O parser self-check failed for thin arm64.");
  assert(
    machoCpuTypes(minimalFatMachO([0x0100000c, 0x01000007])).includes(0x01000007),
    "Mach-O parser self-check failed for fat x64 lookup."
  );
}

async function validateStagedBinary(target, binaryPath) {
  const buffer = await fs.readFile(binaryPath);

  if (target.platform === "linux") {
    assert(
      buffer[0] === 0x7f && buffer[1] === 0x45 && buffer[2] === 0x4c && buffer[3] === 0x46,
      `${target.key} staged binary must be an ELF executable.`
    );
    const littleEndian = buffer[5] === 1;
    const machine = readUInt16(buffer, 18, littleEndian);
    assert(machine === elfMachineForTarget(target), `${target.key} ELF machine ${machine} does not match ${target.arch}.`);
    const interpreter = elfInterpreter(buffer);
    if (target.libc === "glibc") {
      assert(interpreter?.includes("ld-linux"), `${target.key} ELF interpreter must identify glibc ld-linux, got ${interpreter ?? "<static>"}.`);
    } else {
      assert(!interpreter || interpreter.includes("ld-musl"), `${target.key} ELF interpreter must be musl or static, got ${interpreter}.`);
    }
    return;
  }

  if (target.platform === "win32") {
    const machine = peMachine(buffer);
    assert(machine === peMachineForTarget(target), `${target.key} PE machine ${hex(machine)} does not match ${target.arch}.`);
    return;
  }

  if (target.platform === "darwin") {
    const cpuTypes = machoCpuTypes(buffer);
    const expected = machoCpuTypeForTarget(target);
    assert(
      cpuTypes.includes(expected),
      `${target.key} Mach-O CPU types ${cpuTypes.map(hex).join(", ")} do not include ${target.arch}.`
    );
  }
}

async function validateRootPackage() {
  const packageJson = await readJson(path.join(repoRoot, "package.json"));
  const packageLock = await readJson(path.join(repoRoot, "package-lock.json"));
  const expectedOptional = optionalDependencyMap(packageJson.version);

  assert(packageJson.name === metaPackageName, `Root package name must be ${metaPackageName}.`);
  assertEqual(packageJson.bin, { archiva: packageBinaryRelativePath }, "Root package bin");
  assertEqual(packageJson.files, expectedRootFiles, "Root package files");
  assertEqual(packageJson.dependencies ?? {}, {}, "Root package runtime dependencies");
  assertEqual(packageJson.optionalDependencies ?? {}, expectedOptional, "Root package optionalDependencies");
  assert(packageJson.scripts?.postinstall === "node tools/install-native.mjs", "Root package postinstall must run tools/install-native.mjs.");
  assert(packageJson.scripts?.["check:package"] === expectedCheckPackageScript, "Root package must expose check:package with meta-package artifact validation.");
  assert(packageJson.scripts?.prepack === expectedPrepackScript, "Root package prepack must stage and validate meta-package artifacts.");
  assert(packageJson.engines?.node === ">=20.11", "Root package must declare Node >=20.11 for import.meta.dirname tooling.");

  const lockRoot = packageLock.packages?.[""];
  assert(lockRoot, "package-lock.json must contain a root package entry.");
  assert(lockRoot.name === packageJson.name, "Lockfile root name must match package.json.");
  assert(lockRoot.version === packageJson.version, "Lockfile root version must match package.json.");
  assertEqual(lockRoot.bin, packageJson.bin, "Lockfile root bin");
  assertEqual(lockRoot.engines, packageJson.engines, "Lockfile root engines");
  assert(lockRoot.hasInstallScript === true, "Lockfile root must record the postinstall script.");
  assertEqual(lockRoot.dependencies ?? {}, {}, "Lockfile root runtime dependencies");
  assertEqual(lockRoot.optionalDependencies ?? {}, expectedOptional, "Lockfile root optionalDependencies");

  for (const target of supportedTargets) {
    const lockPath = lockPackagePath(target.packageName);
    const entry = packageLock.packages?.[lockPath];
    assert(entry, `package-lock.json missing optional native package entry ${lockPath}.`);
    assert(entry.optional === true, `package-lock.json entry ${lockPath} must be optional.`);
  }
}

function validateSupportedTargets() {
  const seen = {
    key: new Set(),
    packageName: new Set(),
    rustTarget: new Set(),
    platformTuple: new Set()
  };

  for (const target of supportedTargets) {
    for (const field of ["key", "packageName", "rustTarget"]) {
      const value = target[field];
      assert(value && typeof value === "string", `Target ${target.key ?? "<unknown>"} must declare ${field}.`);
      assert(!seen[field].has(value), `Duplicate native target ${field}: ${value}.`);
      seen[field].add(value);
    }

    const expectedPackageName = `${metaPackageName}-${target.key}`;
    assert(target.packageName === expectedPackageName, `${target.key} packageName must be ${expectedPackageName}.`);
    assert(target.os === target.platform, `${target.key} os must match platform.`);
    assert(target.cpu === target.arch, `${target.key} cpu must match arch.`);
    assert(target.runner && typeof target.runner === "string", `${target.key} must declare a CI runner.`);

    const platformTuple = [target.platform, target.arch, target.libc ?? ""].join("/");
    assert(!seen.platformTuple.has(platformTuple), `Duplicate native target platform tuple: ${platformTuple}.`);
    seen.platformTuple.add(platformTuple);

    if (target.platform === "linux") {
      assert(["glibc", "musl"].includes(target.libc), `${target.key} linux target must declare glibc or musl libc.`);
      assert(target.binaryName === "archiva", `${target.key} linux binary must be archiva.`);
    } else {
      assert(target.libc === undefined, `${target.key} non-linux target must not declare libc.`);
      assert(target.binaryName === (target.platform === "win32" ? "archiva.exe" : "archiva"), `${target.key} binaryName mismatch.`);
    }
  }
}

async function validateStagedPackage(target) {
  const packageDir = path.join(repoRoot, nativePackageRoot, target.key);
  const packageJsonPath = path.join(packageDir, "package.json");
  const manifestPath = path.join(packageDir, "package-manifest.json");
  const packageJson = await readJson(packageJsonPath);
  const manifest = await readJson(manifestPath);
  const rootPackage = await readJson(path.join(repoRoot, "package.json"));

  assert(packageJson.name === target.packageName, `${target.key} package name must be ${target.packageName}.`);
  assert(packageJson.version === rootPackage.version, `${target.key} package version must match root package.`);
  assertEqual(packageJson.os, [target.os], `${target.key} os`);
  assertEqual(packageJson.cpu, [target.cpu], `${target.key} cpu`);
  if (target.libc) {
    assertEqual(packageJson.libc, [target.libc], `${target.key} libc`);
  } else {
    assert(packageJson.libc === undefined, `${target.key} must not declare libc.`);
  }
  assertEqual(packageJson.bin, { archiva: `bin/${target.binaryName}` }, `${target.key} bin`);
  assertEqual(packageJson.files, ["bin", "package-manifest.json", "README.md", "LICENSE"], `${target.key} files`);

  assert(manifest.package === target.packageName, `${target.key} manifest package mismatch.`);
  assert(manifest.target === target.rustTarget, `${target.key} manifest Rust target mismatch.`);
  assert(manifest.key === target.key, `${target.key} manifest key mismatch.`);
  assert(manifest.platform === target.platform, `${target.key} manifest platform mismatch.`);
  assert(manifest.arch === target.arch, `${target.key} manifest arch mismatch.`);
  assert(manifest.libc === target.libc, `${target.key} manifest libc mismatch.`);
  assert(manifest.output === path.join(nativePackageRoot, target.key, "bin", target.binaryName), `${target.key} manifest output mismatch.`);

  const binaryPath = path.join(packageDir, "bin", target.binaryName);
  assert(await pathExists(binaryPath), `${target.key} staged binary is missing.`);
  await validateStagedBinary(target, binaryPath);
}

async function validateExistingStagedPackages() {
  const root = path.join(repoRoot, nativePackageRoot);
  let entries = [];
  try {
    entries = await fs.readdir(root, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") {
      return [];
    }
    throw error;
  }

  const validated = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) {
      continue;
    }
    const target = findTarget(entry.name);
    assert(target, `Unexpected staged native package directory ${path.join(nativePackageRoot, entry.name)}.`);
    if (await pathExists(path.join(root, entry.name, "package.json"))) {
      await validateStagedPackage(target);
      validated.push(target.key);
    }
  }
  return validated;
}

async function validateMetaPackageArtifacts() {
  const packageJson = await readJson(path.join(repoRoot, "package.json"));
  const binaryPath = path.join(repoRoot, packageBinaryRelativePath);
  const binaryPackagePath = path.join(repoRoot, "dist-native", "package.json");
  const manifestPath = path.join(repoRoot, "dist-native", "package-manifest.json");

  for (const included of packageJson.files) {
    assert(await pathExists(path.join(repoRoot, included)), `Root package included path is missing: ${included}.`);
  }

  const placeholder = await fs.readFile(binaryPath);
  const placeholderText = placeholder.toString("utf8");
  assert(
    placeholderText.startsWith("#!/usr/bin/env node\n"),
    "Root meta package bin placeholder must be directly executable through Node/npm shims."
  );
  assert(
    placeholderText.includes(expectedPlaceholderMessage) &&
      placeholderText.includes("console.error(") &&
      placeholderText.includes("process.exit(1)"),
    "Root meta package bin must be the install-time placeholder before packing."
  );
  assert(
    !(placeholder[0] === 0x7f && placeholder[1] === 0x45 && placeholder[2] === 0x4c && placeholder[3] === 0x46) &&
      !(placeholder[0] === 0x4d && placeholder[1] === 0x5a),
    "Root meta package bin must not contain a native host binary."
  );

  const binaryPackage = await readJson(binaryPackagePath);
  assertEqual(binaryPackage, { type: "commonjs" }, "Root meta binary package marker");

  const manifest = await readJson(manifestPath);
  assert(manifest.name === "archiva", "Root meta manifest name mismatch.");
  assert(manifest.package === metaPackageName, "Root meta manifest package mismatch.");
  assert(manifest.output === packageBinaryRelativePath, "Root meta manifest output mismatch.");
  assertEqual(
    manifest.targets,
    supportedTargets.map((target) => {
      const entry = {
        key: target.key,
        package: target.packageName,
        rustTarget: target.rustTarget,
        platform: target.platform,
        arch: target.arch
      };
      if (target.libc) {
        entry.libc = target.libc;
      }
      return entry;
    }),
    "Root meta manifest targets"
  );
}

function countIndent(line) {
  let count = 0;
  while (line[count] === " ") {
    count += 1;
  }
  return count;
}

function parseWorkflowScalar(value) {
  const trimmed = value.trim().replace(/\s+#.*$/, "");
  if ((trimmed.startsWith("\"") && trimmed.endsWith("\"")) || (trimmed.startsWith("'") && trimmed.endsWith("'"))) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function workflowJobBlock(file, text, jobName) {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((line) => line === `  ${jobName}:`);
  assert(start !== -1, `${file} missing job ${jobName}.`);

  let end = lines.length;
  for (let index = start + 1; index < lines.length; index += 1) {
    if (/^  [A-Za-z0-9_-]+:\s*$/.test(lines[index])) {
      end = index;
      break;
    }
  }

  return lines.slice(start, end).map((line, index) => ({
    line,
    number: start + index + 1
  }));
}

function workflowMatrixInclude(file, text, jobName) {
  const block = workflowJobBlock(file, text, jobName);
  const includeIndex = block.findIndex((item) => /^\s*include:\s*$/.test(item.line));
  assert(includeIndex !== -1, `${file} job ${jobName} missing matrix include.`);

  const includeIndent = countIndent(block[includeIndex].line);
  const entries = [];
  let current;

  for (let index = includeIndex + 1; index < block.length; index += 1) {
    const item = block[index];
    const trimmed = item.line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }

    const indent = countIndent(item.line);
    if (indent <= includeIndent) {
      break;
    }

    const entryMatch = item.line.match(/^\s*-\s+([A-Za-z_][A-Za-z0-9_-]*):\s*(.*?)\s*$/);
    if (entryMatch) {
      current = { __line: item.number, [entryMatch[1]]: parseWorkflowScalar(entryMatch[2]) };
      entries.push(current);
      continue;
    }

    const fieldMatch = item.line.match(/^\s+([A-Za-z_][A-Za-z0-9_-]*):\s*(.*?)\s*$/);
    if (fieldMatch && current) {
      current[fieldMatch[1]] = parseWorkflowScalar(fieldMatch[2]);
      continue;
    }

    throw new Error(`${file}:${item.number} unsupported matrix include line in job ${jobName}: ${trimmed}`);
  }

  assert(entries.length > 0, `${file} job ${jobName} matrix include must not be empty.`);
  return entries;
}

function targetEntriesByKey(file, jobName, entries) {
  const byTarget = new Map();
  for (const entry of entries) {
    assert(entry.target, `${file}:${entry.__line} job ${jobName} matrix entry missing target.`);
    assert(findTarget(entry.target), `${file}:${entry.__line} job ${jobName} references unknown target ${entry.target}.`);
    assert(!byTarget.has(entry.target), `${file} job ${jobName} duplicates target ${entry.target}.`);
    byTarget.set(entry.target, entry);
  }
  return byTarget;
}

function assertTargetCoverage(file, jobName, byTarget) {
  for (const target of supportedTargets) {
    assert(byTarget.has(target.key), `${file} job ${jobName} missing target ${target.key}.`);
  }
  for (const key of byTarget.keys()) {
    assert(findTarget(key), `${file} job ${jobName} has unexpected target ${key}.`);
  }
}

function expectedBinaryPath(target) {
  return `target/${target.rustTarget}/release/${target.binaryName}`;
}

function validateNativeBuildWorkflowMatrix(file, text, jobName) {
  const entries = workflowMatrixInclude(file, text, jobName);
  const byTarget = targetEntriesByKey(file, jobName, entries);
  assertTargetCoverage(file, jobName, byTarget);

  for (const target of supportedTargets) {
    const entry = byTarget.get(target.key);
    assert(entry.rust_target === target.rustTarget, `${file} job ${jobName} target ${target.key} rust_target must be ${target.rustTarget}.`);
    assert(entry.runner === target.runner, `${file} job ${jobName} target ${target.key} runner must be ${target.runner}.`);
    assert(entry.binary === expectedBinaryPath(target), `${file} job ${jobName} target ${target.key} binary must be ${expectedBinaryPath(target)}.`);
  }
}

function validateNativePackageSmokeWorkflowBehavior(file, text, jobName) {
  const block = workflowJobBlock(file, text, jobName);
  const body = block.map((item) => item.line).join("\n");
  assert(body.includes("ARCHIVA_NATIVE_TARGET: ${{ matrix.target }}"), `${file} job ${jobName} must bind ARCHIVA_NATIVE_TARGET to the matrix target.`);
  assert(body.includes("NPM_CONFIG_INCLUDE: optional"), `${file} job ${jobName} must force optional native dependencies on during package smoke.`);
  assert(body.includes("NPM_CONFIG_IGNORE_SCRIPTS: \"false\""), `${file} job ${jobName} must force postinstall scripts on during package smoke.`);
  assert(body.includes("npm run smoke:package -- --target ${{ matrix.target }}"), `${file} job ${jobName} must smoke the requested matrix target.`);
}

function validateNativePackageArtifactUpload(file, text) {
  const block = workflowJobBlock(file, text, "native-package");
  const body = block.map((item) => item.line).join("\n");
  assert(
    body.includes("path: target/npm-tarballs/jalkarna-archiva-${{ matrix.target }}-*.tgz"),
    `${file} native-package artifact upload must only publish the real matrix target tarball.`
  );
  assert(!body.includes("path: target/npm-tarballs/*.tgz"), `${file} native-package artifact upload must not collect fixture tarballs.`);
}

function validatePublishNativeWorkflowBehavior(file, text) {
  const block = workflowJobBlock(file, text, "publish-native");
  const body = block.map((item) => item.line).join("\n");
  assert(body.includes("node tools/npm-publish-idempotent.mjs --native-target"), `${file} publish-native must publish through the shared idempotent npm helper.`);
  assert(!body.includes('spawnSync(npmCommand, ["publish"'), `${file} publish-native must not inline npm publish logic.`);
}

function validatePostPublishSmokeWorkflowMatrices(file, text) {
  const jobs = [
    { name: "post-publish-smoke", musl: false },
    { name: "post-publish-musl-smoke", musl: true }
  ];
  const seen = new Map();

  for (const job of jobs) {
    const entries = workflowMatrixInclude(file, text, job.name);
    const byTarget = targetEntriesByKey(file, job.name, entries);
    for (const [key, entry] of byTarget.entries()) {
      const target = requireTarget(key);
      assert(!seen.has(key), `${file} post-publish smoke matrices duplicate target ${key}.`);
      assert(entry.runner === target.runner, `${file} job ${job.name} target ${key} runner must be ${target.runner}.`);
      assert(Boolean(target.libc === "musl") === job.musl, `${file} job ${job.name} target ${key} has wrong libc smoke grouping.`);
      seen.set(key, job.name);
    }
  }

  assertTargetCoverage(file, "combined post-publish smoke", seen);
}

function validatePostPublishSmokeWorkflowBehavior(file, text) {
  for (const jobName of ["post-publish-smoke", "post-publish-musl-smoke"]) {
    const block = workflowJobBlock(file, text, jobName);
    const body = block.map((item) => item.line).join("\n");
    assert(body.includes("needs: publish-meta"), `${file} job ${jobName} must run after publish-meta.`);
    assert(body.includes("ARCHIVA_NATIVE_TARGET: ${{ matrix.target }}"), `${file} job ${jobName} must bind ARCHIVA_NATIVE_TARGET to the matrix target.`);
    assert(body.includes("NPM_CONFIG_INCLUDE: optional"), `${file} job ${jobName} must force optional native dependencies on.`);
    assert(body.includes("NPM_CONFIG_IGNORE_SCRIPTS: \"false\""), `${file} job ${jobName} must force postinstall scripts on.`);
    assert(body.includes("node tools/smoke-native-package.mjs --published-spec"), `${file} job ${jobName} must run the full published package smoke helper.`);
    assert(body.includes("\"@jalkarna/archiva@$VERSION\""), `${file} job ${jobName} must smoke the published root package version.`);
    assert(body.includes("--target \"${{ matrix.target }}\""), `${file} job ${jobName} must pass the matrix target to the smoke helper.`);
    assert(!body.includes("archiva --version"), `${file} job ${jobName} must not regress to version-only smoke.`);
  }
}

function validatePublishMetaWorkflowBehavior(file, text) {
  const block = workflowJobBlock(file, text, "publish-meta");
  const body = block.map((item) => item.line).join("\n");
  assert(body.includes("node tools/npm-publish-idempotent.mjs --verify-native-targets"), `${file} publish-meta must verify every native package through the shared idempotent npm helper.`);
  assert(body.includes("ARCHIVA_NATIVE_TARGET: linux-x64-gnu"), `${file} publish-meta must smoke the packed meta package with an explicit host target.`);
  assert(body.includes("NPM_CONFIG_INCLUDE: optional"), `${file} publish-meta must force optional native dependencies on during packed meta smoke.`);
  assert(body.includes("NPM_CONFIG_IGNORE_SCRIPTS: \"false\""), `${file} publish-meta must force postinstall scripts on during packed meta smoke.`);
  assert(
    body.includes("node tools/smoke-native-package.mjs --published-spec \"$pkg\" --expected-version \"$VERSION\" --target linux-x64-gnu"),
    `${file} publish-meta must run the full packed meta package smoke helper before publishing.`
  );
  assert(!body.includes("\"$prefix/bin/archiva\" --version"), `${file} publish-meta must not regress to version-only packed meta smoke.`);
  assert(!body.includes("npm install -g --prefix \"$prefix\" \"$pkg\""), `${file} publish-meta packed meta smoke must be owned by the shared helper.`);
  assert(body.includes("node tools/npm-publish-idempotent.mjs --root"), `${file} publish-meta must publish the root package through the shared idempotent npm helper.`);
  assert(!body.includes('spawnSync(npmCommand, ["publish"'), `${file} publish-meta must not inline npm publish logic.`);
}

function validatePublishWorkflowConcurrency(file, text) {
  assert(text.includes("concurrency:"), `${file} must define publish workflow concurrency.`);
  assert(text.includes("group: publish-${{ github.event.release.tag_name || github.ref }}"), `${file} publish workflow concurrency must be keyed by release tag or ref.`);
  assert(text.includes("cancel-in-progress: false"), `${file} publish workflow must not cancel an in-progress publish.`);
}

function validatePublishWorkflowTagGate(file, text) {
  const block = workflowJobBlock(file, text, "heavy-validation");
  const body = block.map((item) => item.line).join("\n");
  assert(body.includes("const expectedTag = `v${packageVersion}`;"), `${file} publish version check must derive the expected release tag from package.json.`);
  assert(body.includes('process.env.GITHUB_EVENT_NAME === "workflow_dispatch"'), `${file} manual publish must be explicitly tag-gated.`);
  assert(body.includes("const expectedRef = `refs/tags/${expectedTag}`;"), `${file} manual publish must require the matching package-version tag ref.`);
  assert(body.includes("Manual publish must run from"), `${file} manual publish tag-gate failure must be explicit.`);
}

function validateCombinedSeededScaleWorkflow(file, text) {
  const block = workflowJobBlock(file, text, "heavy-validation");
  const body = block.map((item) => item.line).join("\n");
  const required = {
    ARCHIVA_SCALE_SEEDED: "1",
    ARCHIVA_SCALE_SEEDED_FILES: "100000",
    ARCHIVA_SCALE_SEEDED_DECISIONS: "1000000",
    ARCHIVA_SCALE_SEEDED_DECISIONS_PER_FILE: "10",
    ARCHIVA_SCALE_SEEDED_MUTATE_FILES: "1000"
  };
  for (const [name, value] of Object.entries(required)) {
    assert(body.includes(`${name}: "${value}"`), `${file} heavy-validation combined seeded scale must set ${name}=${value}.`);
  }
}

function validateHeavyValidationStressWorkflow(file, text) {
  const block = workflowJobBlock(file, text, "heavy-validation");
  const body = block.map((item) => item.line).join("\n");
  assert(body.includes("npm run stress:soak | tee archiva-stress-soak.json"), `${file} heavy-validation must run the native stress soak.`);
  assert(body.includes("archiva-stress-soak.json"), `${file} heavy-validation must upload the native stress soak artifact.`);
}

async function validateWorkflowMatrices() {
  const ciFile = ".github/workflows/ci.yml";
  const publishFile = ".github/workflows/publish.yml";
  const validationFile = ".github/workflows/validation.yml";
  const ci = await fs.readFile(path.join(repoRoot, ciFile), "utf8");
  const publish = await fs.readFile(path.join(repoRoot, publishFile), "utf8");
  const validation = await fs.readFile(path.join(repoRoot, validationFile), "utf8");

  validateWorkflowRustToolchain(ciFile, ci);
  validateWorkflowRustToolchain(publishFile, publish);
  validateWorkflowRustToolchain(validationFile, validation);
  validateWorkflowActionPins(ciFile, ci);
  validateWorkflowActionPins(publishFile, publish);
  validateWorkflowActionPins(validationFile, validation);
  validateNativeBuildWorkflowMatrix(ciFile, ci, "native-package");
  validateNativeBuildWorkflowMatrix(publishFile, publish, "publish-native");
  validateNativePackageSmokeWorkflowBehavior(ciFile, ci, "native-package");
  validateNativePackageSmokeWorkflowBehavior(publishFile, publish, "publish-native");
  validateNativePackageArtifactUpload(ciFile, ci);
  validatePublishNativeWorkflowBehavior(publishFile, publish);
  validatePublishMetaWorkflowBehavior(publishFile, publish);
  validatePublishWorkflowConcurrency(publishFile, publish);
  validatePublishWorkflowTagGate(publishFile, publish);
  validatePostPublishSmokeWorkflowMatrices(publishFile, publish);
  validatePostPublishSmokeWorkflowBehavior(publishFile, publish);
  validateCombinedSeededScaleWorkflow(validationFile, validation);
  validateCombinedSeededScaleWorkflow(publishFile, publish);
  validateHeavyValidationStressWorkflow(validationFile, validation);
  validateHeavyValidationStressWorkflow(publishFile, publish);
  validateRustSelfCorpusWorkflow(validationFile, validation);
  validateRustSelfCorpusWorkflow(publishFile, publish);
}

function expectedRustToolchainVersion() {
  return "1.96.0";
}

function matchRequired(text, pattern, label) {
  const match = text.match(pattern);
  assert(match, `${label} missing.`);
  return match[1];
}

async function validateRustToolchainMetadata() {
  const expectedVersion = expectedRustToolchainVersion();
  const cargo = await fs.readFile(path.join(repoRoot, "Cargo.toml"), "utf8");
  const toolchain = await fs.readFile(path.join(repoRoot, "rust-toolchain.toml"), "utf8");
  const cargoRustVersion = matchRequired(cargo, /^rust-version = "([^"]+)"$/m, "Cargo.toml rust-version");
  const toolchainChannel = matchRequired(toolchain, /^channel = "([^"]+)"$/m, "rust-toolchain.toml channel");

  assert(cargoRustVersion === expectedVersion, `Cargo.toml rust-version must be ${expectedVersion}.`);
  assert(toolchainChannel === expectedVersion, `rust-toolchain.toml channel must be ${expectedVersion}.`);
  assert(toolchain.includes('components = ["rustfmt", "clippy"]'), "rust-toolchain.toml must install rustfmt and clippy.");
}

function validateWorkflowRustToolchain(file, text) {
  const expectedVersion = expectedRustToolchainVersion();
  const installs = [...text.matchAll(/rustup toolchain install ([0-9]+\.[0-9]+\.[0-9]+)/g)].map((match) => match[1]);
  const defaults = [...text.matchAll(/rustup default ([0-9]+\.[0-9]+\.[0-9]+)/g)].map((match) => match[1]);
  assert(installs.length > 0, `${file} must install the pinned Rust toolchain.`);
  assert(defaults.length > 0, `${file} must set the pinned Rust toolchain as default.`);
  assert(installs.every((version) => version === expectedVersion), `${file} rustup toolchain install versions must all be ${expectedVersion}.`);
  assert(defaults.every((version) => version === expectedVersion), `${file} rustup default versions must all be ${expectedVersion}.`);
}

function validateWorkflowActionPins(file, text) {
  const expected = new Map([
    ["actions/checkout", "v7"],
    ["actions/setup-node", "v6"],
    ["actions/upload-artifact", "v6"]
  ]);
  const uses = [...text.matchAll(/uses:\s+(actions\/(?:checkout|setup-node|upload-artifact))@([^\s#]+)/g)];
  assert(uses.length > 0, `${file} must use checked GitHub action pins.`);
  for (const [, action, version] of uses) {
    assert(version === expected.get(action), `${file} must pin ${action}@${expected.get(action)}.`);
  }
}

validateBinaryFormatParsers();
validateSupportedTargets();
await validateRustToolchainMetadata();
await validateWorkflowMatrices();
await validateRootPackage();
if (hasFlag("--meta-package")) {
  await validateMetaPackageArtifacts();
}

const explicitTarget = readArg("--staged-target");
let stagedTargets;
if (explicitTarget) {
  const target = requireTarget(explicitTarget);
  await validateStagedPackage(target);
  stagedTargets = [target.key];
} else {
  stagedTargets = await validateExistingStagedPackages();
}

if (hasFlag("--json")) {
  console.log(JSON.stringify({ status: "passed", stagedTargets }, null, 2));
} else {
  console.log(`Native package metadata OK (${stagedTargets.length} staged package${stagedTargets.length === 1 ? "" : "s"} checked).`);
}

function validateRustSelfCorpusWorkflow(file, text) {
  const block = workflowJobBlock(file, text, "heavy-validation");
  const body = block.map((item) => item.line).join("\n");
  assert(body.includes("Rust self-corpus scale"), `${file} heavy-validation must run the Rust self-corpus scale leg.`);
  assert(body.includes('ARCHIVA_SCALE_CORPUS_FILES: "40"'), `${file} Rust self-corpus scale must pin its file count.`);
  assert(body.includes('ARCHIVA_SCALE_CORPUS_DECISIONS: "24"'), `${file} Rust self-corpus scale must pin its decision count.`);
  assert(body.includes('ARCHIVA_SCALE_CORPUS_MUTATE_FILES: "16"'), `${file} Rust self-corpus scale must pin its mutation count.`);
  assert(body.includes('ARCHIVA_SCALE_CORPUS_ROOT="$GITHUB_WORKSPACE/src"'), `${file} Rust self-corpus scale must use the checked-out Rust source tree.`);
  assert(body.includes("npm run --silent scale:corpus:rust | tee archiva-scale-rust-corpus.json"), `${file} must capture the Rust self-corpus scale artifact.`);
  assert(body.includes("archiva-scale-rust-corpus.json"), `${file} must upload the Rust self-corpus scale artifact.`);
}
