import fs from "node:fs/promises";
import yaml from "js-yaml";
import { z } from "zod";
import { ensureDirFor, pathExists } from "./fs.js";
import { dlogPath, normalizeRelativePath } from "./paths.js";
import type { DecisionRecord, DlogFile } from "./types.js";

const rejectedSchema = z.object({
  approach: z.string().min(1),
  reason: z.string().min(1)
});

const historySchema = z.object({
  id: z.string().min(1),
  chose: z.string().min(1),
  because: z.string().optional(),
  timestamp: z.string().optional(),
  superseded_reason: z.string().optional()
});

const decisionSchema = z.object({
  id: z.string().min(1),
  lines_hint: z.tuple([z.number().int().positive(), z.number().int().positive()]),
  fingerprint: z.string().min(1),
  chose: z.string().min(1),
  because: z.string().min(1),
  rejected: z.array(rejectedSchema),
  expires_if: z.string().optional(),
  session: z.string().optional(),
  timestamp: z.string().min(1),
  history: z.array(historySchema).default([]),
  status: z.enum(["UNDECIDED", "STALE", "ORPHAN"]).optional(),
  stale_since: z.string().optional(),
  supersedes: z.string().optional()
}) satisfies z.ZodType<DecisionRecord, z.ZodTypeDef, unknown>;

const dlogSchema = z.object({
  file: z.string().min(1),
  schema: z.literal(1),
  decisions: z.record(decisionSchema)
}) satisfies z.ZodType<DlogFile, z.ZodTypeDef, unknown>;

export function createEmptyDlog(file: string): DlogFile {
  return {
    file: normalizeRelativePath(file),
    schema: 1,
    decisions: {}
  };
}

export function validateDlog(value: unknown): DlogFile {
  return dlogSchema.parse(value);
}

export async function loadDlog(projectRoot: string, file: string): Promise<DlogFile | undefined> {
  const filePath = dlogPath(projectRoot, file);
  if (!(await pathExists(filePath))) return undefined;
  const parsed = yaml.load(await fs.readFile(filePath, "utf8"));
  return validateDlog(parsed);
}

export async function loadOrCreateDlog(projectRoot: string, file: string): Promise<DlogFile> {
  return (await loadDlog(projectRoot, file)) ?? createEmptyDlog(file);
}

export async function writeDlog(projectRoot: string, dlog: DlogFile): Promise<void> {
  const filePath = dlogPath(projectRoot, dlog.file);
  const validated = validateDlog(dlog);
  await ensureDirFor(filePath);
  const rendered = yaml.dump(validated, {
    lineWidth: 100,
    noRefs: true,
    sortKeys: false
  });
  await fs.writeFile(filePath, rendered, "utf8");
}
