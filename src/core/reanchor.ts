import { execFile } from "node:child_process";
import fs from "node:fs/promises";
import { promisify } from "node:util";
import { diffLines } from "diff";
import { extractAnchors } from "./anchor.js";
import { loadDlog, writeDlog } from "./dlog.js";
import { writeDmap } from "./dmap.js";
import { fingerprint, getLines } from "./fingerprint.js";
import { sourcePath } from "./paths.js";

const execFileAsync = promisify(execFile);

export async function postToolUse(projectRoot: string, file: string): Promise<string> {
  const dlog = await loadDlog(projectRoot, file);
  if (!dlog) return `No decisions for ${file}; nothing to re-anchor.`;

  const fullPath = sourcePath(projectRoot, file);
  const newContent = await fs.readFile(fullPath, "utf8");
  const oldContent = await readGitHead(projectRoot, file).catch(() => newContent);
  const anchors = extractAnchors(file, newContent);

  let stale = 0;
  let orphan = 0;
  for (const [anchor, decision] of Object.entries(dlog.decisions)) {
    if (!anchors[anchor]) {
      decision.status = "ORPHAN";
      orphan += 1;
      continue;
    }

    decision.lines_hint = applyDiffToRange(oldContent, newContent, decision.lines_hint);
    const currentFingerprint = fingerprint(getLines(newContent, decision.lines_hint));
    if (currentFingerprint !== decision.fingerprint) {
      if (decision.status !== "STALE") decision.stale_since = new Date().toISOString();
      decision.status = "STALE";
      stale += 1;
    } else if (decision.status === "STALE") {
      delete decision.status;
      delete decision.stale_since;
    }
  }

  await writeDlog(projectRoot, dlog);
  await writeDmap(projectRoot, dlog);
  return `Re-anchored ${file}: ${stale} stale, ${orphan} orphan.`;
}

export function applyDiffToRange(oldContent: string, newContent: string, range: [number, number]): [number, number] {
  const changes = diffLines(oldContent, newContent);
  let oldLine = 1;
  let offset = 0;
  const [start, end] = range;

  for (const change of changes) {
    const count = lineCount(change.value);
    if (change.added) {
      if (oldLine <= start) offset += count;
      continue;
    }
    if (change.removed) {
      if (oldLine + count - 1 < start) offset -= count;
      oldLine += count;
      continue;
    }
    oldLine += count;
  }

  return [Math.max(1, start + offset), Math.max(1, end + offset)];
}

function lineCount(value: string): number {
  if (!value) return 0;
  const normalized = value.endsWith("\n") ? value.slice(0, -1) : value;
  return normalized.length === 0 ? 0 : normalized.split(/\r?\n/).length;
}

async function readGitHead(projectRoot: string, file: string): Promise<string> {
  const { stdout } = await execFileAsync("git", ["show", `HEAD:${file}`], {
    cwd: projectRoot,
    maxBuffer: 10 * 1024 * 1024
  });
  return stdout;
}
