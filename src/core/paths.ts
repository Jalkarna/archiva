import path from "node:path";

export function toProjectRelative(projectRoot: string, filePath: string): string {
  const relative = path.isAbsolute(filePath)
    ? path.relative(projectRoot, filePath)
    : filePath;
  return normalizeRelativePath(relative);
}

export function normalizeRelativePath(filePath: string): string {
  const normalized = filePath.replaceAll("\\", "/").replace(/^\.\/+/, "");
  if (!normalized || normalized.startsWith("../") || normalized === ".." || path.isAbsolute(normalized)) {
    throw new Error(`Expected a project-relative path, got "${filePath}"`);
  }
  return normalized;
}

export function sourcePath(projectRoot: string, filePath: string): string {
  return path.join(projectRoot, normalizeRelativePath(filePath));
}

export function decisionBasePath(projectRoot: string, filePath: string): string {
  return path.join(projectRoot, ".decisions", normalizeRelativePath(filePath));
}

export function dlogPath(projectRoot: string, filePath: string): string {
  return `${decisionBasePath(projectRoot, filePath)}.dlog`;
}

export function dmapPath(projectRoot: string, filePath: string): string {
  return `${decisionBasePath(projectRoot, filePath)}.dmap`;
}

export function sourcePathFromDecisionFile(projectRoot: string, decisionFilePath: string): string {
  const relative = path.relative(path.join(projectRoot, ".decisions"), decisionFilePath);
  return normalizeRelativePath(relative.replace(/\.dlog$/, "").replace(/\.dmap$/, ""));
}
