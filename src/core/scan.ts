import path from "node:path";
import { listFiles } from "./fs.js";
import { sourcePathFromDecisionFile, toProjectRelative } from "./paths.js";

export async function listDlogFiles(projectRoot: string): Promise<string[]> {
  return listFiles(path.join(projectRoot, ".decisions"), (file) => file.endsWith(".dlog"));
}

export async function listSourceFiles(projectRoot: string): Promise<string[]> {
  const extensions = new Set([".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"]);
  return listFiles(projectRoot, (file) => {
    if (file.includes(`${path.sep}.decisions${path.sep}`)) return false;
    return extensions.has(path.extname(file));
  });
}

export function decisionFileToSource(projectRoot: string, decisionFilePath: string): string {
  return sourcePathFromDecisionFile(projectRoot, decisionFilePath);
}

export function absoluteToRelative(projectRoot: string, filePath: string): string {
  return toProjectRelative(projectRoot, filePath);
}
