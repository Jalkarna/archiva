#!/usr/bin/env node
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const root = process.cwd();
const bin = path.join(root, "bin", "archiva.js");

async function main() {
  const fixture = await fs.mkdtemp(path.join(os.tmpdir(), "archiva-bench-"));
  await fs.mkdir(path.join(fixture, "src", "auth"), { recursive: true });
  await fs.writeFile(path.join(fixture, "src", "auth", "session.ts"), sourceFixture(), "utf8");

  await run(["init"], fixture);
  const decisions = [
    {
      file: "src/auth/session.ts",
      anchor: "fn:processCheckout",
      lines: [1, 11],
      chose: "optimistic locking via version field increment",
      because: "checkout and inventory deduction can race under concurrent carts",
      rejected: [
        { approach: "SELECT FOR UPDATE", reason: "deadlocks under concurrent carts touching the same SKU" },
        { approach: "queue serialization", reason: "adds latency to the checkout path" }
      ]
    },
    {
      file: "src/auth/session.ts",
      anchor: "fn:refundOrder",
      lines: [13, 21],
      chose: "refund only settled payments",
      because: "pending payments can still fail and should not create reversal records",
      rejected: [
        { approach: "refund any payment id", reason: "creates reversals for payments that never settled" }
      ]
    },
    {
      file: "src/auth/session.ts",
      anchor: "fn:SessionManager.rotate",
      lines: [24, 29],
      chose: "rotate sessions by replacing the token",
      because: "reusing token ids makes stolen-session detection ambiguous",
      rejected: [
        { approach: "extend the same token", reason: "cannot distinguish legitimate extension from replay" }
      ]
    }
  ];

  for (const decision of decisions) {
    await run(["write-decision", "--json", JSON.stringify(decision)], fixture);
  }

  const dlogPath = path.join(fixture, ".decisions", "src", "auth", "session.ts.dlog");
  const dmapPath = path.join(fixture, ".decisions", "src", "auth", "session.ts.dmap");
  const dlog = await fs.readFile(dlogPath, "utf8");
  const dmap = await fs.readFile(dmapPath, "utf8");
  const sessionStart = await run(["hooks", "session-start"], fixture);

  const fullBytes = Buffer.byteLength(dlog);
  const mapBytes = Buffer.byteLength(dmap);
  const sessionBytes = Buffer.byteLength(sessionStart);
  const reduction = ((1 - mapBytes / fullBytes) * 100).toFixed(1);
  const sessionReduction = ((1 - sessionBytes / fullBytes) * 100).toFixed(1);

  console.log("Archiva benchmark fixture");
  console.log("=========================");
  console.log(`decisions: ${decisions.length}`);
  console.log(`full .dlog bytes: ${fullBytes}`);
  console.log(`compact .dmap bytes: ${mapBytes}`);
  console.log(`session-start bytes: ${sessionBytes}`);
  console.log(`.dmap vs .dlog byte reduction: ${reduction}%`);
  console.log(`session context vs .dlog byte reduction: ${sessionReduction}%`);
  console.log(`average .dmap bytes per decision: ${(mapBytes / decisions.length).toFixed(1)}`);
  console.log("");
  console.log("Note: byte counts are a proxy for context footprint, not a model-quality benchmark.");
  console.log(`fixture: ${fixture}`);
}

async function run(args, cwd) {
  const { stdout } = await execFileAsync("node", [bin, ...args], { cwd });
  return stdout.trim();
}

function sourceFixture() {
  return `function processCheckout(cartId: string, version: number) {
  const current = loadCart(cartId)
  if (current.version !== version) {
    throw new Error("conflict")
  }
  reserveInventory(current.items)
  chargePayment(current.paymentId)
  current.version += 1
  return current
}

function refundOrder(paymentId: string, state: "pending" | "settled") {
  if (state !== "settled") {
    throw new Error("not settled")
  }
  createReversal(paymentId)
  return true
}

class SessionManager {
  rotate(userId: string) {
    const next = createToken(userId)
    revokeOldTokens(userId)
    return next
  }
}

declare function loadCart(id: string): { version: number; items: string[]; paymentId: string }
declare function reserveInventory(items: string[]): void
declare function chargePayment(id: string): void
declare function createReversal(id: string): void
declare function createToken(id: string): string
declare function revokeOldTokens(id: string): void
`;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
