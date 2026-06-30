import path from "node:path";
import { loadGitignoreMatcher } from "./gitignore.js";
import { listFiles } from "./fs.js";
import { sourcePathFromDecisionFile, toProjectRelative } from "./paths.js";

const SOURCE_EXTENSIONS = new Set([".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"]);

export async function listDlogFiles(projectRoot: string): Promise<string[]> {
  return listFiles(path.join(projectRoot, ".decisions"), (file) => file.endsWith(".dlog"));
}

export async function listSourceFiles(projectRoot: string): Promise<string[]> {
  return listFiles(projectRoot, (file) => isSourceFilePath(file));
}

/** Source files for lint scans, excluding paths matched by the project `.gitignore`. */
export async function listLintSourceFiles(projectRoot: string): Promise<string[]> {
  const isIgnored = await loadGitignoreMatcher(projectRoot);
  const root = path.resolve(projectRoot);
  return listFiles(root, (file) => {
    if (!isSourceFilePath(file)) return false;
    const relative = path.relative(root, file).replaceAll("\\", "/");
    return !isIgnored(relative);
  });
}

function isSourceFilePath(file: string): boolean {
  if (file.includes(`${path.sep}.decisions${path.sep}`)) return false;
  return SOURCE_EXTENSIONS.has(path.extname(file));
}

export function decisionFileToSource(projectRoot: string, decisionFilePath: string): string {
  return sourcePathFromDecisionFile(projectRoot, decisionFilePath);
}

export function absoluteToRelative(projectRoot: string, filePath: string): string {
  return toProjectRelative(projectRoot, filePath);
}
