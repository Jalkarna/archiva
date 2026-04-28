import fs from "node:fs/promises";
import { ensureDirFor, pathExists } from "./fs.js";
import { dmapPath } from "./paths.js";
import type { DlogFile, DmapEntry } from "./types.js";

const STATUS_VALUES = new Set(["UNDECIDED", "STALE", "ORPHAN"]);

export function parseDmap(content: string): DmapEntry[] {
  return content
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const firstColon = line.indexOf(":");
      if (firstColon === -1) throw new Error(`Invalid .dmap line: ${line}`);
      const rangePart = line.slice(0, firstColon);
      const rest = line.slice(firstColon + 1);
      const lastColon = rest.lastIndexOf(":");
      const possibleStatus = lastColon === -1 ? undefined : rest.slice(lastColon + 1);
      const status = possibleStatus && STATUS_VALUES.has(possibleStatus) ? possibleStatus : undefined;
      const anchor = status ? rest.slice(0, lastColon) : rest;
      if (!rangePart || !anchor) throw new Error(`Invalid .dmap line: ${line}`);
      const [start, end] = rangePart.split("-").map(Number);
      if (!Number.isInteger(start) || !Number.isInteger(end)) {
        throw new Error(`Invalid .dmap range: ${line}`);
      }
      return {
        startLine: start,
        endLine: end,
        anchor,
        status: status as DmapEntry["status"] | undefined
      };
    });
}

export function renderDmap(entries: DmapEntry[]): string {
  return entries
    .sort((a, b) => a.startLine - b.startLine || a.anchor.localeCompare(b.anchor))
    .map((entry) => {
      const suffix = entry.status ? `:${entry.status}` : "";
      return `${entry.startLine}-${entry.endLine}:${entry.anchor}${suffix}`;
    })
    .join("\n")
    .concat(entries.length > 0 ? "\n" : "");
}

export function dmapEntriesFromDlog(dlog: DlogFile): DmapEntry[] {
  return Object.entries(dlog.decisions).map(([anchor, decision]) => ({
    startLine: decision.lines_hint[0],
    endLine: decision.lines_hint[1],
    anchor,
    status: decision.status
  }));
}

export async function loadDmap(projectRoot: string, file: string): Promise<DmapEntry[]> {
  const filePath = dmapPath(projectRoot, file);
  if (!(await pathExists(filePath))) return [];
  return parseDmap(await fs.readFile(filePath, "utf8"));
}

export async function writeDmap(projectRoot: string, dlog: DlogFile): Promise<void> {
  const filePath = dmapPath(projectRoot, dlog.file);
  await ensureDirFor(filePath);
  await fs.writeFile(filePath, renderDmap(dmapEntriesFromDlog(dlog)), "utf8");
}
