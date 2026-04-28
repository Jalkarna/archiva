import fs from "node:fs/promises";
import path from "node:path";
import { pathExists } from "../core/fs.js";

const AGENTS_BLOCK = `## Decision Logging (Archiva)

This project uses Archiva for decision tracking.

### Before modifying any file
- Read the decision map injected at session start (prefixed \`[Archiva]\`)
- Or call the \`why\` MCP tool: \`why(file, anchor)\`
- Do NOT modify code marked with a decision without reading it first

### After any non-trivial implementation choice
Call \`write_decision\` with:
- \`file\` and \`anchor\` (function or block name)
- \`chose\` - what approach you selected
- \`because\` - the specific reason, not a generic description
- \`rejected\` - every alternative you considered, with specific disqualifying reasons

Required for: algorithm choices, concurrency patterns, error handling strategies,
any point where you weighed 2+ approaches.

Not required for: imports, type declarations, formatting, variable names.

If changing code that has an existing decision:
- If your change preserves the reasoning -> keep the decision, update \`lines_hint\`
- If your change invalidates the reasoning -> call \`write_decision\` with \`supersedes: <id>\`
`;

export async function initProject(projectRoot: string, options: { gitignoreDecisions?: boolean } = {}): Promise<string> {
  await fs.mkdir(path.join(projectRoot, ".decisions"), { recursive: true });
  await writeClaudeSettings(projectRoot);
  await appendAgentsBlock(projectRoot);
  if (options.gitignoreDecisions) await ensureGitignoreEntry(projectRoot, ".decisions/");
  return "Archiva initialized.";
}

async function writeClaudeSettings(projectRoot: string): Promise<void> {
  const settingsPath = path.join(projectRoot, ".claude", "settings.json");
  await fs.mkdir(path.dirname(settingsPath), { recursive: true });
  let settings: Record<string, unknown> = {};
  if (await pathExists(settingsPath)) {
    settings = JSON.parse(await fs.readFile(settingsPath, "utf8")) as Record<string, unknown>;
  }

  settings.hooks = {
    SessionStart: [
      {
        hooks: [{ type: "command", command: "archiva hooks session-start" }]
      }
    ],
    PostToolUse: [
      {
        matcher: "Write|Edit|MultiEdit",
        hooks: [{ type: "command", command: "archiva hooks post-tool-use" }]
      }
    ]
  };
  settings.mcpServers = {
    ...(typeof settings.mcpServers === "object" && settings.mcpServers ? settings.mcpServers : {}),
    archiva: {
      command: "archiva",
      args: ["mcp"]
    }
  };

  await fs.writeFile(settingsPath, `${JSON.stringify(settings, null, 2)}\n`, "utf8");
}

async function appendAgentsBlock(projectRoot: string): Promise<void> {
  const agentsPath = path.join(projectRoot, "AGENTS.md");
  const existing = (await pathExists(agentsPath)) ? await fs.readFile(agentsPath, "utf8") : "";
  if (existing.includes("## Decision Logging (Archiva)")) return;
  const prefix = existing.trim().length > 0 ? `${existing.trimEnd()}\n\n` : "";
  await fs.writeFile(agentsPath, `${prefix}${AGENTS_BLOCK}`, "utf8");
}

async function ensureGitignoreEntry(projectRoot: string, entry: string): Promise<void> {
  const gitignorePath = path.join(projectRoot, ".gitignore");
  const existing = (await pathExists(gitignorePath)) ? await fs.readFile(gitignorePath, "utf8") : "";
  if (existing.split(/\r?\n/).includes(entry)) return;
  const prefix = existing.trim().length > 0 ? `${existing.trimEnd()}\n` : "";
  await fs.writeFile(gitignorePath, `${prefix}${entry}\n`, "utf8");
}
