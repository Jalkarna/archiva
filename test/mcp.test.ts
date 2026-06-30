import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { handleRequest } from "../src/mcp/server.js";

type ToolResult = { content: Array<{ type: string; text: string }> };

async function tempProject(): Promise<string> {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-mcp-test-"));
  await fs.mkdir(path.join(root, "src"), { recursive: true });
  return root;
}

function toolText(result: unknown): string {
  return (result as ToolResult).content[0]?.text ?? "";
}

describe("mcp server", () => {
  it("advertises protocol version and tool catalog", async () => {
    const root = await tempProject();

    const init = (await handleRequest(root, "initialize", {})) as {
      protocolVersion: string;
      serverInfo: { name: string; version: string };
    };
    expect(init.protocolVersion).toBe("2024-11-05");
    expect(init.serverInfo.name).toBe("archiva");

    const list = (await handleRequest(root, "tools/list", {})) as { tools: Array<{ name: string }> };
    expect(list.tools.map((tool) => tool.name).sort()).toEqual(["ghost_check", "why", "write_decision"]);
  });

  it("records a decision then reads it back through why", async () => {
    const root = await tempProject();
    await fs.writeFile(path.join(root, "src/session.ts"), "export function processCheckout() {\n  return \"ok\";\n}\n", "utf8");

    const write = await handleRequest(root, "tools/call", {
      name: "write_decision",
      arguments: {
        file: "src/session.ts",
        anchor: "fn:processCheckout",
        lines: [1, 3],
        chose: "optimistic locking via version field",
        because: "checkout and inventory deduction race under concurrent carts",
        rejected: [{ approach: "SELECT FOR UPDATE", reason: "deadlocks on hot SKUs" }]
      }
    });
    expect(toolText(write)).toContain("dec_001");

    const why = await handleRequest(root, "tools/call", {
      name: "why",
      arguments: { file: "src/session.ts", anchor: "fn:processCheckout" }
    });
    const text = toolText(why);
    expect(text).toContain("optimistic locking");
    expect(text).toContain("SELECT FOR UPDATE");
  });

  it("rejects a decision written against a missing anchor and lists real ones", async () => {
    const root = await tempProject();
    await fs.writeFile(path.join(root, "src/session.ts"), "export function processCheckout() {\n  return \"ok\";\n}\n", "utf8");

    await expect(
      handleRequest(root, "tools/call", {
        name: "write_decision",
        arguments: {
          file: "src/session.ts",
          anchor: "fn:doesNotExist",
          lines: [1, 3],
          chose: "anything",
          because: "fixture",
          rejected: []
        }
      })
    ).rejects.toThrow(/does not exist[\s\S]*fn:processCheckout/);
  });

  it("surfaces stale decisions through ghost_check after code drifts", async () => {
    const root = await tempProject();
    const target = path.join(root, "src/drift.ts");
    await fs.writeFile(target, "export function compute() {\n  return 1;\n}\n", "utf8");

    await handleRequest(root, "tools/call", {
      name: "write_decision",
      arguments: {
        file: "src/drift.ts",
        anchor: "fn:compute",
        lines: [1, 3],
        chose: "return constant",
        because: "fixture setup",
        rejected: []
      }
    });

    await fs.writeFile(target, "export function compute() {\n  return 999;\n}\n", "utf8");

    const ghost = await handleRequest(root, "tools/call", {
      name: "ghost_check",
      arguments: { file: "src/drift.ts" }
    });
    expect(toolText(ghost)).toContain("arc/stale");
  });

  it("raises on unknown methods and tools", async () => {
    const root = await tempProject();
    await expect(handleRequest(root, "tools/call", { name: "nope", arguments: {} })).rejects.toThrow(/Unknown tool/);
    await expect(handleRequest(root, "does/not/exist", {})).rejects.toThrow(/Unsupported MCP method/);
  });

  it("rejects write_decision input that fails shared validation", async () => {
    const root = await tempProject();
    await fs.writeFile(path.join(root, "src/session.ts"), "export function processCheckout() {\n  return \"ok\";\n}\n", "utf8");

    await expect(
      handleRequest(root, "tools/call", {
        name: "write_decision",
        arguments: {
          file: "src/session.ts",
          anchor: "fn:processCheckout",
          lines: [1, 3],
          chose: "",
          because: "fixture",
          rejected: []
        }
      })
    ).rejects.toThrow();
  });
});
