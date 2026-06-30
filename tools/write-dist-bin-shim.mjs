import fs from "node:fs/promises";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const binDir = path.join(repoRoot, "dist", "bin");
const binPath = path.join(binDir, "archiva.js");
const distPackagePath = path.join(repoRoot, "dist", "package.json");

async function writeDistBinShim() {
  await fs.mkdir(binDir, { recursive: true });
  const packageJson = JSON.parse(await fs.readFile(path.join(repoRoot, "package.json"), "utf8"));
  await fs.writeFile(
    distPackagePath,
    `${JSON.stringify({ name: packageJson.name, version: packageJson.version, type: packageJson.type }, null, 2)}\n`
  );
  await fs.writeFile(binPath, "#!/usr/bin/env node\nimport \"../src/cli/main.js\";\n");
  if (process.platform !== "win32") {
    await fs.chmod(binPath, 0o755);
  }
}

await writeDistBinShim();
