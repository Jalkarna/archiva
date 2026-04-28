export type DecisionStatus = "UNDECIDED" | "STALE" | "ORPHAN";

export type RejectedAlternative = {
  approach: string;
  reason: string;
};

export type DecisionHistoryEntry = {
  id: string;
  chose: string;
  because?: string;
  timestamp?: string;
  superseded_reason?: string;
};

export type DecisionRecord = {
  id: string;
  lines_hint: [number, number];
  fingerprint: string;
  chose: string;
  because: string;
  rejected: RejectedAlternative[];
  expires_if?: string;
  session?: string;
  timestamp: string;
  history: DecisionHistoryEntry[];
  status?: DecisionStatus;
  stale_since?: string;
  supersedes?: string;
};

export type DlogFile = {
  file: string;
  schema: 1;
  decisions: Record<string, DecisionRecord>;
};

export type DmapEntry = {
  startLine: number;
  endLine: number;
  anchor: string;
  status?: DecisionStatus;
};

export type AnchorInfo = {
  anchor: string;
  start: number;
  end: number;
  complexity: number;
  kind: "function" | "class" | "method" | "export" | "block";
};

export type WriteDecisionInput = {
  file: string;
  anchor: string;
  lines: [number, number];
  chose: string;
  because: string;
  rejected: RejectedAlternative[];
  expires_if?: string;
  supersedes?: string;
  session?: string;
};

export type LintIssue = {
  rule: "arc/stale" | "arc/orphan" | "arc/undecided" | "arc/supersede";
  severity: "error" | "warning";
  file: string;
  anchor: string;
  message: string;
  fixable: boolean;
};
