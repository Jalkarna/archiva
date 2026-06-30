import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function readVersion(): string {
  let dir = path.dirname(fileURLToPath(import.meta.url));
  while (true) {
    const candidate = path.join(dir, "package.json");
    try {
      const json = JSON.parse(readFileSync(candidate, "utf8")) as { name?: string; version?: string };
      if (json.name === "@jalkarna/archiva" && json.version) return json.version;
    } catch {
      // keep walking
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return "0.0.0";
}

export const ARCHIVA_VERSION: string = readVersion();
