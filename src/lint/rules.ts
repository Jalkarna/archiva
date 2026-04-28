import fs from "node:fs/promises";
import { extractAnchors } from "../core/anchor.js";
import { loadDlog, writeDlog } from "../core/dlog.js";
import { writeDmap } from "../core/dmap.js";
import { pathExists } from "../core/fs.js";
import { fingerprint, getLines } from "../core/fingerprint.js";
import { sourcePath } from "../core/paths.js";
import { absoluteToRelative, decisionFileToSource, listDlogFiles, listSourceFiles } from "../core/scan.js";
import type { DlogFile, LintIssue } from "../core/types.js";

export async function lintProject(projectRoot: string, options: { fix?: boolean } = {}): Promise<LintIssue[]> {
  const issues: LintIssue[] = [];
  const dlogFiles = await listDlogFiles(projectRoot);
  const dlogs: DlogFile[] = [];

  for (const dlogFile of dlogFiles) {
    const file = decisionFileToSource(projectRoot, dlogFile);
    const dlog = await loadDlog(projectRoot, file);
    if (!dlog) continue;
    dlogs.push(dlog);
    issues.push(...(await lintDlog(projectRoot, dlog, options)));
  }

  issues.push(...(await lintComplexUndecided(projectRoot, dlogs)));
  return issues;
}

async function lintDlog(projectRoot: string, dlog: DlogFile, options: { fix?: boolean }): Promise<LintIssue[]> {
  const issues: LintIssue[] = [];
  const fullSourcePath = sourcePath(projectRoot, dlog.file);
  const sourceExists = await pathExists(fullSourcePath);
  const source = sourceExists ? await fs.readFile(fullSourcePath, "utf8") : "";
  const anchors = sourceExists ? extractAnchors(dlog.file, source) : {};
  let changed = false;

  const supersededIds = new Set(
    Object.values(dlog.decisions)
      .map((decision) => decision.supersedes)
      .filter((id): id is string => Boolean(id))
  );

  for (const [anchor, decision] of Object.entries(dlog.decisions)) {
    if (!anchors[anchor]) {
      issues.push({
        rule: "arc/orphan",
        severity: "warning",
        file: dlog.file,
        anchor,
        message: `${anchor} no longer exists in ${dlog.file}`,
        fixable: true
      });
      if (options.fix) {
        delete dlog.decisions[anchor];
        changed = true;
      }
      continue;
    }

    const currentFingerprint = fingerprint(getLines(source, decision.lines_hint));
    if (currentFingerprint !== decision.fingerprint || decision.status === "STALE") {
      issues.push({
        rule: "arc/stale",
        severity: "error",
        file: dlog.file,
        anchor,
        message: `${anchor} code fingerprint differs from recorded decision`,
        fixable: false
      });
      if (decision.status !== "STALE") {
        decision.status = "STALE";
        decision.stale_since = new Date().toISOString();
        changed = true;
      }
    }

    if (decision.status === "STALE" && !supersededIds.has(decision.id)) {
      issues.push({
        rule: "arc/supersede",
        severity: "error",
        file: dlog.file,
        anchor,
        message: `${anchor} is stale and has not been superseded`,
        fixable: false
      });
    }
  }

  if (changed) {
    await writeDlog(projectRoot, dlog);
    await writeDmap(projectRoot, dlog);
  }

  return issues;
}

async function lintComplexUndecided(projectRoot: string, dlogs: DlogFile[]): Promise<LintIssue[]> {
  const issues: LintIssue[] = [];
  const decisionsByFile = new Map(dlogs.map((dlog) => [dlog.file, new Set(Object.keys(dlog.decisions))]));

  for (const absoluteFile of await listSourceFiles(projectRoot)) {
    const file = absoluteToRelative(projectRoot, absoluteFile);
    const content = await fs.readFile(absoluteFile, "utf8");
    const anchors = extractAnchors(file, content);
    const decided = decisionsByFile.get(file) ?? new Set<string>();

    for (const [anchor, info] of Object.entries(anchors)) {
      if (info.kind === "class" || info.kind === "export" || info.kind === "block") continue;
      if (info.complexity >= 5 && !decided.has(anchor)) {
        issues.push({
          rule: "arc/undecided",
          severity: "warning",
          file,
          anchor,
          message: `${anchor} has complexity ${info.complexity} and no decision`,
          fixable: false
        });
      }
    }
  }

  return issues;
}

export function formatLintIssues(issues: LintIssue[]): string {
  if (issues.length === 0) return "No decision issues found.";
  return issues
    .map((issue) => `${issue.severity.toUpperCase()} ${issue.rule} ${issue.file} ${issue.anchor}: ${issue.message}`)
    .join("\n");
}
