import readline from "node:readline";
import { z } from "zod";
import { why, writeDecision } from "../core/decision.js";
import { lintProject } from "../lint/rules.js";

const writeDecisionSchema = z.object({
  file: z.string(),
  anchor: z.string(),
  lines: z.tuple([z.number().int(), z.number().int()]),
  chose: z.string(),
  because: z.string(),
  rejected: z.array(z.object({ approach: z.string(), reason: z.string() })),
  expires_if: z.string().optional(),
  supersedes: z.string().optional()
});

const whySchema = z.object({
  file: z.string(),
  anchor: z.string().optional()
});

const ghostCheckSchema = z.object({
  file: z.string()
});

export async function startMcpServer(projectRoot: string): Promise<void> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout, terminal: false });
  let queue = Promise.resolve();
  rl.on("line", (line) => {
    if (!line.trim()) return;
    queue = queue.then(() => handleLine(projectRoot, line));
  });
}

async function handleLine(projectRoot: string, line: string): Promise<void> {
  let request: { id?: string | number; method?: string; params?: unknown };
  try {
    request = JSON.parse(line) as { id?: string | number; method?: string; params?: unknown };
  } catch (error) {
    respond({
      jsonrpc: "2.0",
      id: null,
      error: {
        code: -32700,
        message: error instanceof Error ? error.message : String(error)
      }
    });
    return;
  }

  if (!request.method) return;
  if (request.method.startsWith("notifications/")) return;
  try {
    const result = await handleRequest(projectRoot, request.method, request.params);
    respond({ jsonrpc: "2.0", id: request.id, result });
  } catch (error) {
    respond({
      jsonrpc: "2.0",
      id: request.id,
      error: {
        code: -32000,
        message: error instanceof Error ? error.message : String(error)
      }
    });
  }
}

async function handleRequest(projectRoot: string, method: string, params: unknown): Promise<unknown> {
  if (method === "initialize") {
    return {
      protocolVersion: "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "archiva", version: "0.1.2" }
    };
  }
  if (method === "tools/list") {
    return { tools: toolDefinitions() };
  }
  if (method === "tools/call") {
    const call = params as { name?: string; arguments?: unknown };
    return callTool(projectRoot, call.name ?? "", call.arguments ?? {});
  }
  throw new Error(`Unsupported MCP method: ${method}`);
}

async function callTool(projectRoot: string, name: string, args: unknown): Promise<unknown> {
  if (name === "write_decision") {
    const input = writeDecisionSchema.parse(args);
    const decision = await writeDecision(projectRoot, input);
    return textResult(`Recorded ${decision.id}.`);
  }
  if (name === "why") {
    const input = whySchema.parse(args);
    return textResult(await why(projectRoot, input.file, input.anchor));
  }
  if (name === "ghost_check") {
    const input = ghostCheckSchema.parse(args);
    const issues = (await lintProject(projectRoot)).filter((issue) => issue.file === input.file);
    return textResult(issues.length === 0 ? `No issues found for ${input.file}.` : issues.map((issue) => `${issue.rule} ${issue.anchor}: ${issue.message}`).join("\n"));
  }
  throw new Error(`Unknown tool: ${name}`);
}

function textResult(text: string): unknown {
  return {
    content: [{ type: "text", text }]
  };
}

function respond(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function toolDefinitions(): unknown[] {
  return [
    {
      name: "write_decision",
      description: "Log a decision you just made: what you chose, why, and what you rejected.",
      inputSchema: {
        type: "object",
        required: ["file", "anchor", "lines", "chose", "because", "rejected"],
        properties: {
          file: { type: "string" },
          anchor: { type: "string" },
          lines: { type: "array", items: { type: "number" }, minItems: 2, maxItems: 2 },
          chose: { type: "string" },
          because: { type: "string" },
          rejected: {
            type: "array",
            items: {
              type: "object",
              required: ["approach", "reason"],
              properties: {
                approach: { type: "string" },
                reason: { type: "string" }
              }
            }
          },
          expires_if: { type: "string" },
          supersedes: { type: "string" }
        }
      }
    },
    {
      name: "why",
      description: "Look up the decision log for a file and anchor before modifying it.",
      inputSchema: {
        type: "object",
        required: ["file"],
        properties: {
          file: { type: "string" },
          anchor: { type: "string" }
        }
      }
    },
    {
      name: "ghost_check",
      description: "Check for stale or orphaned decisions in a file.",
      inputSchema: {
        type: "object",
        required: ["file"],
        properties: {
          file: { type: "string" }
        }
      }
    }
  ];
}
