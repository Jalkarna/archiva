import { loadDlog } from "../core/dlog.js";
import { decisionFileToSource, listDlogFiles } from "../core/scan.js";
import { lintProject } from "../lint/rules.js";

export async function status(projectRoot: string): Promise<string> {
  const dlogFiles = await listDlogFiles(projectRoot);
  const lines: string[] = [];
  let totalDecisions = 0;
  let totalStale = 0;
  let totalOrphan = 0;

  for (const dlogFile of dlogFiles) {
    const file = decisionFileToSource(projectRoot, dlogFile);
    const dlog = await loadDlog(projectRoot, file);
    if (!dlog) continue;
    const decisions = Object.values(dlog.decisions);
    const stale = decisions.filter((decision) => decision.status === "STALE").length;
    const orphan = decisions.filter((decision) => decision.status === "ORPHAN").length;
    totalDecisions += decisions.length;
    totalStale += stale;
    totalOrphan += orphan;
    lines.push(`${file.padEnd(32)} ${decisions.length} decisions  ${stale} stale  ${orphan} orphan`);
  }

  const issues = await lintProject(projectRoot);
  if (lines.length === 0) lines.push("No decision logs found.");
  lines.push("");
  lines.push(`Total: ${totalDecisions} decisions  ${totalStale} stale  ${totalOrphan} orphan  ${issues.length} issues`);
  return lines.join("\n");
}
