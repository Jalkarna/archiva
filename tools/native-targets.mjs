export const metaPackageName = "@jalkarna/archiva";
export const packageBinaryRelativePath = "dist-native/archiva.exe";
export const localBinaryRelativePath = "dist-native/archiva";
export const nativePackageRoot = "target/npm-packages";
export const nativeTarballRoot = "target/npm-tarballs";

export const supportedTargets = [
  {
    key: "linux-x64-gnu",
    packageName: "@jalkarna/archiva-linux-x64-gnu",
    rustTarget: "x86_64-unknown-linux-gnu",
    platform: "linux",
    arch: "x64",
    os: "linux",
    cpu: "x64",
    libc: "glibc",
    binaryName: "archiva",
    runner: "ubuntu-22.04"
  },
  {
    key: "linux-x64-musl",
    packageName: "@jalkarna/archiva-linux-x64-musl",
    rustTarget: "x86_64-unknown-linux-musl",
    platform: "linux",
    arch: "x64",
    os: "linux",
    cpu: "x64",
    libc: "musl",
    binaryName: "archiva",
    runner: "ubuntu-22.04"
  },
  {
    key: "linux-arm64-gnu",
    packageName: "@jalkarna/archiva-linux-arm64-gnu",
    rustTarget: "aarch64-unknown-linux-gnu",
    platform: "linux",
    arch: "arm64",
    os: "linux",
    cpu: "arm64",
    libc: "glibc",
    binaryName: "archiva",
    runner: "ubuntu-24.04-arm"
  },
  {
    key: "linux-arm64-musl",
    packageName: "@jalkarna/archiva-linux-arm64-musl",
    rustTarget: "aarch64-unknown-linux-musl",
    platform: "linux",
    arch: "arm64",
    os: "linux",
    cpu: "arm64",
    libc: "musl",
    binaryName: "archiva",
    runner: "ubuntu-24.04-arm"
  },
  {
    key: "darwin-x64",
    packageName: "@jalkarna/archiva-darwin-x64",
    rustTarget: "x86_64-apple-darwin",
    platform: "darwin",
    arch: "x64",
    os: "darwin",
    cpu: "x64",
    binaryName: "archiva",
    runner: "macos-15-intel"
  },
  {
    key: "darwin-arm64",
    packageName: "@jalkarna/archiva-darwin-arm64",
    rustTarget: "aarch64-apple-darwin",
    platform: "darwin",
    arch: "arm64",
    os: "darwin",
    cpu: "arm64",
    binaryName: "archiva",
    runner: "macos-15"
  },
  {
    key: "win32-x64-msvc",
    packageName: "@jalkarna/archiva-win32-x64-msvc",
    rustTarget: "x86_64-pc-windows-msvc",
    platform: "win32",
    arch: "x64",
    os: "win32",
    cpu: "x64",
    binaryName: "archiva.exe",
    runner: "windows-latest"
  }
];

export function runtimeLibc(platform = process.platform) {
  if (platform !== "linux") {
    return undefined;
  }
  const report = process.report?.getReport?.();
  return report?.header?.glibcVersionRuntime ? "glibc" : "musl";
}

export function findTarget(value) {
  return supportedTargets.find((target) => {
    return target.key === value || target.rustTarget === value || target.packageName === value;
  });
}

export function detectHostTarget(platform = process.platform, arch = process.arch, libc = runtimeLibc(platform)) {
  return supportedTargets.find((target) => {
    if (target.platform !== platform || target.arch !== arch) {
      return false;
    }
    return target.platform !== "linux" || target.libc === libc;
  });
}

export function requireTarget(value) {
  const target = value ? findTarget(value) : detectHostTarget();
  if (!target) {
    const supported = supportedTargets.map((item) => item.key).join(", ");
    throw new Error(`Unsupported Archiva native target '${value ?? `${process.platform}/${process.arch}`}'. Supported targets: ${supported}.`);
  }
  return target;
}

export function optionalDependencyMap(version) {
  return Object.fromEntries(supportedTargets.map((target) => [target.packageName, version]));
}

export function packagePathSegments(packageName) {
  return packageName.split("/");
}
