import { loadDlog } from "../core/dlog.js";
import { decisionFileToSource, listDlogFiles } from "../core/scan.js";

export async function sessionStart(projectRoot: string): Promise<string> {
  const dlogFiles = await listDlogFiles(projectRoot);
  if (dlogFiles.length === 0) return "[Archiva] No decision map found.";

  const lines = [`[Archiva] Decision map loaded for ${dlogFiles.length} files:`, ""];
  for (const dlogFile of dlogFiles) {
    const file = decisionFileToSource(projectRoot, dlogFile);
    const dlog = await loadDlog(projectRoot, file);
    if (!dlog) continue;
    lines.push(file);
    for (const [anchor, decision] of Object.entries(dlog.decisions)) {
      const status = decision.status ? ` ${decision.status}` : "";
      const rejected = decision.rejected
        .slice(0, 2)
        .map((item) => `${item.approach}(${compact(item.reason)})`)
        .join(", ");
      lines.push(`  ${decision.lines_hint[0]}-${decision.lines_hint[1]} ${anchor}${status} -> ${compact(decision.chose)}${rejected ? ` | x ${rejected}` : ""}`);
    }
    lines.push("");
  }
  return lines.join("\n").trimEnd();
}

function compact(value: string): string {
  return value.replace(/\s+/g, " ").slice(0, 80);
}
