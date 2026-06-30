import { fingerprint, getLines } from "./fingerprint.js";
import type { DecisionRecord } from "./types.js";

export function isFingerprintStale(source: string, decision: DecisionRecord): boolean {
  return fingerprint(getLines(source, decision.lines_hint)) !== decision.fingerprint;
}

export function markStale(decision: DecisionRecord): void {
  if (decision.status !== "STALE") {
    decision.stale_since = new Date().toISOString();
  }
  decision.status = "STALE";
}

export function markOrphan(decision: DecisionRecord): void {
  decision.status = "ORPHAN";
}

/** Clears STALE/ORPHAN when the anchor and fingerprint match again. */
export function clearRecoveredStatus(decision: DecisionRecord): boolean {
  if (decision.status === "STALE" || decision.status === "ORPHAN") {
    delete decision.status;
    delete decision.stale_since;
    return true;
  }
  return false;
}
