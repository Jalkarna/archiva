import fs from "node:fs/promises";
import { diffLines } from "diff";
import { extractAnchors } from "./anchor.js";
import { clearRecoveredStatus, isFingerprintStale, markOrphan, markStale } from "./decision-status.js";
import { loadDlog, writeDlog } from "./dlog.js";
import { writeDmap } from "./dmap.js";
import { readGitHeadFile } from "./git.js";
import { sourcePath } from "./paths.js";

export async function postToolUse(projectRoot: string, file: string): Promise<string> {
  const dlog = await loadDlog(projectRoot, file);
  if (!dlog) return `No decisions for ${file}; nothing to re-anchor.`;

  const fullPath = sourcePath(projectRoot, file);
  const newContent = await fs.readFile(fullPath, "utf8");
  const oldContent = await readGitHeadFile(projectRoot, file).catch(() => newContent);
  const anchors = extractAnchors(file, newContent);

  let stale = 0;
  let orphan = 0;
  for (const [anchor, decision] of Object.entries(dlog.decisions)) {
    decision.lines_hint = applyDiffToRange(oldContent, newContent, decision.lines_hint);

    if (!anchors[anchor]) {
      markOrphan(decision);
      orphan += 1;
      continue;
    }

    if (isFingerprintStale(newContent, decision)) {
      markStale(decision);
      stale += 1;
    } else if (clearRecoveredStatus(decision)) {
      // recovered from STALE or ORPHAN
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
