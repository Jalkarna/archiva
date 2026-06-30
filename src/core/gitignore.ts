import fs from "node:fs/promises";
import path from "node:path";
import { pathExists } from "./fs.js";

export type GitignoreMatcher = (relativePath: string) => boolean;

export async function loadGitignoreMatcher(projectRoot: string): Promise<GitignoreMatcher> {
  const patterns: string[] = [];
  const gitignorePath = path.join(projectRoot, ".gitignore");
  if (await pathExists(gitignorePath)) {
    patterns.push(...parseGitignore(await fs.readFile(gitignorePath, "utf8")));
  }
  return (relativePath: string) => matchesGitignore(relativePath, patterns);
}

function parseGitignore(content: string): string[] {
  return content
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith("#"));
}

function matchesGitignore(relativePath: string, patterns: string[]): boolean {
  const normalized = relativePath.replaceAll("\\", "/");
  const segments = normalized.split("/");

  for (const pattern of patterns) {
    if (pattern.startsWith("!")) continue;
    if (matchPattern(normalized, segments, pattern)) return true;
  }
  return false;
}

function matchPattern(normalized: string, segments: string[], pattern: string): boolean {
  const anchored = pattern.startsWith("/");
  const dirOnly = pattern.endsWith("/");
  let body = pattern;
  if (anchored) body = body.slice(1);
  if (dirOnly) body = body.slice(0, -1);

  const regex = gitignorePatternToRegExp(body);
  if (anchored) return regex.test(normalized);

  if (dirOnly) {
    return segments.some((segment) => regex.test(segment));
  }

  return regex.test(normalized) || segments.some((segment) => regex.test(segment));
}

function gitignorePatternToRegExp(pattern: string): RegExp {
  let source = "^";
  for (let i = 0; i < pattern.length; i += 1) {
    const char = pattern[i];
    if (char === "*") {
      if (pattern[i + 1] === "*") {
        source += ".*";
        i += 1;
        if (pattern[i + 1] === "/") i += 1;
      } else {
        source += "[^/]*";
      }
      continue;
    }
    if (char === "?") {
      source += "[^/]";
      continue;
    }
    source += escapeRegExp(char);
  }
  source += "$";
  return new RegExp(source);
}

function escapeRegExp(value: string): string {
  return value.replace(/[|\\{}()[\]^$+?.]/g, "\\$&");
}
