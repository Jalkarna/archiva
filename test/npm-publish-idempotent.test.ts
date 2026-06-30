import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";

const repoRoot = path.dirname(path.dirname(new URL(import.meta.url).pathname));
const helper = path.join(repoRoot, "tools/npm-publish-idempotent.mjs");

describe("npm publish idempotency helper", () => {
  it("skips publish when the exact version is already visible", async () => {
    const fixture = await fakeNpmFixture("already-published");
    const result = runHelper(fixture, ["--spec", "@scope/pkg@1.2.3", "--package", "pkg"]);

    expect(result.status).toBe(0);
    expect(await fixtureLog(fixture)).toEqual([["view", "@scope/pkg@1.2.3", "version"]]);
  });

  it("publishes a missing package and waits for visibility", async () => {
    const fixture = await fakeNpmFixture("publish-success");
    const result = runHelper(fixture, ["--spec", "@scope/pkg@1.2.3", "--package", "pkg"]);

    expect(result.status).toBe(0);
    expect(await fixtureLog(fixture)).toEqual([
      ["view", "@scope/pkg@1.2.3", "version"],
      ["publish", "pkg", "--access", "public"],
      ["view", "@scope/pkg@1.2.3", "version"]
    ]);
  });

  it("treats concurrent publish conflicts as success when the version becomes visible", async () => {
    const fixture = await fakeNpmFixture("publish-conflict-visible");
    const result = runHelper(fixture, ["--spec", "@scope/pkg@1.2.3", "--package", "pkg"]);

    expect(result.status).toBe(0);
    expect(result.stdout).toContain("became visible after publish failure");
    expect(await fixtureLog(fixture)).toEqual([
      ["view", "@scope/pkg@1.2.3", "version"],
      ["publish", "pkg", "--access", "public"],
      ["view", "@scope/pkg@1.2.3", "version"]
    ]);
  });

  it("retries delayed registry visibility after publish success", async () => {
    const fixture = await fakeNpmFixture("publish-delayed-visible");
    const result = runHelper(fixture, ["--spec", "@scope/pkg@1.2.3", "--package", "pkg"]);

    expect(result.status).toBe(0);
    expect(await fixtureLog(fixture)).toEqual([
      ["view", "@scope/pkg@1.2.3", "version"],
      ["publish", "pkg", "--access", "public"],
      ["view", "@scope/pkg@1.2.3", "version"],
      ["view", "@scope/pkg@1.2.3", "version"]
    ]);
  });

  it("can run npm through a shell for cmd-style publish helpers", async () => {
    const fixture = await fakeNpmFixture("publish-success");
    const command = `${JSON.stringify(process.execPath)} ${JSON.stringify(path.join(fixture, "fake-npm.mjs"))}`;
    const result = runHelper(fixture, ["--spec", "@scope/pkg@1.2.3", "--package", "pkg"], {
      ARCHIVA_NPM_COMMAND: command,
      ARCHIVA_NPM_COMMAND_SHELL: "1"
    });

    expect(result.status).toBe(0);
    expect(await fixtureLog(fixture)).toEqual([
      ["view", "@scope/pkg@1.2.3", "version"],
      ["publish", "pkg", "--access", "public"],
      ["view", "@scope/pkg@1.2.3", "version"]
    ]);
  });
});

function runHelper(fixture: string, args: string[], env: Record<string, string> = {}) {
  return spawnSync(process.execPath, [helper, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      ARCHIVA_NPM_COMMAND: path.join(fixture, "fake-npm.mjs"),
      ARCHIVA_FAKE_NPM_STATE: path.join(fixture, "state.json"),
      ARCHIVA_NPM_VIEW_ATTEMPTS: "4",
      ARCHIVA_NPM_VIEW_RETRY_DELAY_MS: "0",
      ...env
    }
  });
}

async function fakeNpmFixture(scenario: string): Promise<string> {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-fake-npm-"));
  await fs.writeFile(path.join(root, "state.json"), JSON.stringify({ scenario, views: 0, publishes: 0, log: [] }), "utf8");
  await fs.writeFile(
    path.join(root, "fake-npm.mjs"),
    `#!/usr/bin/env node
import fs from "node:fs";
const statePath = process.env.ARCHIVA_FAKE_NPM_STATE;
const state = JSON.parse(fs.readFileSync(statePath, "utf8"));
const args = process.argv.slice(2);
state.log.push(args);
function save() { fs.writeFileSync(statePath, JSON.stringify(state), "utf8"); }
if (args[0] === "view") {
  state.views += 1;
  save();
  const visible =
    state.scenario === "already-published" ||
    (state.scenario === "publish-success" && state.publishes >= 1) ||
    (state.scenario === "publish-conflict-visible" && state.publishes >= 1) ||
    (state.scenario === "publish-delayed-visible" && state.publishes >= 1 && state.views >= 3);
  if (visible) {
    process.stdout.write("1.2.3\\n");
    process.exit(0);
  }
  process.stderr.write("E404 not found\\n");
  process.exit(1);
}
if (args[0] === "publish") {
  state.publishes += 1;
  save();
  if (state.scenario === "publish-conflict-visible") {
    process.stderr.write("cannot publish over existing version\\n");
    process.exit(1);
  }
  process.exit(0);
}
process.stderr.write("unexpected fake npm args " + JSON.stringify(args) + "\\n");
process.exit(2);
`,
    { mode: 0o755 }
  );
  return root;
}

async function fixtureLog(root: string): Promise<string[][]> {
  const state = JSON.parse(await fs.readFile(path.join(root, "state.json"), "utf8"));
  return state.log;
}
