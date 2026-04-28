import { Command } from "commander";
import { history, why, whyForLine, writeDecision } from "../core/decision.js";
import { postToolUse } from "../core/reanchor.js";
import { initProject } from "./init.js";
import { status } from "./status.js";
import { sessionStart } from "../hooks/session-start.js";
import { formatLintIssues, lintProject } from "../lint/rules.js";
import { startMcpServer } from "../mcp/server.js";

const program = new Command();

program
  .name("archiva")
  .description("Decision layer for agentic codebases.")
  .version("0.1.4");

program
  .command("init")
  .description("Set up Archiva in the current project")
  .option("--gitignore-decisions", "add .decisions/ to .gitignore instead of tracking decisions")
  .action(async (options: { gitignoreDecisions?: boolean }) => {
    await print(initProject(process.cwd(), options));
  });

program
  .command("status")
  .description("Show decision health across the repo")
  .action(async () => {
    await print(status(process.cwd()));
  });

program
  .command("why")
  .argument("<file>")
  .argument("[lineOrAnchor]")
  .description("Explain why code was written")
  .action(async (file: string, lineOrAnchor?: string) => {
    if (lineOrAnchor && /^\d+$/.test(lineOrAnchor)) {
      await print(whyForLine(process.cwd(), file, Number(lineOrAnchor)));
      return;
    }
    await print(why(process.cwd(), file, lineOrAnchor));
  });

program
  .command("history")
  .argument("<file>")
  .argument("<anchor>")
  .description("Show the decision history chain for an anchor")
  .action(async (file: string, anchor: string) => {
    await print(history(process.cwd(), file, anchor));
  });

program
  .command("lint")
  .description("Run decision lint rules")
  .option("--fix", "apply safe fixes")
  .action(async (options: { fix?: boolean }) => {
    const issues = await lintProject(process.cwd(), options);
    console.log(formatLintIssues(issues));
    if (issues.some((issue) => issue.severity === "error")) process.exitCode = 1;
  });

const hooks = program.command("hooks").description("Run Archiva hook commands");

hooks
  .command("session-start")
  .description("Print compact decision context")
  .action(async () => {
    await print(sessionStart(process.cwd()));
  });

hooks
  .command("post-tool-use")
  .description("Re-anchor decisions after a file edit")
  .argument("[file]")
  .action(async (file?: string) => {
    const target = file ?? process.env.ARCHIVA_FILE;
    if (!target) throw new Error("Missing file path. Pass one or set ARCHIVA_FILE.");
    await print(postToolUse(process.cwd(), target));
  });

program
  .command("mcp")
  .description("Start the Archiva MCP server over stdio")
  .action(async () => {
    await startMcpServer(process.cwd());
  });

program
  .command("write-decision")
  .description("Record a decision from JSON on stdin or --json")
  .option("--json <json>", "write_decision input JSON")
  .action(async (options: { json?: string }) => {
    const raw = options.json ?? (await readStdin());
    const input = JSON.parse(raw);
    const decision = await writeDecision(process.cwd(), input);
    await print(Promise.resolve(`Recorded ${decision.id}.`));
  });

program.parseAsync(process.argv).catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});

async function print(value: Promise<string>): Promise<void> {
  console.log(await value);
}

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString("utf8");
}
