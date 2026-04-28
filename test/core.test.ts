import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { extractAnchors } from "../src/core/anchor.js";
import { writeDecision, why, whyForLine } from "../src/core/decision.js";
import { loadDlog } from "../src/core/dlog.js";
import { loadDmap, parseDmap, renderDmap } from "../src/core/dmap.js";
import { fingerprint } from "../src/core/fingerprint.js";
import { applyDiffToRange } from "../src/core/reanchor.js";
import { lintProject } from "../src/lint/rules.js";

describe("dmap", () => {
  it("round trips anchors that contain colons and statuses", () => {
    const entries = parseDmap("4-9:fn:processCheckout\n12-20:block:if_version_mismatch:STALE\n");
    expect(entries).toEqual([
      { startLine: 4, endLine: 9, anchor: "fn:processCheckout", status: undefined },
      { startLine: 12, endLine: 20, anchor: "block:if_version_mismatch", status: "STALE" }
    ]);
    expect(renderDmap(entries)).toContain("12-20:block:if_version_mismatch:STALE");
  });
});

describe("anchors", () => {
  it("extracts functions, methods, exports, arrows, and collisions", () => {
    const anchors = extractAnchors(
      "src/example.ts",
      `
export function handle() {
  if (a && b || c) return 1;
  return 2;
}
function duplicate() {}
function duplicate() {}
class Store {
  save() {
    return true;
  }
}
export const run = () => true;
`
    );

    expect(anchors["fn:handle"]).toBeTruthy();
    expect(anchors["export:handle"]).toBeTruthy();
    expect(anchors["fn:duplicate"]).toBeTruthy();
    expect(anchors["fn:duplicate#2"]).toBeTruthy();
    expect(anchors["class:Store"]).toBeTruthy();
    expect(anchors["fn:Store.save"]).toBeTruthy();
    expect(anchors["fn:run"]).toBeTruthy();
  });
});

describe("decisions", () => {
  it("writes dlog/dmap and answers why queries", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(
      path.join(root, "src/session.ts"),
      `export function processCheckout() {
  return "ok";
}
`,
      "utf8"
    );

    const decision = await writeDecision(root, {
      file: "src/session.ts",
      anchor: "fn:processCheckout",
      lines: [1, 3],
      chose: "simple return for smoke test",
      because: "the fixture only needs a stable anchor",
      rejected: [{ approach: "class wrapper", reason: "adds no behavior" }]
    });

    expect(decision.id).toBe("dec_001");
    expect(await loadDlog(root, "src/session.ts")).toBeTruthy();
    expect(await loadDmap(root, "src/session.ts")).toEqual([
      { startLine: 1, endLine: 3, anchor: "fn:processCheckout", status: undefined }
    ]);
    const explanation = await why(root, "src/session.ts", "fn:processCheckout");
    expect(explanation).toContain("dec_001");
    expect(explanation).toContain("simple return");
    expect(await whyForLine(root, "src/session.ts", 2)).toContain("fn:processCheckout");
  });

  it("reports complex undecided functions", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(
      path.join(root, "src/complex.ts"),
      `function consumeToken(a: boolean, b: boolean) {
  if (a) return 1;
  if (b) return 2;
  for (const item of [1]) {
    if (item) return 3;
  }
  return 4;
}
`,
      "utf8"
    );
    const issues = await lintProject(root);
    expect(issues.some((issue) => issue.rule === "arc/undecided" && issue.anchor === "fn:consumeToken")).toBe(true);
  });

  it("reports stale and orphaned decisions", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/changes.ts"), "function kept() {\n  return 1;\n}\nfunction removed() {\n  return 2;\n}\n", "utf8");
    await writeDecision(root, {
      file: "src/changes.ts",
      anchor: "fn:kept",
      lines: [1, 3],
      chose: "initial kept behavior",
      because: "fixture setup",
      rejected: []
    });
    await writeDecision(root, {
      file: "src/changes.ts",
      anchor: "fn:removed",
      lines: [4, 6],
      chose: "initial removed behavior",
      because: "fixture setup",
      rejected: []
    });

    await fs.writeFile(path.join(root, "src/changes.ts"), "function kept() {\n  return 42;\n}\n", "utf8");
    const issues = await lintProject(root);
    expect(issues.some((issue) => issue.rule === "arc/stale" && issue.anchor === "fn:kept")).toBe(true);
    expect(issues.some((issue) => issue.rule === "arc/orphan" && issue.anchor === "fn:removed")).toBe(true);
  });

  it("preserves superseded decisions in history", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/history.ts"), "function next() {\n  return 1;\n}\n", "utf8");
    const first = await writeDecision(root, {
      file: "src/history.ts",
      anchor: "fn:next",
      lines: [1, 3],
      chose: "first approach",
      because: "fixture setup",
      rejected: []
    });
    const second = await writeDecision(root, {
      file: "src/history.ts",
      anchor: "fn:next",
      lines: [1, 3],
      chose: "second approach",
      because: "new fixture reason",
      rejected: [],
      supersedes: first.id
    });

    expect(second.history).toHaveLength(1);
    expect(second.history[0]?.id).toBe(first.id);
  });

  it("rejects unknown supersedes ids", async () => {
    const root = await tempProject();
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    await fs.writeFile(path.join(root, "src/bad-history.ts"), "function next() {\n  return 1;\n}\n", "utf8");

    await expect(writeDecision(root, {
      file: "src/bad-history.ts",
      anchor: "fn:next",
      lines: [1, 3],
      chose: "second approach",
      because: "new fixture reason",
      rejected: [],
      supersedes: "2026-04-28T15:26:08.322Z"
    })).rejects.toThrow("Cannot supersede unknown decision id");
  });
});

describe("fingerprints and reanchor", () => {
  it("normalizes whitespace and shifts ranges for insertions before anchors", () => {
    expect(fingerprint("const x = 1;\n")).toBe(fingerprint("  const   x   =   1;\n\n"));
    expect(applyDiffToRange("a\nb\nc\n", "a\nx\nb\nc\n", [2, 3])).toEqual([3, 4]);
  });
});

async function tempProject(): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), "archiva-test-"));
}
