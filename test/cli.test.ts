import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { initProject } from "../src/cli/init.js";
import { status } from "../src/cli/status.js";
import { writeDecision } from "../src/core/decision.js";
import { sessionStart } from "../src/hooks/session-start.js";

describe("cli flows", () => {
  it("initializes project files without ignoring decisions by default", async () => {
    const root = await tempProject();
    await expect(initProject(root)).resolves.toBe("Archiva initialized.");
    await expect(fs.stat(path.join(root, ".decisions"))).resolves.toBeTruthy();
    await expect(fs.readFile(path.join(root, ".claude", "settings.json"), "utf8")).resolves.toContain("archiva");
    await expect(fs.readFile(path.join(root, "AGENTS.md"), "utf8")).resolves.toContain("Decision Logging");
    await expect(fs.stat(path.join(root, ".gitignore"))).rejects.toThrow();
  });

  it("prints status and session context from decisions", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/a.ts"), "function makeThing() {\n  return 1;\n}\n", "utf8");
    await writeDecision(root, {
      file: "src/a.ts",
      anchor: "fn:makeThing",
      lines: [1, 3],
      chose: "plain function",
      because: "small fixture",
      rejected: [{ approach: "class", reason: "unneeded" }]
    });

    expect(await status(root)).toContain("1 decisions");
    expect(await sessionStart(root)).toContain("fn:makeThing");
  });
});

async function tempProject(): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), "archiva-cli-test-"));
}
