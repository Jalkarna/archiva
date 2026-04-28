import fs from "node:fs/promises";
import { extractAnchors } from "./anchor.js";
import { loadOrCreateDlog, loadDlog, writeDlog } from "./dlog.js";
import { writeDmap } from "./dmap.js";
import { fingerprint, getLines } from "./fingerprint.js";
import { sourcePath } from "./paths.js";
import type { DecisionHistoryEntry, DecisionRecord, WriteDecisionInput } from "./types.js";

export async function writeDecision(projectRoot: string, input: WriteDecisionInput): Promise<DecisionRecord> {
  const dlog = await loadOrCreateDlog(projectRoot, input.file);
  const fullSourcePath = sourcePath(projectRoot, input.file);
  const source = await fs.readFile(fullSourcePath, "utf8");
  const id = nextDecisionId(dlog);
  const existingToSupersede = input.supersedes ? findDecisionById(dlog.decisions, input.supersedes) : undefined;
  if (input.supersedes && !existingToSupersede) {
    throw new Error(`Cannot supersede unknown decision id "${input.supersedes}" in ${input.file}. Call why first and use the recorded decision id.`);
  }
  const history: DecisionHistoryEntry[] = existingToSupersede
    ? [
        ...existingToSupersede.record.history,
        {
          id: existingToSupersede.record.id,
          chose: existingToSupersede.record.chose,
          because: existingToSupersede.record.because,
          timestamp: existingToSupersede.record.timestamp,
          superseded_reason: input.because
        }
      ]
    : [];

  const decision: DecisionRecord = {
    id,
    lines_hint: input.lines,
    fingerprint: fingerprint(getLines(source, input.lines)),
    chose: input.chose,
    because: input.because,
    rejected: input.rejected,
    expires_if: input.expires_if,
    session: input.session ?? process.env.ARCHIVA_SESSION,
    timestamp: new Date().toISOString(),
    supersedes: input.supersedes,
    history
  };

  if (existingToSupersede && existingToSupersede.anchor !== input.anchor) {
    delete dlog.decisions[existingToSupersede.anchor];
  }
  dlog.decisions[input.anchor] = decision;
  await writeDlog(projectRoot, dlog);
  await writeDmap(projectRoot, dlog);
  return decision;
}

export async function why(projectRoot: string, file: string, anchor?: string): Promise<string> {
  const dlog = await loadDlog(projectRoot, file);
  if (!dlog) return `No decisions found for ${file}.`;

  const decisions = anchor ? Object.entries(dlog.decisions).filter(([key]) => key === anchor) : Object.entries(dlog.decisions);
  if (decisions.length === 0) return `No decision found for ${file}${anchor ? ` at ${anchor}` : ""}.`;

  return decisions.map(([key, decision]) => formatDecision(key, decision)).join("\n\n");
}

export async function whyForLine(projectRoot: string, file: string, line: number): Promise<string> {
  const dlog = await loadDlog(projectRoot, file);
  if (!dlog) return `No decisions found for ${file}.`;
  const match = Object.entries(dlog.decisions).find(([, decision]) => {
    const [start, end] = decision.lines_hint;
    return line >= start && line <= end;
  });
  if (!match) return `No decision found for ${file} at line ${line}.`;
  return formatDecision(match[0], match[1]);
}

export async function history(projectRoot: string, file: string, anchor: string): Promise<string> {
  const dlog = await loadDlog(projectRoot, file);
  const decision = dlog?.decisions[anchor];
  if (!decision) return `No decision found for ${file} at ${anchor}.`;
  const chain = [
    ...decision.history.map((entry) => ({
      id: entry.id,
      chose: entry.chose,
      because: entry.because,
      timestamp: entry.timestamp
    })),
    {
      id: decision.id,
      chose: decision.chose,
      because: decision.because,
      timestamp: decision.timestamp
    }
  ];
  return chain
    .map((entry) => `${entry.id} ${entry.timestamp ?? ""}\n  Chose: ${entry.chose}${entry.because ? `\n  Because: ${entry.because}` : ""}`)
    .join("\n\n");
}

export async function validateAnchor(projectRoot: string, file: string, anchor: string): Promise<boolean> {
  const content = await fs.readFile(sourcePath(projectRoot, file), "utf8");
  return Object.prototype.hasOwnProperty.call(extractAnchors(file, content), anchor);
}

function formatDecision(anchor: string, decision: DecisionRecord): string {
  const status = decision.status ? ` [${decision.status}]` : "";
  const rejected = decision.rejected.length
    ? `\nRejected:\n${decision.rejected.map((item) => `  - ${item.approach} -> ${item.reason}`).join("\n")}`
    : "";
  const expires = decision.expires_if ? `\nExpires if: ${decision.expires_if}` : "";
  return `${anchor} ${decision.id} (lines ${decision.lines_hint[0]}-${decision.lines_hint[1]})${status}
Chose: ${decision.chose}
Because: ${decision.because}${rejected}
Recorded: ${decision.timestamp}${decision.session ? `  Session: ${decision.session}` : ""}${expires}`;
}

function nextDecisionId(dlog: { decisions: Record<string, DecisionRecord> }): string {
  const max = Object.values(dlog.decisions).reduce((current, decision) => {
    const match = /^dec_(\d+)$/.exec(decision.id);
    return match ? Math.max(current, Number(match[1])) : current;
  }, 0);
  return `dec_${String(max + 1).padStart(3, "0")}`;
}

function findDecisionById(decisions: Record<string, DecisionRecord>, id: string): { anchor: string; record: DecisionRecord } | undefined {
  for (const [anchor, record] of Object.entries(decisions)) {
    if (record.id === id) return { anchor, record };
  }
  return undefined;
}
