import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";
import { pathExists } from "./fs.js";

const execFileAsync = promisify(execFile);

export async function findGitRoot(startDir: string): Promise<string | undefined> {
  let dir = path.resolve(startDir);
  while (true) {
    if (await pathExists(path.join(dir, ".git"))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

/** Path of `file` (project-relative) at HEAD, resolved from the git work tree root. */
export async function readGitHeadFile(projectRoot: string, file: string): Promise<string> {
  const gitRoot = await findGitRoot(projectRoot);
  if (!gitRoot) {
    throw new Error("Not a git repository");
  }
  const absoluteSource = path.resolve(projectRoot, file);
  const relativeToGit = path.relative(gitRoot, absoluteSource).replaceAll("\\", "/");
  if (relativeToGit.startsWith("..")) {
    throw new Error(`File "${file}" is outside the git repository`);
  }
  const { stdout } = await execFileAsync("git", ["show", `HEAD:${relativeToGit}`], {
    cwd: gitRoot,
    maxBuffer: 10 * 1024 * 1024
  });
  return stdout;
}
