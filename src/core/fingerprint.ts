import crypto from "node:crypto";

export function normalizeCode(content: string): string {
  return content
    .split(/\r?\n/)
    .map((line) => line.trim().replace(/\s+/g, " "))
    .filter((line) => line.length > 0)
    .join("\n");
}

export function fingerprint(content: string): string {
  return crypto.createHash("sha256").update(normalizeCode(content)).digest("hex").slice(0, 8);
}

export function getLines(content: string, range: [number, number]): string {
  const [start, end] = range;
  return content
    .split(/\r?\n/)
    .slice(Math.max(0, start - 1), end)
    .join("\n");
}
