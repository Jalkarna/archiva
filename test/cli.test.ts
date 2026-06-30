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

  it("merges Archiva hooks into existing Claude settings without removing other hooks", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, ".claude"), { recursive: true });
    await fs.writeFile(
      path.join(root, ".claude", "settings.json"),
      JSON.stringify(
        {
          hooks: {
            SessionStart: [{ hooks: [{ type: "command", command: "echo custom-session" }] }],
            PostToolUse: [
              {
                matcher: "Write|Edit|MultiEdit",
                hooks: [{ type: "command", command: "echo custom-post" }]
              }
            ]
          }
        },
        null,
        2
      ),
      "utf8"
    );

    await initProject(root);
    const settings = JSON.parse(await fs.readFile(path.join(root, ".claude", "settings.json"), "utf8")) as {
      hooks: {
        SessionStart: Array<{ hooks: Array<{ command: string }> }>;
        PostToolUse: Array<{ matcher?: string; hooks: Array<{ command: string }> }>;
      };
    };

    const sessionCommands = settings.hooks.SessionStart.flatMap((group) => group.hooks.map((hook) => hook.command));
    expect(sessionCommands).toContain("echo custom-session");
    expect(sessionCommands).toContain("archiva hooks session-start");

    const postGroup = settings.hooks.PostToolUse.find((group) => group.matcher === "Write|Edit|MultiEdit");
    const postCommands = postGroup?.hooks.map((hook) => hook.command) ?? [];
    expect(postCommands).toContain("echo custom-post");
    expect(postCommands).toContain("archiva hooks post-tool-use");
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
