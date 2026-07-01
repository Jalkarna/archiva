import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fsSync from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

type Runtime = {
  name: string;
  command: string;
  prefixArgs: string[];
};

type CommandResult = {
  status: number | null;
  stdout: string;
  stderr: string;
  peakRss: PeakRss;
};

type PeakRss =
  | { status: "measured"; peakRssKb: number }
  | { status: "unavailable"; reason: string };

type ScaleConfig = {
  files: number;
  decisions: number;
  mutateFiles: number;
};

type SyntheticScaleConfig = ScaleConfig & {
  decisionsPerFile: number;
};

type PhaseResult = {
  name: string;
  ms: number;
  peakRss?: PeakRss;
};

type ScaleRunResult = {
  root: string;
  config: ScaleConfig;
  phases: PhaseResult[];
  commandSummaries: Record<string, CommandSummary>;
  artifactSummary: ArtifactSummary;
  semanticSummary?: SeededSemanticSummary | CorpusSemanticSummary;
  parityArtifacts?: Record<string, string>;
};

type CommandSummary = {
  status: number | null;
  stdoutBytes: number;
  stderrBytes: number;
  stdoutHash: string;
  stderrHash: string;
  peakRss: PeakRss;
};

type ArtifactSummary = {
  dlogFiles: number;
  dmapFiles: number;
  sourceFiles: number;
  decisionBytes: number;
};

type SeededSemanticSummary = {
  sampledFiles: number;
  sampledDecisions: number;
  shiftedFiles: number;
  staleDecisions: number;
  cleanDecisions: number;
  lockArtifacts: number;
  tempArtifacts: number;
};

type CorpusSemanticSummary = {
  kind: "corpus";
  decisionFiles: number;
  mutatedFiles: number;
  dlogFiles: number;
  dmapFiles: number;
  shiftedDmapEntries: number;
  stableDmapEntries: number;
  staleDmapEntries: number;
  anchorKinds: Record<string, number>;
  statusMentionsTotal: true;
  sessionMentionsFirstFile: true;
  whyMentionsFirstDecision: true;
};

type CorpusDecision = {
  file: string;
  anchor: string;
  lines: [number, number];
};

type CorpusAnchorCandidate = {
  anchor: string;
  lines: [number, number];
};

type CorpusLanguage = "typescript" | "rust" | "c/cpp";

type CorpusSelection = {
  corpusRoot: string;
  language: CorpusLanguage;
  scannedFiles: number;
  selected: CorpusDecision[];
};

const repoRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const rustBinInput = process.env.ARCHIVA_RUST_BIN ?? "dist-native/archiva";
const rustBin = path.isAbsolute(rustBinInput) ? rustBinInput : path.resolve(repoRoot, rustBinInput);
let rssFileCounter = 0;
const commandMaxBuffer = positiveIntEnv("ARCHIVA_SCALE_COMMAND_MAX_BUFFER_MB", 512) * 1024 * 1024;
const commandTimeoutMs = positiveIntEnv("ARCHIVA_SCALE_COMMAND_TIMEOUT_MS", 600_000);
const processTimeoutTool = detectProcessTimeoutTool();
const rssTool = detectRssTool();

const fullConfig = normalizeConfig({
  files: positiveIntEnv("ARCHIVA_SCALE_FILES", 512),
  decisions: positiveIntEnv("ARCHIVA_SCALE_DECISIONS", 128),
  mutateFiles: positiveIntEnv("ARCHIVA_SCALE_MUTATE_FILES", 96),
  decisionsPerFile: positiveIntEnv("ARCHIVA_SCALE_DECISIONS_PER_FILE", 1)
});
const parityConfig = normalizeConfig({
  files: positiveIntEnv("ARCHIVA_SCALE_PARITY_FILES", 200),
  decisions: positiveIntEnv("ARCHIVA_SCALE_PARITY_DECISIONS", 50),
  mutateFiles: positiveIntEnv("ARCHIVA_SCALE_PARITY_MUTATE_FILES", 50),
  decisionsPerFile: positiveIntEnv("ARCHIVA_SCALE_PARITY_DECISIONS_PER_FILE", 1)
});
const corpusRequested =
  process.env.ARCHIVA_SCALE_CORPUS === "1" || process.env.ARCHIVA_SCALE_CORPUS_ROOT !== undefined;
const corpusConfig = normalizeCorpusConfig({
  files: positiveIntEnv("ARCHIVA_SCALE_CORPUS_FILES", 300),
  decisions: positiveIntEnv("ARCHIVA_SCALE_CORPUS_DECISIONS", 120),
  mutateFiles: positiveIntEnv("ARCHIVA_SCALE_CORPUS_MUTATE_FILES", 80)
});
const corpusMaxFileBytes = positiveIntEnv("ARCHIVA_SCALE_CORPUS_MAX_FILE_BYTES", 256 * 1024);
const corpusLanguage = corpusLanguageEnv("ARCHIVA_SCALE_CORPUS_LANGUAGE");
const seededRequested = process.env.ARCHIVA_SCALE_SEEDED === "1";
const seededConfig = seededRequested
  ? normalizeConfig({
      files: positiveIntEnv("ARCHIVA_SCALE_SEEDED_FILES", 100_000),
      decisions: positiveIntEnv("ARCHIVA_SCALE_SEEDED_DECISIONS", 1_000_000),
      mutateFiles: positiveIntEnv("ARCHIVA_SCALE_SEEDED_MUTATE_FILES", 1_000),
      decisionsPerFile: positiveIntEnv("ARCHIVA_SCALE_SEEDED_DECISIONS_PER_FILE", 10)
    })
  : undefined;

await fs.access(rustBin).catch(() => {
  throw new Error(`Missing Rust binary at ${rustBin}. Run npm run build:rust or set ARCHIVA_RUST_BIN.`);
});

const rustRuntime: Runtime = { name: "rust", command: rustBin, prefixArgs: [] };
const typescriptRuntime: Runtime = {
  name: "typescript",
  command: process.execPath,
  prefixArgs: [path.join(repoRoot, "bin", "archiva.js")]
};

const rustFull = await runScale(rustRuntime, fullConfig, false);
const typescriptParity = await runScale(typescriptRuntime, parityConfig, true);
const rustParity = await runScale(rustRuntime, parityConfig, true);
const parityOk = scaleParityMatches(typescriptParity, rustParity);
let corpusOk = true;
let corpus:
  | {
      corpusRoot: string;
      language: CorpusLanguage;
      validation: "typescript-rust-parity" | "rust-native-only" | "c/cpp-native-only";
      scannedFiles: number;
      selectedFiles: number;
      decisionWrites: number;
      typescript?: Omit<ScaleRunResult, "parityArtifacts"> | ScaleRunResult;
      rust: Omit<ScaleRunResult, "parityArtifacts"> | ScaleRunResult;
    }
  | undefined;
let seeded: Omit<ScaleRunResult, "parityArtifacts"> | undefined;

if (corpusRequested) {
  const corpusRootInput = process.env.ARCHIVA_SCALE_CORPUS_ROOT;
  if (!corpusRootInput) {
    throw new Error("ARCHIVA_SCALE_CORPUS_ROOT is required when ARCHIVA_SCALE_CORPUS=1");
  }
  const corpusRoot = path.resolve(repoRoot, corpusRootInput);
  const selection = await selectCorpus(corpusRoot, corpusConfig, rustRuntime);
  const rustCorpus = await runCorpusScale(rustRuntime, selection.corpusRoot, selection.selected, corpusConfig, true);
  if (selection.language === "rust") {
    assertRustCorpusCoverage(selection, rustCorpus.semanticSummary);
  } else if (selection.language === "c/cpp") {
    assertCxxCorpusCoverage(selection, rustCorpus.semanticSummary);
  }
  const typescriptCorpus =
    selection.language === "typescript"
      ? await runCorpusScale(typescriptRuntime, selection.corpusRoot, selection.selected, corpusConfig, true)
      : undefined;
  corpusOk = typescriptCorpus === undefined || corpusScaleParityMatches(typescriptCorpus, rustCorpus);
  corpus = {
    corpusRoot: selection.corpusRoot,
    language: selection.language,
    validation: selection.language === "typescript" ? "typescript-rust-parity" : `${selection.language}-native-only`,
    scannedFiles: selection.scannedFiles,
    selectedFiles: selection.selected.length,
    decisionWrites: corpusConfig.decisions,
    typescript: typescriptCorpus === undefined ? undefined : corpusOk ? compactRun(typescriptCorpus) : typescriptCorpus,
    rust: corpusOk ? compactRun(rustCorpus) : rustCorpus
  };
}
if (seededConfig) {
  seeded = compactRun(await runSeededScale(rustRuntime, seededConfig));
}

console.log(
  JSON.stringify(
    {
      tool: "archiva-scale-smoke",
	      status: parityOk && corpusOk ? "passed" : "failed",
	      rustBinary: rustBin,
	      commandTimeoutMs,
	      fullConfig,
	      parityConfig,
	      rustFull: compactRun(rustFull),
      parity: parityOk
        ? {
            typescript: compactRun(typescriptParity),
            rust: compactRun(rustParity)
          }
        : {
            typescript: typescriptParity,
            rust: rustParity
          },
      corpus,
      seeded
    },
    null,
    2
  )
);
process.exitCode = parityOk && corpusOk ? 0 : 1;

async function runScale(
  runtime: Runtime,
  config: SyntheticScaleConfig,
  includeParityArtifacts: boolean
): Promise<ScaleRunResult> {
  const root = await tempProject(runtime.name, "scale-smoke");
  const phases: PhaseResult[] = [];
  const phaseRss = new Map<string, PeakRss>();
  const commandSummaries: Record<string, CommandSummary> = {};
  const runCommand = (phase: string, args: string[], input: string, expectedStatuses: number[]): CommandResult => {
    const result =
      runtime.name === "rust"
        ? checkedRunMeasured(runtime, args, input, root, expectedStatuses)
        : checkedRun(runtime, args, input, root, expectedStatuses, "RSS measurement is only enabled for Rust scale runs");
    recordPhaseRss(phaseRss, phase, result.peakRss);
    return result;
  };

  await measure(phases, "generate", async () => {
    await fs.mkdir(path.join(root, "src"), { recursive: true });
    for (let index = 0; index < config.files; index += 1) {
      const absoluteSourceFile = path.join(root, sourceFile(index));
      await fs.mkdir(path.dirname(absoluteSourceFile), { recursive: true });
      await fs.writeFile(absoluteSourceFile, renderSource(index, false, config.decisionsPerFile), "utf8");
    }
  });

  await measure(phases, "git.initial", async () => {
    git(root, ["init"]);
    git(root, ["config", "gc.auto", "0"]);
    git(root, ["add", "src"]);
    git(root, [
      "-c",
      "user.name=Archiva Scale",
      "-c",
      "user.email=archiva@example.invalid",
      "commit",
      "--quiet",
      "-m",
      "initial scale"
    ]);
  });

  await measure(phases, "decision.write", async () => {
    for (let index = 0; index < config.decisions; index += 1) {
      runCommand("decision.write", ["write-decision", "--json", JSON.stringify(decisionInput(index, config))], "", [
        0
      ]);
    }
  }, phaseRss);

  await measure(phases, "mutate", async () => {
    for (let index = 0; index < config.mutateFiles; index += 1) {
      await fs.writeFile(path.join(root, sourceFile(index)), renderSource(index, true, config.decisionsPerFile), "utf8");
    }
  });

  await measure(phases, "post-tool-use", async () => {
    for (let index = 0; index < config.mutateFiles; index += 1) {
      runCommand("post-tool-use", ["hooks", "post-tool-use", sourceFile(index)], "", [0]);
    }
  }, phaseRss);

  await measure(phases, "lint", async () => {
    commandSummaries.lint = summarize(runCommand("lint", ["lint"], "", [0, 1]));
  }, phaseRss);
  await measure(phases, "status", async () => {
    commandSummaries.status = summarize(runCommand("status", ["status"], "", [0]));
  }, phaseRss);
  await measure(phases, "session-start", async () => {
    commandSummaries.sessionStart = summarize(runCommand("session-start", ["hooks", "session-start"], "", [0]));
  }, phaseRss);
  await measure(phases, "why", async () => {
    commandSummaries.why = summarize(runCommand("why", ["why", sourceFile(0), anchorName(0, config)], "", [0]));
  }, phaseRss);

  return {
    root,
    config,
    phases,
    commandSummaries,
    artifactSummary: await summarizeArtifacts(root),
    parityArtifacts: includeParityArtifacts ? await readParityArtifacts(root, config) : undefined
  };
}

async function runSeededScale(runtime: Runtime, config: SyntheticScaleConfig): Promise<ScaleRunResult> {
  const root = await tempProject(runtime.name, "scale-seeded");
  const phases: PhaseResult[] = [];
  const phaseRss = new Map<string, PeakRss>();
  const commandSummaries: Record<string, CommandSummary> = {};
  const runCommand = (phase: string, args: string[], input: string, expectedStatuses: number[]): CommandResult => {
    const result = checkedRunMeasured(runtime, args, input, root, expectedStatuses);
    recordPhaseRss(phaseRss, phase, result.peakRss);
    return result;
  };

  await measure(phases, "generate", async () => {
    for (let index = 0; index < config.files; index += 1) {
      const absoluteSourceFile = path.join(root, sourceFile(index));
      await fs.mkdir(path.dirname(absoluteSourceFile), { recursive: true });
      await fs.writeFile(absoluteSourceFile, renderSource(index, false, config.decisionsPerFile), "utf8");
    }
  });

  await measure(phases, "git.initial", async () => {
    git(root, ["init"]);
    git(root, ["config", "gc.auto", "0"]);
    git(root, ["add", "src"]);
    git(root, [
      "-c",
      "user.name=Archiva Seeded Scale",
      "-c",
      "user.email=archiva@example.invalid",
      "commit",
      "--quiet",
      "-m",
      "initial seeded scale"
    ]);
  });

  await measure(phases, "seed.decisions", async () => {
    await seedSyntheticDecisionArtifacts(root, config);
  });

  await measure(phases, "mutate", async () => {
    for (let index = 0; index < config.mutateFiles; index += 1) {
      await fs.writeFile(path.join(root, sourceFile(index)), renderSource(index, true, config.decisionsPerFile), "utf8");
    }
  });

  await measure(phases, "post-tool-use", async () => {
    for (let index = 0; index < config.mutateFiles; index += 1) {
      runCommand("post-tool-use", ["hooks", "post-tool-use", sourceFile(index)], "", [0]);
    }
  }, phaseRss);

  await measure(phases, "lint", async () => {
    commandSummaries.lint = summarize(runCommand("lint", ["lint"], "", [0, 1]));
  }, phaseRss);
  await measure(phases, "status", async () => {
    commandSummaries.status = summarize(runCommand("status", ["status"], "", [0]));
  }, phaseRss);
  await measure(phases, "session-start", async () => {
    commandSummaries.sessionStart = summarize(runCommand("session-start", ["hooks", "session-start"], "", [0]));
  }, phaseRss);
  await measure(phases, "why", async () => {
    commandSummaries.why = summarize(runCommand("why", ["why", sourceFile(0), anchorName(0, config)], "", [0]));
  }, phaseRss);

  let semanticSummary: SeededSemanticSummary | undefined;
  await measure(phases, "seed.verify", async () => {
    semanticSummary = await verifySeededScaleArtifacts(root, config);
  });

  return {
    root,
    config,
    phases,
    commandSummaries,
    artifactSummary: await summarizeArtifacts(root),
    semanticSummary
  };
}

async function runCorpusScale(
  runtime: Runtime,
  corpusRoot: string,
  corpusFiles: CorpusDecision[],
  config: ScaleConfig,
  includeParityArtifacts: boolean
): Promise<ScaleRunResult> {
  const root = await tempProject(runtime.name, "scale-corpus");
  const phases: PhaseResult[] = [];
  const phaseRss = new Map<string, PeakRss>();
  const commandSummaries: Record<string, CommandSummary> = {};
  const commandOutputs: Partial<Record<"status" | "sessionStart" | "why", string>> = {};
  const decisionFiles = corpusFiles.slice(0, config.decisions);
  const mutatedFiles = decisionFiles.slice(0, config.mutateFiles);
  const runCommand = (phase: string, args: string[], input: string, expectedStatuses: number[]): CommandResult => {
    const result =
      runtime.name === "rust"
        ? checkedRunMeasured(runtime, args, input, root, expectedStatuses)
        : checkedRun(runtime, args, input, root, expectedStatuses, "RSS measurement is only enabled for Rust corpus runs");
    recordPhaseRss(phaseRss, phase, result.peakRss);
    return result;
  };

  await measure(phases, "copy.corpus", async () => {
    for (const entry of corpusFiles) {
      const absoluteTarget = path.join(root, entry.file);
      await fs.mkdir(path.dirname(absoluteTarget), { recursive: true });
      await fs.copyFile(path.join(corpusRoot, entry.file), absoluteTarget);
    }
  });

  await measure(phases, "git.initial", async () => {
    git(root, ["init"]);
    git(root, ["add", "."]);
    git(root, [
      "-c",
      "user.name=Archiva Corpus",
      "-c",
      "user.email=archiva@example.invalid",
      "commit",
      "-m",
      "initial corpus"
    ]);
  });

  await measure(phases, "decision.write", async () => {
    for (let index = 0; index < decisionFiles.length; index += 1) {
      runCommand(
        "decision.write",
        ["write-decision", "--json", JSON.stringify(corpusDecisionInput(decisionFiles[index], index))],
        "",
        [0]
      );
    }
  }, phaseRss);
  const writtenDecisionFiles = await readWrittenCorpusDecisions(root, decisionFiles);
  const writtenMutatedFiles = writtenDecisionFiles.slice(0, mutatedFiles.length);

  await measure(phases, "mutate", async () => {
    for (let index = 0; index < mutatedFiles.length; index += 1) {
      await mutateCorpusFile(root, mutatedFiles[index], index);
    }
  });

  await measure(phases, "post-tool-use", async () => {
    for (const entry of mutatedFiles) {
      runCommand("post-tool-use", ["hooks", "post-tool-use", entry.file], "", [0]);
    }
  }, phaseRss);

  await measure(phases, "lint", async () => {
    commandSummaries.lint = summarize(runCommand("lint", ["lint"], "", [0, 1]));
  }, phaseRss);
  await measure(phases, "status", async () => {
    const result = runCommand("status", ["status"], "", [0]);
    commandOutputs.status = result.stdout;
    commandSummaries.status = summarize(result);
  }, phaseRss);
  await measure(phases, "session-start", async () => {
    const result = runCommand("session-start", ["hooks", "session-start"], "", [0]);
    commandOutputs.sessionStart = result.stdout;
    commandSummaries.sessionStart = summarize(result);
  }, phaseRss);
  await measure(phases, "why", async () => {
    const first = decisionFiles[0];
    const result = runCommand("why", ["why", first.file, first.anchor], "", [0]);
    commandOutputs.why = result.stdout;
    commandSummaries.why = summarize(result);
  }, phaseRss);
  const semanticSummary = await verifyCorpusScaleArtifacts(
    root,
    writtenDecisionFiles,
    writtenMutatedFiles,
    commandOutputs
  );

  return {
    root,
    config,
    phases,
    commandSummaries,
    artifactSummary: await summarizeArtifacts(root),
    semanticSummary,
    parityArtifacts: includeParityArtifacts ? await readDecisionArtifacts(root, decisionFiles) : undefined
  };
}

async function readWrittenCorpusDecisions(root: string, decisions: CorpusDecision[]): Promise<CorpusDecision[]> {
  const written: CorpusDecision[] = [];
  for (const decision of decisions) {
    const dmapPath = path.join(root, `.decisions/${decision.file}.dmap`);
    const dmap = await fs.readFile(dmapPath, "utf8");
    const dmapEntry = parseCorpusDmapEntry(dmap.trimEnd(), decision);
    written.push({
      ...decision,
      lines: [dmapEntry.start, dmapEntry.end]
    });
  }
  return written;
}

async function verifyCorpusScaleArtifacts(
  root: string,
  decisionFiles: CorpusDecision[],
  mutatedFiles: CorpusDecision[],
  commandOutputs: Partial<Record<"status" | "sessionStart" | "why", string>>
): Promise<CorpusSemanticSummary> {
  if (decisionFiles.length === 0) {
    throw new Error("Corpus verification received no decision files");
  }
  const mutated = new Set(mutatedFiles.map((entry) => entry.file));
  const anchorKinds: Record<string, number> = {};
  let dlogFiles = 0;
  let dmapFiles = 0;
  let shiftedDmapEntries = 0;
  let stableDmapEntries = 0;
  let staleDmapEntries = 0;

  for (let index = 0; index < decisionFiles.length; index += 1) {
    const entry = decisionFiles[index];
    const wasMutated = mutated.has(entry.file);
    const dlogPath = path.join(root, `.decisions/${entry.file}.dlog`);
    const dmapPath = path.join(root, `.decisions/${entry.file}.dmap`);
    const dlog = await fs.readFile(dlogPath, "utf8");
    const dmap = await fs.readFile(dmapPath, "utf8");
    const dmapEntry = parseCorpusDmapEntry(dmap.trimEnd(), entry);
    const block = corpusDecisionBlock(dlog, entry.anchor);
    const anchorKind = corpusAnchorKind(entry.anchor);

    dlogFiles += 1;
    dmapFiles += 1;
    anchorKinds[anchorKind] = (anchorKinds[anchorKind] ?? 0) + 1;
    if (dmapEntry.start > entry.lines[0]) {
      shiftedDmapEntries += 1;
    } else {
      stableDmapEntries += 1;
    }
    if (dmapEntry.stale) {
      staleDmapEntries += 1;
    }

    if (wasMutated && dmapEntry.start <= entry.lines[0] && !dmapEntry.stale) {
      throw new Error(
        `Corpus verification expected ${entry.file} ${entry.anchor} to shift or become stale after mutation; got ${dmap.trimEnd()}`
      );
    }
    if (!wasMutated && dmapEntry.start !== entry.lines[0]) {
      throw new Error(
        `Corpus verification expected ${entry.file} ${entry.anchor} to stay at line ${entry.lines[0]}; got ${dmap.trimEnd()}`
      );
    }
    assertIncludes(block, `    chose: corpus decision ${index}`, `${entry.file} ${entry.anchor} chose`);
    assertIncludes(block, "    because:", `${entry.file} ${entry.anchor} because`);
    assertIncludes(
      block,
      `    lines_hint:\n      - ${dmapEntry.start}\n      - ${dmapEntry.end}\n`,
      `${entry.file} ${entry.anchor} line hint`
    );
  }

  const first = decisionFiles[0];
  assertIncludes(commandOutputs.why ?? "", first.anchor, "corpus why output anchor");
  assertIncludes(commandOutputs.why ?? "", "corpus decision 0", "corpus why output decision text");
  assertIncludes(commandOutputs.status ?? "", `Total: ${decisionFiles.length} decisions`, "corpus status total");
  assertIncludes(commandOutputs.sessionStart ?? "", first.file, "corpus session-start first file");
  if (mutatedFiles.length > 0 && shiftedDmapEntries + staleDmapEntries === 0) {
    throw new Error("Corpus verification found no shifted or stale dmap entries after mutation");
  }

  return {
    kind: "corpus",
    decisionFiles: decisionFiles.length,
    mutatedFiles: mutatedFiles.length,
    dlogFiles,
    dmapFiles,
    shiftedDmapEntries,
    stableDmapEntries,
    staleDmapEntries,
    anchorKinds: sortedRecord(anchorKinds),
    statusMentionsTotal: true,
    sessionMentionsFirstFile: true,
    whyMentionsFirstDecision: true
  };
}

function parseCorpusDmapEntry(
  dmapEntry: string,
  decision: CorpusDecision
): { start: number; end: number; stale: boolean } {
  const stale = dmapEntry.endsWith(":STALE");
  const base = stale ? dmapEntry.slice(0, -":STALE".length) : dmapEntry;
  const anchorSuffix = `:${decision.anchor}`;
  if (!base.endsWith(anchorSuffix)) {
    throw new Error(`Corpus verification expected ${decision.anchor} in dmap, got ${JSON.stringify(dmapEntry)}`);
  }
  const range = base.slice(0, -anchorSuffix.length);
  const [startText, endText, extra] = range.split("-");
  const start = Number(startText);
  const end = Number(endText);
  if (extra !== undefined || !Number.isInteger(start) || !Number.isInteger(end) || start <= 0 || end < start) {
    throw new Error(`Corpus verification found malformed dmap range ${JSON.stringify(dmapEntry)}`);
  }
  return { start, end, stale };
}

function corpusDecisionBlock(dlog: string, anchor: string): string {
  const marker = `  ${anchor}:\n`;
  const start = dlog.indexOf(marker);
  if (start === -1) {
    throw new Error(`Corpus verification could not find ${anchor} in dlog`);
  }
  const afterMarker = start + marker.length;
  const next = dlog.slice(afterMarker).search(/\n  [^ \n][^\n]*:\n/);
  return dlog.slice(start, next === -1 ? dlog.length : afterMarker + next + 1);
}

function corpusAnchorKind(anchor: string): string {
  if (anchor.startsWith("fn:")) {
    return anchor.slice("fn:".length).includes(".") ? "method" : "function";
  }
  const separator = anchor.indexOf(":");
  return separator === -1 ? "unknown" : anchor.slice(0, separator);
}

function assertRustCorpusCoverage(
  selection: CorpusSelection,
  semanticSummary: ScaleRunResult["semanticSummary"]
): void {
  if (!semanticSummary || !("kind" in semanticSummary) || semanticSummary.kind !== "corpus") {
    throw new Error("Rust corpus validation did not produce a corpus semantic summary");
  }
  const coveredKinds = Object.keys(semanticSummary.anchorKinds).filter(
    (kind) => semanticSummary.anchorKinds[kind] > 0
  );
  if (selection.selected.length >= 12 && coveredKinds.length < 2) {
    throw new Error(
      `Rust corpus validation covered only ${coveredKinds.join(", ") || "no"} anchor kind; expected at least two kinds`
    );
  }
  const structuralKinds = ["enum", "impl", "method", "mod", "struct", "trait"];
  if (
    selection.selected.length >= 24 &&
    !structuralKinds.some((kind) => (semanticSummary.anchorKinds[kind] ?? 0) > 0)
  ) {
    throw new Error("Rust corpus validation selected no structural, impl, or method anchors");
  }
}

function assertCxxCorpusCoverage(
  selection: CorpusSelection,
  semanticSummary: ScaleRunResult["semanticSummary"]
): void {
  if (!semanticSummary || !("kind" in semanticSummary) || semanticSummary.kind !== "corpus") {
    throw new Error("C/C++ corpus validation did not produce a corpus semantic summary");
  }
  const coveredKinds = Object.keys(semanticSummary.anchorKinds).filter(
    (kind) => semanticSummary.anchorKinds[kind] > 0
  );
  if (selection.selected.length >= 12 && coveredKinds.length < 2) {
    throw new Error(
      `C/C++ corpus validation covered only ${coveredKinds.join(", ") || "no"} anchor kind; expected at least two kinds`
    );
  }
  const structuralKinds = ["class", "enum", "method", "struct"];
  if (
    selection.selected.length >= 24 &&
    !structuralKinds.some((kind) => (semanticSummary.anchorKinds[kind] ?? 0) > 0)
  ) {
    throw new Error("C/C++ corpus validation selected no class, struct, enum, or method anchors");
  }
}

async function measure(
  phases: PhaseResult[],
  name: string,
  action: () => Promise<void>,
  phaseRss?: Map<string, PeakRss>
): Promise<void> {
  const start = performance.now();
  await action();
  const phase: PhaseResult = { name, ms: round(performance.now() - start) };
  const peakRss = phaseRss?.get(name);
  if (peakRss) {
    phase.peakRss = peakRss;
  }
  phases.push(phase);
}

function compactRun(result: ScaleRunResult): Omit<ScaleRunResult, "parityArtifacts"> {
  return {
    root: result.root,
    config: result.config,
    phases: result.phases,
    commandSummaries: result.commandSummaries,
    artifactSummary: result.artifactSummary,
    semanticSummary: result.semanticSummary
  };
}

function scaleParityMatches(left: ScaleRunResult, right: ScaleRunResult): boolean {
  return (
    JSON.stringify(left.parityArtifacts) === JSON.stringify(right.parityArtifacts) &&
    JSON.stringify(comparableCommandSummaries(left.commandSummaries)) ===
      JSON.stringify(comparableCommandSummaries(right.commandSummaries))
  );
}

function corpusScaleParityMatches(left: ScaleRunResult, right: ScaleRunResult): boolean {
  return (
    JSON.stringify(left.parityArtifacts) === JSON.stringify(right.parityArtifacts) &&
    JSON.stringify(left.artifactSummary) === JSON.stringify(right.artifactSummary) &&
    JSON.stringify(left.semanticSummary) === JSON.stringify(right.semanticSummary) &&
    JSON.stringify(comparableCommandOutcomes(left.commandSummaries)) ===
      JSON.stringify(comparableCommandOutcomes(right.commandSummaries))
  );
}

function comparableCommandSummaries(commandSummaries: Record<string, CommandSummary>): Record<string, Omit<CommandSummary, "peakRss">> {
  const output: Record<string, Omit<CommandSummary, "peakRss">> = {};
  for (const key of Object.keys(commandSummaries).sort()) {
    const { peakRss: _peakRss, ...summary } = commandSummaries[key];
    output[key] = summary;
  }
  return output;
}

function comparableCommandOutcomes(
  commandSummaries: Record<string, CommandSummary>
): Record<string, Pick<CommandSummary, "status" | "stderrBytes" | "stderrHash">> {
  const output: Record<string, Pick<CommandSummary, "status" | "stderrBytes" | "stderrHash">> = {};
  for (const key of Object.keys(commandSummaries).sort()) {
    const { status, stderrBytes, stderrHash } = commandSummaries[key];
    output[key] = { status, stderrBytes, stderrHash };
  }
  return output;
}

function decisionInput(index: number, config: SyntheticScaleConfig): unknown {
  const location = syntheticDecisionLocation(index, config);
  return {
    file: sourceFile(location.fileIndex),
    anchor: anchorName(index, config),
    lines: syntheticFunctionLines(location.slot),
    chose: `scale decision ${index}`,
    because: "synthetic scale smoke needs persisted decisions across many files",
    rejected: [{ approach: "small fixture only", reason: "does not exercise repository-scale scanning" }]
  };
}

function corpusDecisionInput(entry: CorpusDecision, index: number): unknown {
  return {
    file: entry.file,
    anchor: entry.anchor,
    lines: entry.lines,
    chose: `corpus decision ${index}`,
    because: "external corpus validation needs persisted decisions against real source files",
    rejected: [
      { approach: "synthetic-only scale validation", reason: "does not exercise real repository layouts or syntax" }
    ]
  };
}

function renderSource(index: number, mutated: boolean, functionsPerFile: number): string {
  const prefix = mutated ? `// scale mutation ${index}\n// inserted line for ${functionName(index)}\n` : "";
  const functions = Array.from({ length: functionsPerFile }, (_, slot) => renderFunction(index, slot, mutated));
  return `${prefix}${functions.join("\n\n")}\n`;
}

function renderFunction(fileIndex: number, slot: number, mutated: boolean): string {
  const decisionIndex = fileIndex + slot;
  const changed = mutated && seededFunctionChangesWhenMutated(fileIndex, slot);
  return `export function ${functionName(fileIndex, slot)}(input: number) {
  if (input > ${decisionIndex % 17} && input < ${(decisionIndex % 17) + 100}) {
    return input + ${decisionIndex + (changed ? 1 : 0)};
  }
  return input - ${decisionIndex % 11};
}`;
}

function seededFunctionChangesWhenMutated(fileIndex: number, slot: number): boolean {
  return (fileIndex + slot) % 4 === 0;
}

function sourceFile(index: number): string {
  const bucket = Math.floor(index / 100);
  return `src/scale-${bucket}/file-${index}.ts`;
}

function functionName(fileIndex: number, slot = 0): string {
  return `scale_${fileIndex}_${slot}`;
}

function anchorName(index: number, config: SyntheticScaleConfig): string {
  const location = syntheticDecisionLocation(index, config);
  return `fn:${functionName(location.fileIndex, location.slot)}`;
}

function syntheticDecisionLocation(index: number, config: SyntheticScaleConfig): { fileIndex: number; slot: number } {
  return {
    fileIndex: Math.floor(index / config.decisionsPerFile),
    slot: index % config.decisionsPerFile
  };
}

function syntheticFunctionLines(slot: number): [number, number] {
  const start = slot * 7 + 1;
  return [start, start + 5];
}

function syntheticDecisionSources(config: SyntheticScaleConfig): string[] {
  const sources = new Set<string>();
  for (let index = 0; index < config.decisions; index += 1) {
    sources.add(sourceFile(syntheticDecisionLocation(index, config).fileIndex));
  }
  return [...sources].sort();
}

async function seedSyntheticDecisionArtifacts(root: string, config: SyntheticScaleConfig): Promise<void> {
  const decisionFiles = Math.ceil(config.decisions / config.decisionsPerFile);
  for (let fileIndex = 0; fileIndex < decisionFiles; fileIndex += 1) {
    const source = sourceFile(fileIndex);
    const decisionsInFile = Math.min(config.decisionsPerFile, config.decisions - fileIndex * config.decisionsPerFile);
    const content = renderSource(fileIndex, false, config.decisionsPerFile);
    const dlogPath = path.join(root, `.decisions/${source}.dlog`);
    const dmapPath = path.join(root, `.decisions/${source}.dmap`);
    await fs.mkdir(path.dirname(dlogPath), { recursive: true });
    await fs.writeFile(dlogPath, renderSeededDlog(source, fileIndex, decisionsInFile, config, content), "utf8");
    await fs.writeFile(dmapPath, renderSeededDmap(fileIndex, decisionsInFile, config), "utf8");
  }
}

function renderSeededDlog(
  source: string,
  fileIndex: number,
  decisionsInFile: number,
  config: SyntheticScaleConfig,
  content: string
): string {
  const lines = [`file: ${source}`, "schema: 1", "decisions:"];
  for (let slot = 0; slot < decisionsInFile; slot += 1) {
    const decisionIndex = fileIndex * config.decisionsPerFile + slot;
    const anchor = anchorName(decisionIndex, config);
    const [start, end] = syntheticFunctionLines(slot);
    lines.push(
      `  ${anchor}:`,
      `    id: ${decisionId(slot)}`,
      "    lines_hint:",
      `      - ${start}`,
      `      - ${end}`,
      `    fingerprint: ${yamlSingleQuoted(fingerprintSourceLines(content, start, end))}`,
      `    chose: ${yamlSingleQuoted(`seeded scale decision ${decisionIndex}`)}`,
      "    because: >-",
      "      seeded scale validation needs compatible decision artifacts without measuring one",
      "      process launch per decision",
      "    rejected:",
      "      - approach: per-decision CLI fixture setup",
      "        reason: >-",
      "          that would measure repeated process startup and full write rewrites instead of",
      "          large-repository read, lint, session, status, why, and reanchor behavior",
      "    timestamp: '2026-06-28T00:00:00.000Z'",
      "    history: []"
    );
  }
  return `${lines.join("\n")}\n`;
}

function renderSeededDmap(fileIndex: number, decisionsInFile: number, config: SyntheticScaleConfig): string {
  const lines: string[] = [];
  for (let slot = 0; slot < decisionsInFile; slot += 1) {
    const decisionIndex = fileIndex * config.decisionsPerFile + slot;
    const [start, end] = syntheticFunctionLines(slot);
    lines.push(`${start}-${end}:${anchorName(decisionIndex, config)}`);
  }
  return `${lines.join("\n")}\n`;
}

async function verifySeededScaleArtifacts(root: string, config: SyntheticScaleConfig): Promise<SeededSemanticSummary> {
  const summary: SeededSemanticSummary = {
    sampledFiles: 0,
    sampledDecisions: 0,
    shiftedFiles: 0,
    staleDecisions: 0,
    cleanDecisions: 0,
    lockArtifacts: 0,
    tempArtifacts: 0
  };

  for (const fileIndex of seededVerificationFileIndexes(config)) {
    const source = sourceFile(fileIndex);
    const dlogPath = path.join(root, `.decisions/${source}.dlog`);
    const dmapPath = path.join(root, `.decisions/${source}.dmap`);
    const dlog = await fs.readFile(dlogPath, "utf8");
    const dmap = await fs.readFile(dmapPath, "utf8");
    const dmapLines = dmap.trimEnd().length === 0 ? [] : dmap.trimEnd().split("\n");
    const decisionsInFile = Math.min(config.decisionsPerFile, config.decisions - fileIndex * config.decisionsPerFile);
    const mutated = fileIndex < config.mutateFiles;
    const expectedDmapLines: string[] = [];

    summary.sampledFiles += 1;
    if (mutated) {
      summary.shiftedFiles += 1;
    }

    for (let slot = 0; slot < decisionsInFile; slot += 1) {
      const decisionIndex = fileIndex * config.decisionsPerFile + slot;
      const anchor = anchorName(decisionIndex, config);
      const [originalStart, originalEnd] = syntheticFunctionLines(slot);
      const start = originalStart + (mutated ? 2 : 0);
      const end = originalEnd + (mutated ? 2 : 0);
      const stale = mutated && seededFunctionChangesWhenMutated(fileIndex, slot);
      const dmapLine = `${start}-${end}:${anchor}${stale ? ":STALE" : ""}`;
      const block = seededDecisionBlock(dlog, anchor);

      expectedDmapLines.push(dmapLine);
      assertIncludes(block, `    lines_hint:\n      - ${start}\n      - ${end}\n`, `${source} ${anchor} line hint`);
      if (stale) {
        assertIncludes(block, "    status: STALE\n", `${source} ${anchor} stale status`);
        assertIncludes(block, "    stale_since:", `${source} ${anchor} stale timestamp`);
        summary.staleDecisions += 1;
      } else {
        assertNotIncludes(block, "    status:", `${source} ${anchor} clean status`);
        assertNotIncludes(block, "    stale_since:", `${source} ${anchor} clean stale timestamp`);
        summary.cleanDecisions += 1;
      }
      summary.sampledDecisions += 1;
    }

    assertEqual(dmapLines.join("\n"), expectedDmapLines.join("\n"), `${source} dmap entries`);
  }

  const residue = countDecisionTreeResidue(path.join(root, ".decisions"));
  summary.lockArtifacts = residue.lockArtifacts;
  summary.tempArtifacts = residue.tempArtifacts;

  if (summary.lockArtifacts !== 0 || summary.tempArtifacts !== 0) {
    throw new Error(
      `Seeded verification found leftover lock/temp artifacts: locks=${summary.lockArtifacts} temp=${summary.tempArtifacts}`
    );
  }
  if (summary.sampledDecisions === 0) {
    throw new Error("Seeded verification sampled no decisions");
  }
  if (config.mutateFiles > 0 && summary.staleDecisions === 0) {
    throw new Error("Seeded verification found no sampled stale decisions");
  }
  if (summary.cleanDecisions === 0) {
    throw new Error("Seeded verification found no sampled clean decisions");
  }

  return summary;
}

function seededVerificationFileIndexes(config: SyntheticScaleConfig): number[] {
  const decisionFiles = Math.ceil(config.decisions / config.decisionsPerFile);
  const indexes = new Set<number>();
  const add = (index: number): void => {
    if (index >= 0 && index < decisionFiles) {
      indexes.add(index);
    }
  };

  add(0);
  add(Math.floor((decisionFiles - 1) / 2));
  add(decisionFiles - 1);
  if (config.mutateFiles > 0) {
    add(config.mutateFiles - 1);
  }
  if (config.mutateFiles < decisionFiles) {
    add(config.mutateFiles);
  }
  return [...indexes].sort((left, right) => left - right);
}

function seededDecisionBlock(dlog: string, anchor: string): string {
  const marker = `  ${anchor}:\n`;
  const start = dlog.indexOf(marker);
  if (start === -1) {
    throw new Error(`Seeded verification could not find ${anchor} in dlog`);
  }
  const next = dlog.indexOf("\n  fn:", start + marker.length);
  return dlog.slice(start, next === -1 ? dlog.length : next + 1);
}

function countDecisionTreeResidue(decisionRoot: string): { lockArtifacts: number; tempArtifacts: number } {
  const summary = { lockArtifacts: 0, tempArtifacts: 0 };
  const pending = [decisionRoot];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const entry of fsSync.readdirSync(current, { withFileTypes: true })) {
      const name = entry.name;
      if (entry.isDirectory()) {
        pending.push(path.join(current, name));
        continue;
      }
      if (name.endsWith(".lock")) {
        summary.lockArtifacts += 1;
      }
      if (name.includes(".archiva-tmp-") || name.endsWith(".tmp")) {
        summary.tempArtifacts += 1;
      }
    }
  }
  return summary;
}

function assertIncludes(value: string, expected: string, label: string): void {
  if (!value.includes(expected)) {
    throw new Error(`Scale verification failed for ${label}: missing ${JSON.stringify(expected)}`);
  }
}

function assertNotIncludes(value: string, unexpected: string, label: string): void {
  if (value.includes(unexpected)) {
    throw new Error(`Scale verification failed for ${label}: unexpected ${JSON.stringify(unexpected)}`);
  }
}

function assertEqual(actual: string, expected: string, label: string): void {
  if (actual !== expected) {
    throw new Error(
      `Scale verification failed for ${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`
    );
  }
}

function sortedRecord(record: Record<string, number>): Record<string, number> {
  const output: Record<string, number> = {};
  for (const key of Object.keys(record).sort()) {
    output[key] = record[key];
  }
  return output;
}

function decisionId(index: number): string {
  return `dec_${String(index + 1).padStart(3, "0")}`;
}

function fingerprintSourceLines(content: string, start: number, end: number): string {
  return hash256(normalizeCode(getLines(content, start, end))).slice(0, 8);
}

function normalizeCode(content: string): string {
  return splitJsLines(content)
    .map((line) => line.split(/\s+/).filter(Boolean).join(" "))
    .filter((line) => line.length > 0)
    .join("\n");
}

function getLines(content: string, start: number, end: number): string {
  return splitJsLines(content)
    .slice(start - 1, end)
    .join("\n");
}

function splitJsLines(content: string): string[] {
  const lines: string[] = [];
  let start = 0;
  for (let index = 0; index < content.length; index += 1) {
    if (content.charCodeAt(index) === 10) {
      let end = index;
      if (end > start && content.charCodeAt(end - 1) === 13) {
        end -= 1;
      }
      lines.push(content.slice(start, end));
      start = index + 1;
    }
  }
  lines.push(content.slice(start));
  return lines;
}

function yamlSingleQuoted(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

function checkedRun(
  runtime: Runtime,
  args: string[],
  input: string,
  cwd: string,
  expectedStatuses: number[],
  unavailableReason = "RSS measurement was not requested for this command"
): CommandResult {
	const result = spawnSync(runtime.command, [...runtime.prefixArgs, ...args], {
	  cwd,
	  input,
	  encoding: "utf8",
	  maxBuffer: commandMaxBuffer,
	  timeout: commandTimeoutMs,
	  killSignal: "SIGKILL",
	  env: { ...process.env, ARCHIVA_SESSION: "scale_smoke_session" }
	});
  const output = {
    status: result.status,
    stdout: normalizeVolatile(result.stdout ?? ""),
    stderr: normalizeVolatile(result.stderr ?? result.error?.message ?? ""),
    peakRss: { status: "unavailable", reason: unavailableReason } satisfies PeakRss
  };
	if (!expectedStatuses.includes(output.status ?? -1)) {
	  throwCommandError(runtime.name, args, output.status, result.signal, output.stdout, output.stderr, result.error);
	}
	return output;
}

function checkedRunMeasured(
  runtime: Runtime,
  args: string[],
  input: string,
  cwd: string,
  expectedStatuses: number[]
): CommandResult {
  if (rssTool.status === "unavailable") {
    return checkedRun(runtime, args, input, cwd, expectedStatuses, rssTool.reason);
  }
  const rssFile = path.join(os.tmpdir(), `archiva-rss-${process.pid}-${rssFileCounter++}.txt`);
  const measured = measuredCommand(rssTool.command, ["-f", "%M", "-o", rssFile, runtime.command, ...runtime.prefixArgs, ...args]);
  const result = spawnSync(measured.command, measured.args, {
    cwd,
    input,
    encoding: "utf8",
    maxBuffer: commandMaxBuffer,
    timeout: measured.nodeTimeoutMs,
    killSignal: "SIGKILL",
    env: { ...process.env, ARCHIVA_SESSION: "scale_smoke_session" }
  });
  const peakRss = readPeakRss(rssFile);
  removeFileIfExists(rssFile);
  const output = {
    status: result.status,
    stdout: normalizeVolatile(result.stdout ?? ""),
    stderr: normalizeVolatile(result.stderr ?? result.error?.message ?? ""),
    peakRss
  };
	if (!expectedStatuses.includes(output.status ?? -1)) {
	  throwCommandError(runtime.name, args, output.status, result.signal, output.stdout, output.stderr, result.error);
	}
	return output;
}

function throwCommandError(
  command: string,
  args: string[],
  status: number | null,
  signal: NodeJS.Signals | null,
  stdout: string,
  stderr: string,
  error: Error | undefined
): never {
  if (isTimeoutError(error) || status === 124) {
	  throw new Error(
	    `${command} ${args.join(" ")} timed out after ${commandTimeoutMs}ms: ${formatFailure(status, signal, stdout, stderr, error)}`
	  );
	}
  throw new Error(`${command} ${args.join(" ")} failed: ${formatFailure(status, signal, stdout, stderr, error)}`);
}

function isTimeoutError(error: Error | undefined): boolean {
  const code = (error as NodeJS.ErrnoException | undefined)?.code;
  return code === "ETIMEDOUT" || error?.message.includes("ETIMEDOUT") === true;
}

function measuredCommand(command: string, args: string[]): { command: string; args: string[]; nodeTimeoutMs: number } {
  if (processTimeoutTool) {
    return {
      command: processTimeoutTool,
      args: ["--kill-after=5s", `${commandTimeoutMs / 1000}s`, command, ...args],
      nodeTimeoutMs: commandTimeoutMs + 6_000
    };
  }
  return { command, args, nodeTimeoutMs: commandTimeoutMs };
}

function detectProcessTimeoutTool(): string | undefined {
  if (process.platform !== "linux") {
    return undefined;
  }
  const command = "/usr/bin/timeout";
  return fsSync.existsSync(command) ? command : undefined;
}

function formatFailure(
  status: number | null,
  signal: NodeJS.Signals | null,
  stdout: string,
  stderr: string,
  error: Error | undefined
): string {
  const code = (error as NodeJS.ErrnoException | undefined)?.code;
  return [
    `status=${status}`,
    `signal=${signal ?? "none"}`,
    code ? `errorCode=${code}` : undefined,
    error?.message ? `error=${error.message}` : undefined,
    stderr ? `stderr=${tail(stderr)}` : undefined,
    stdout ? `stdout=${tail(stdout)}` : undefined
  ]
    .filter((part): part is string => part !== undefined)
    .join(" ");
}

function tail(value: string): string {
  const normalized = value.trim();
  return JSON.stringify(normalized.length > 2000 ? normalized.slice(-2000) : normalized);
}

async function selectCorpus(
  corpusRoot: string,
  config: ScaleConfig,
  validatorRuntime: Runtime
): Promise<CorpusSelection> {
  const rootStat = await fs.stat(corpusRoot).catch((error) => {
    throw new Error(`ARCHIVA_SCALE_CORPUS_ROOT is not readable: ${String(error)}`);
  });
  if (!rootStat.isDirectory()) {
    throw new Error(`ARCHIVA_SCALE_CORPUS_ROOT must be a directory: ${corpusRoot}`);
  }
  const files = await listCorpusFiles(corpusRoot);
  const language = selectCorpusLanguage(files);
  const validatorRoot = await tempProject(validatorRuntime.name, "corpus-anchor-select");
  const selected: CorpusDecision[] = [];
  let scannedFiles = 0;
  for (const file of files) {
    if (selected.length >= config.files) {
      break;
    }
    if (sourceLanguage(file) !== language) {
      continue;
    }
    scannedFiles += 1;
    const absolute = path.join(corpusRoot, file);
    const stat = await fs.stat(absolute);
    if (!stat.isFile() || stat.size > corpusMaxFileBytes) {
      continue;
    }
    const content = await fs.readFile(absolute, "utf8").catch(() => undefined);
    if (content === undefined || content.includes("\0")) {
      continue;
    }
    const anchor = await firstAcceptedCorpusAnchor(
      validatorRuntime,
      validatorRoot,
      corpusRoot,
      file,
      findCorpusAnchors(file, content, language)
    );
    if (anchor) {
      selected.push({ file, anchor: anchor.anchor, lines: anchor.lines });
    }
  }
  if (selected.length < config.decisions) {
    throw new Error(
      `Corpus only provided ${selected.length} usable anchored ${language} source files; need at least ${config.decisions}. ` +
        "Set ARCHIVA_SCALE_CORPUS_ROOT to a larger repository, set ARCHIVA_SCALE_CORPUS_LANGUAGE, or lower ARCHIVA_SCALE_CORPUS_DECISIONS."
    );
  }
  return { corpusRoot, language, scannedFiles, selected };
}

async function firstAcceptedCorpusAnchor(
  runtime: Runtime,
  validationRoot: string,
  corpusRoot: string,
  file: string,
  candidates: CorpusAnchorCandidate[]
): Promise<CorpusAnchorCandidate | undefined> {
  if (candidates.length === 0) {
    return undefined;
  }
  const absoluteTarget = path.join(validationRoot, file);
  await fs.mkdir(path.dirname(absoluteTarget), { recursive: true });
  await fs.copyFile(path.join(corpusRoot, file), absoluteTarget);
  for (const candidate of candidates) {
    const result = spawnSync(
      runtime.command,
      [...runtime.prefixArgs, "write-decision", "--json", JSON.stringify(corpusDecisionInput({ file, ...candidate }, 0))],
      {
        cwd: validationRoot,
        encoding: "utf8",
        maxBuffer: commandMaxBuffer,
        timeout: commandTimeoutMs,
        killSignal: "SIGKILL",
        env: { ...process.env, ARCHIVA_SESSION: "scale_smoke_session" }
      }
    );
    if (result.status === 0) {
      return candidate;
    }
  }
  return undefined;
}

async function summarizeArtifacts(root: string): Promise<ArtifactSummary> {
  const files = await listFiles(root);
  let decisionBytes = 0;
  for (const file of files) {
    if (file.startsWith(".decisions/")) {
      decisionBytes += (await fs.stat(path.join(root, file))).size;
    }
  }
  return {
    dlogFiles: files.filter((file) => file.endsWith(".dlog")).length,
    dmapFiles: files.filter((file) => file.endsWith(".dmap")).length,
    sourceFiles: files.filter((file) => isSourceFile(file)).length,
    decisionBytes
  };
}

async function readParityArtifacts(root: string, config: SyntheticScaleConfig): Promise<Record<string, string>> {
  const output: Record<string, string> = {};
  for (const source of syntheticDecisionSources(config)) {
    for (const file of decisionFilesForSource(source)) {
      output[file] = normalizeVolatile(await fs.readFile(path.join(root, file), "utf8"));
    }
  }
  output["__status__"] = JSON.stringify(await summarizeArtifacts(root));
  return output;
}

async function readDecisionArtifacts(root: string, decisions: CorpusDecision[]): Promise<Record<string, string>> {
  const output: Record<string, string> = {};
  for (const decision of decisions) {
    for (const file of decisionFilesForSource(decision.file)) {
      output[file] = normalizeVolatile(await fs.readFile(path.join(root, file), "utf8"));
    }
  }
  output["__status__"] = JSON.stringify(await summarizeArtifacts(root));
  return output;
}

function decisionFilesForSource(source: string): string[] {
  return [`.decisions/${source}.dlog`, `.decisions/${source}.dmap`];
}

function summarize(result: CommandResult): CommandSummary {
  return {
    status: result.status,
    stdoutBytes: Buffer.byteLength(result.stdout),
    stderrBytes: Buffer.byteLength(result.stderr),
    stdoutHash: hash(result.stdout),
    stderrHash: hash(result.stderr),
    peakRss: result.peakRss
  };
}

function recordPhaseRss(phaseRss: Map<string, PeakRss>, phase: string, peakRss: PeakRss): void {
  const previous = phaseRss.get(phase);
  if (peakRss.status === "measured") {
    if (previous?.status !== "measured" || peakRss.peakRssKb > previous.peakRssKb) {
      phaseRss.set(phase, peakRss);
    }
    return;
  }
  if (!previous) {
    phaseRss.set(phase, peakRss);
  }
}

function detectRssTool(): { status: "available"; command: string } | { status: "unavailable"; reason: string } {
  if (process.platform !== "linux") {
    return { status: "unavailable", reason: "RSS measurement is currently implemented for Linux only" };
  }
  const command = "/usr/bin/time";
  if (!fsSync.existsSync(command)) {
    return { status: "unavailable", reason: "/usr/bin/time is not available" };
  }
  const rssFile = path.join(os.tmpdir(), `archiva-rss-probe-${process.pid}-${rssFileCounter++}.txt`);
  const result = spawnSync(command, ["-f", "%M", "-o", rssFile, "true"], {
    encoding: "utf8",
    timeout: commandTimeoutMs,
    killSignal: "SIGKILL"
  });
  const peakRss = readPeakRss(rssFile);
  removeFileIfExists(rssFile);
  if (result.status !== 0) {
    return { status: "unavailable", reason: `/usr/bin/time probe failed: ${result.stderr || result.stdout}`.trim() };
  }
  if (peakRss.status === "unavailable") {
    return peakRss;
  }
  return { status: "available", command };
}

function readPeakRss(rssFile: string): PeakRss {
  try {
    const content = fsSync.readFileSync(rssFile, "utf8").trim();
    const numericLine = content
      .split(/\r?\n/)
      .map((line) => line.trim())
      .reverse()
      .find((line) => /^\d+$/.test(line));
    const value = numericLine === undefined ? Number.NaN : Number(numericLine);
    if (!Number.isFinite(value) || value <= 0) {
      return { status: "unavailable", reason: `unparsable RSS output: ${JSON.stringify(content)}` };
    }
    return { status: "measured", peakRssKb: value };
  } catch (error) {
    return { status: "unavailable", reason: `unable to read RSS output: ${String(error)}` };
  }
}

function removeFileIfExists(file: string): void {
  try {
    fsSync.unlinkSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }
}

function normalizeConfig(config: SyntheticScaleConfig): SyntheticScaleConfig {
  const capacity = config.files * config.decisionsPerFile;
  if (config.decisions > capacity) {
    throw new Error(
      `decisions (${config.decisions}) must be <= files * decisionsPerFile (${capacity})`
    );
  }
  const decisionFiles = Math.ceil(config.decisions / config.decisionsPerFile);
  if (config.mutateFiles > decisionFiles) {
    throw new Error(
      `mutateFiles (${config.mutateFiles}) must be <= files containing decisions (${decisionFiles})`
    );
  }
  return config;
}

function normalizeCorpusConfig(config: ScaleConfig): ScaleConfig {
  if (config.decisions > config.files) {
    throw new Error(`decisions (${config.decisions}) must be <= files (${config.files})`);
  }
  if (config.mutateFiles > config.decisions) {
    throw new Error(`mutateFiles (${config.mutateFiles}) must be <= decisions (${config.decisions})`);
  }
  return config;
}

function positiveIntEnv(name: string, fallback: number): number {
  const value = process.env[name];
  if (!value) {
    return fallback;
  }
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function corpusLanguageEnv(name: string): CorpusLanguage | "auto" {
  const value = process.env[name]?.trim().toLowerCase();
  if (!value || value === "auto") {
    return "auto";
  }
  if (value === "typescript" || value === "rust" || value === "c/cpp") {
    return value;
  }
  throw new Error(`${name} must be auto, typescript, rust, or c/cpp`);
}

async function listFiles(root: string): Promise<string[]> {
  const output: string[] = [];
  async function walk(relative: string): Promise<void> {
    const entries = await fs.readdir(path.join(root, relative), { withFileTypes: true });
    for (const entry of entries) {
      const child = relative.length === 0 ? entry.name : `${relative}/${entry.name}`;
      if (entry.isDirectory()) {
        if (entry.name === ".git") {
          continue;
        }
        await walk(child);
      } else {
        output.push(child);
      }
    }
  }
  await walk("");
  output.sort();
  return output;
}

async function listCorpusFiles(root: string): Promise<string[]> {
  const output: string[] = [];
  const ignoredDirs = new Set([
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".turbo"
  ]);
  async function walk(relative: string): Promise<void> {
    const entries = await fs.readdir(path.join(root, relative), { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const child = relative.length === 0 ? entry.name : `${relative}/${entry.name}`;
      if (entry.isDirectory()) {
        if (ignoredDirs.has(entry.name)) {
          continue;
        }
        await walk(child);
      } else if (entry.isFile()) {
        output.push(child);
      }
    }
  }
  await walk("");
  output.sort();
  return output;
}

function findCorpusAnchors(
  file: string,
  content: string,
  language = sourceLanguage(file)
): CorpusAnchorCandidate[] {
  if (language === "rust") {
    return findRustCorpusAnchors(file, content);
  }
  if (language === "c/cpp") {
    return findCxxCorpusAnchors(file, content);
  }
  if (language !== "typescript") {
    return [];
  }
  return findTypeScriptCorpusAnchors(content);
}

function findTypeScriptCorpusAnchors(content: string): CorpusAnchorCandidate[] {
  const lines = content.split(/\r?\n/);
  const candidates: CorpusAnchorCandidate[] = [];
  let depth = 0;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (depth === 0) {
      const exported = line.match(
        /^\s*export\s+(?:declare\s+)?(?:(?:const\s+)?enum|namespace|module|interface|type|(?:abstract\s+)?class|(?:async\s+)?function|const|let|var)\s+([A-Za-z_$][\w$]*)\b/
      );
      if (exported) {
        candidates.push({ anchor: `export:${exported[1]}`, lines: [index + 1, index + 1] });
      }
      const fn = line.match(/^\s*(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*(?:<[^(){};=]*)?\(/);
      if (fn) {
        candidates.push({ anchor: `fn:${fn[1]}`, lines: [index + 1, index + 1] });
      }
      const cls = line.match(/^\s*(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)\b/);
      if (cls) {
        candidates.push({ anchor: `class:${cls[1]}`, lines: [index + 1, index + 1] });
      }
    }
    depth = updateBraceDepth(depth, line);
  }
  return uniqueCorpusCandidates(candidates);
}

function findRustCorpusAnchors(file: string, content: string): CorpusAnchorCandidate[] {
  const lines = content.split(/\r?\n/);
  const methodCandidates: CorpusAnchorCandidate[] = [];
  const structuralCandidates: CorpusAnchorCandidate[] = [];
  const functionCandidates: CorpusAnchorCandidate[] = [];
  const implCandidates: CorpusAnchorCandidate[] = [];
  let depth = 0;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (depth === 0) {
      const structuralAnchor = rustStructuralCorpusAnchor(line);
      if (structuralAnchor) {
        structuralCandidates.push({ anchor: structuralAnchor, lines: [index + 1, index + 1] });
      }
      const fn = line.match(
        /^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:async|const|unsafe)\s+)*(?:extern\s+(?:"[^"]+"\s+)?)?fn\s+((?:r#)?[A-Za-z_][A-Za-z0-9_]*)\b/
      );
      if (fn) {
        functionCandidates.push({ anchor: `fn:${rustAnchorName(fn[1])}`, lines: [index + 1, index + 1] });
      }
      const implAnchor = rustImplCorpusAnchor(line);
      if (implAnchor) {
        implCandidates.push({ anchor: implAnchor, lines: [index + 1, index + 1] });
        const methodAnchor = findRustImplMethodAnchor(lines, index, implAnchor);
        if (methodAnchor) {
          methodCandidates.push(methodAnchor);
        }
      }
    }
    depth = updateBraceDepth(depth, line);
  }
  return orderedCorpusCandidates(file, [methodCandidates, structuralCandidates, functionCandidates, implCandidates]);
}

function findCxxCorpusAnchors(file: string, content: string): CorpusAnchorCandidate[] {
  const lines = content.split(/\r?\n/);
  const methodCandidates: CorpusAnchorCandidate[] = [];
  const structuralCandidates: CorpusAnchorCandidate[] = [];
  const functionCandidates: CorpusAnchorCandidate[] = [];
  const typeStack: { name: string; depth: number }[] = [];
  let depth = 0;
  for (let index = 0; index < lines.length; index += 1) {
    const line = stripCxxLineComment(lines[index]);
    const structural = line.match(/^\s*(?:template\s*<[^>{;]*>\s*)?(class|struct|enum)(?:\s+(?:class|struct))?\s+([A-Za-z_][A-Za-z0-9_]*)\b.*\{/);
    if (structural) {
      const anchor = `${structural[1]}:${structural[2]}`;
      structuralCandidates.push({ anchor, lines: [index + 1, index + 1] });
      if (structural[1] === "class" || structural[1] === "struct") {
        typeStack.push({ name: structural[2], depth: depth + 1 });
      }
    }
    const header = cxxDeclarationHeader(lines, index);
    if (header) {
      const qualified = header.text.match(/^\s*(?:template\s*<[^>{;]*>\s*)?(?:[A-Za-z_][\w:<>,~*&\s]*\s+)?([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_~][A-Za-z0-9_~]*)+)\s*\([^;]*\)\s*(?:const\s*)?(?:noexcept\s*)?(?:override\s*)?(?:final\s*)?\{/);
      if (qualified) {
        const anchor = `fn:${qualified[1].replace(/::/g, ".")}`;
        methodCandidates.push({ anchor, lines: [index + 1, header.endLine] });
      } else {
        const fn = header.text.match(/^\s*(?:template\s*<[^>{;]*>\s*)?(?:(?:static|inline|extern|virtual|constexpr|consteval|constinit|friend|explicit)\s+)*(?:[A-Za-z_][\w:<>,~*&\s]*\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\([^;]*\)\s*(?:const\s*)?(?:noexcept\s*)?(?:override\s*)?(?:final\s*)?\{/);
        if (fn && !cxxControlKeyword(fn[1])) {
          const currentType = [...typeStack].reverse().find((scope) => scope.depth <= depth);
          const anchor = currentType ? `fn:${currentType.name}.${fn[1]}` : `fn:${fn[1]}`;
          (currentType ? methodCandidates : functionCandidates).push({ anchor, lines: [index + 1, header.endLine] });
        }
      }
    }
    depth = updateBraceDepth(depth, line);
    while (typeStack.length > 0 && typeStack[typeStack.length - 1].depth > depth) {
      typeStack.pop();
    }
  }
  return orderedCorpusCandidates(file, [methodCandidates, structuralCandidates, functionCandidates]);
}

function orderedCorpusCandidates(file: string, buckets: CorpusAnchorCandidate[][]): CorpusAnchorCandidate[] {
  const nonEmpty = buckets.filter((bucket) => bucket.length > 0);
  if (nonEmpty.length === 0) {
    return [];
  }
  const start = stableBucket(file) % nonEmpty.length;
  const ordered: CorpusAnchorCandidate[] = [];
  for (let offset = 0; offset < nonEmpty.length; offset += 1) {
    ordered.push(...nonEmpty[(start + offset) % nonEmpty.length]);
  }
  return uniqueCorpusCandidates(ordered);
}

function uniqueCorpusCandidates(candidates: CorpusAnchorCandidate[]): CorpusAnchorCandidate[] {
  const seen = new Set<string>();
  const output: CorpusAnchorCandidate[] = [];
  for (const candidate of candidates) {
    const key = `${candidate.anchor}\0${candidate.lines[0]}\0${candidate.lines[1]}`;
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    output.push(candidate);
  }
  return output;
}

function cxxDeclarationHeader(lines: string[], start: number): { text: string; endLine: number } | undefined {
  const first = stripCxxLineComment(lines[start]);
  if (first.trimStart().startsWith("#")) {
    return undefined;
  }
  let header = "";
  for (let index = start; index < Math.min(lines.length, start + 8); index += 1) {
    const line = stripCxxLineComment(lines[index]).trim();
    if (line === "" || line.startsWith("#")) {
      continue;
    }
    header = `${header} ${line}`.trim();
    const blockIndex = header.indexOf("{");
    const semicolonIndex = header.indexOf(";");
    if (blockIndex !== -1 && (semicolonIndex === -1 || blockIndex < semicolonIndex)) {
      return { text: header, endLine: index + 1 };
    }
    if (semicolonIndex !== -1) {
      return undefined;
    }
  }
  return undefined;
}

function stripCxxLineComment(line: string): string {
  const comment = line.indexOf("//");
  return comment === -1 ? line : line.slice(0, comment);
}

function cxxControlKeyword(value: string): boolean {
  return ["if", "for", "while", "switch", "catch", "sizeof", "return"].includes(value);
}

function rustStructuralCorpusAnchor(line: string): string | undefined {
  const match = line.match(
    /^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(struct|enum|trait|mod)\s+((?:r#)?[A-Za-z_][A-Za-z0-9_]*)\b/
  );
  if (!match) {
    return undefined;
  }
  return `${match[1]}:${rustAnchorName(match[2])}`;
}

function stableBucket(value: string): number {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = (hash * 31 + value.charCodeAt(index)) >>> 0;
  }
  return hash;
}

function findRustImplMethodAnchor(
  lines: string[],
  implLineIndex: number,
  implAnchor: string
): { anchor: string; lines: [number, number] } | undefined {
  let depth = updateBraceDepth(0, lines[implLineIndex]);
  if (depth === 0) {
    return undefined;
  }
  const methodPrefix = implAnchor.slice("impl:".length);
  for (let index = implLineIndex + 1; index < lines.length; index += 1) {
    if (depth === 1) {
      const method = lines[index].match(
        /^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:async|const|unsafe)\s+)*(?:extern\s+(?:"[^"]+"\s+)?)?fn\s+((?:r#)?[A-Za-z_][A-Za-z0-9_]*)\b/
      );
      if (method) {
        return {
          anchor: `fn:${methodPrefix}.${rustAnchorName(method[1])}`,
          lines: [index + 1, index + 1]
        };
      }
    }
    depth = updateBraceDepth(depth, lines[index]);
    if (depth === 0) {
      return undefined;
    }
  }
  return undefined;
}

function rustImplCorpusAnchor(line: string): string | undefined {
  const match = line.match(/^\s*(?:unsafe\s+)?impl\b(.*)$/);
  if (!match) {
    return undefined;
  }
  let header = match[1].replace(/\{.*$/, "").replace(/\bwhere\b.*$/, "").trim();
  if (header.startsWith("<")) {
    const end = matchingAngleEnd(header, 0);
    if (end === undefined) {
      return undefined;
    }
    header = header.slice(end + 1).trim();
  }
  header = header.replace(/^(?:unsafe|const)\s+/, "").trim();
  const forMatch = header.match(/^(.*?)\s+for\s+(.*)$/);
  if (forMatch) {
    const traitName = rustPathTerminal(forMatch[1]);
    const typeName = rustPathTerminal(forMatch[2]);
    return traitName && typeName ? `impl:${typeName}.${traitName}` : undefined;
  }
  const typeName = rustPathTerminal(header);
  return typeName ? `impl:${typeName}` : undefined;
}

function matchingAngleEnd(value: string, start: number): number | undefined {
  let depth = 0;
  for (let index = start; index < value.length; index += 1) {
    const char = value[index];
    if (char === "<") {
      depth += 1;
    } else if (char === ">") {
      depth -= 1;
      if (depth === 0) {
        return index;
      }
    }
  }
  return undefined;
}

function rustPathTerminal(value: string): string | undefined {
  const withoutGenerics = stripAngleGroups(value);
  const matches = Array.from(withoutGenerics.matchAll(/(?:r#)?[A-Za-z_][A-Za-z0-9_]*/g))
    .map((match) => match[0])
    .filter((name) => name !== "dyn" && name !== "r#dyn");
  const terminal = matches.at(-1);
  return terminal === undefined ? undefined : rustAnchorName(terminal);
}

function rustAnchorName(name: string): string {
  return name.startsWith("r#") ? name.slice(2) : name;
}

function stripAngleGroups(value: string): string {
  let output = "";
  let depth = 0;
  for (let index = 0; index < value.length; index += 1) {
    const char = value[index];
    if (char === "<") {
      depth += 1;
      continue;
    }
    if (char === ">" && depth > 0) {
      depth -= 1;
      continue;
    }
    if (depth === 0) {
      output += char;
    }
  }
  return output;
}

function updateBraceDepth(depth: number, line: string): number {
  let next = depth;
  for (let index = 0; index < line.length; index += 1) {
    if (line[index] === "{") {
      next += 1;
    } else if (line[index] === "}") {
      next = Math.max(0, next - 1);
    }
  }
  return next;
}

async function mutateCorpusFile(root: string, entry: CorpusDecision, index: number): Promise<void> {
  const file = path.join(root, entry.file);
  const content = await fs.readFile(file, "utf8");
  await fs.writeFile(file, insertMutationComment(content, index, sourceLanguage(entry.file)), "utf8");
}

function insertMutationComment(content: string, index: number, language?: CorpusLanguage): string {
  const comment = `// archiva corpus mutation ${index}\n`;
  if (language === "rust") {
    const prefixEnd = rustMutationPrefixEnd(content);
    return content.slice(0, prefixEnd) + comment + content.slice(prefixEnd);
  }
  if (!content.startsWith("#!")) {
    return comment + content;
  }
  const newline = content.indexOf("\n");
  if (newline === -1) {
    return `${content}\n${comment}`;
  }
  return content.slice(0, newline + 1) + comment + content.slice(newline + 1);
}

function rustMutationPrefixEnd(content: string): number {
  let offset = 0;
  let consumedPrefix = false;
  while (offset < content.length) {
    const newline = content.indexOf("\n", offset);
    const lineEnd = newline === -1 ? content.length : newline + 1;
    const line = content.slice(offset, lineEnd);
    const trimmed = line.trimStart();
    if (offset === 0 && trimmed.startsWith("#!") && !trimmed.startsWith("#![")) {
      offset = lineEnd;
      consumedPrefix = true;
      continue;
    }
    if (trimmed.startsWith("//!") || trimmed.startsWith("#![")) {
      offset = lineEnd;
      consumedPrefix = true;
      continue;
    }
    if (trimmed.startsWith("/*!")) {
      const close = content.indexOf("*/", offset + line.indexOf("/*!") + 3);
      offset = close === -1 ? lineEnd : close + 2 + trailingNewlineLength(content, close + 2);
      consumedPrefix = true;
      continue;
    }
    if (consumedPrefix && trimmed.trim() === "") {
      offset = lineEnd;
      continue;
    }
    break;
  }
  return offset;
}

function trailingNewlineLength(content: string, offset: number): number {
  if (content[offset] === "\r" && content[offset + 1] === "\n") {
    return 2;
  }
  return content[offset] === "\n" ? 1 : 0;
}

function isSourceFile(file: string): boolean {
  return sourceLanguage(file) !== undefined;
}

function sourceLanguage(file: string): CorpusLanguage | undefined {
  const lower = file.toLowerCase();
  if (lower.endsWith(".d.ts")) {
    return undefined;
  }
  if (
    lower.endsWith(".ts") ||
    lower.endsWith(".tsx") ||
    lower.endsWith(".js") ||
    lower.endsWith(".jsx") ||
    lower.endsWith(".mjs") ||
    lower.endsWith(".cjs")
  ) {
    return "typescript";
  }
  if (lower.endsWith(".rs")) {
    return "rust";
  }
  if (
    lower.endsWith(".c") ||
    lower.endsWith(".h") ||
    lower.endsWith(".cc") ||
    lower.endsWith(".cpp") ||
    lower.endsWith(".cxx") ||
    lower.endsWith(".hh") ||
    lower.endsWith(".hpp") ||
    lower.endsWith(".hxx") ||
    lower.endsWith(".ipp") ||
    lower.endsWith(".inc")
  ) {
    return "c/cpp";
  }
  return undefined;
}

function selectCorpusLanguage(files: string[]): CorpusLanguage {
  if (corpusLanguage !== "auto") {
    return corpusLanguage;
  }
  let typeScriptFiles = 0;
  let rustFiles = 0;
  let cxxFiles = 0;
  for (const file of files) {
    const language = sourceLanguage(file);
    if (language === "typescript") {
      typeScriptFiles += 1;
    } else if (language === "rust") {
      rustFiles += 1;
    } else if (language === "c/cpp") {
      cxxFiles += 1;
    }
  }
  if (cxxFiles > rustFiles && cxxFiles > typeScriptFiles) {
    return "c/cpp";
  }
  return rustFiles > typeScriptFiles ? "rust" : "typescript";
}

function git(cwd: string, args: string[]): void {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", timeout: commandTimeoutMs, killSignal: "SIGKILL" });
  if (result.status !== 0) {
    throwCommandError("git", args, result.status, result.signal, result.stdout ?? "", result.stderr ?? "", result.error);
  }
}

async function tempProject(runtime: string, name: string): Promise<string> {
  return fs.mkdtemp(path.join(os.tmpdir(), `archiva-${name}-${runtime}-`));
}

function normalizeVolatile(value: string): string {
  return value.replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/g, "<timestamp>");
}

function hash(value: string): string {
  return hash256(value).slice(0, 16);
}

function hash256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}
