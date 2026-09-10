// Run from the repository root after building the compiler: bun packages/server/fixtures/compiled-batch/regenerate.ts
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const fixture = dirname(fileURLToPath(import.meta.url));
const temporary = mkdtempSync(join(tmpdir(), "pyre-compiled-batch-"));
try {
  const output = join(temporary, "output");
  const compiler = process.env.PYRE_COMPILER ?? resolve("target/debug/pyre");
  const result = spawnSync(compiler, ["--in", fixture, "generate", "--out", output], { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error("Compiler fixture generation failed");
  const server = join(output, "typescript/server.ts");
  const bundled = await Bun.build({ entrypoints: [server], target: "bun", external: ["zod", "@pyre/*"] });
  if (!bundled.success) throw new AggregateError(bundled.logs, "Generated server metadata must compile");
  copyFileSync(join(output, "typescript/databases.ts"), join(fixture, "generated/databases.ts"));
  for (const path of ["decode.ts", "queries/metadata/entryCreate.ts", "queries/sql/entryCreate.ts", "queries/metadata/entriesForContext.ts", "queries/sql/entriesForContext.ts", "queries/sql/types.ts"]) {
    const destination = join(fixture, "generated", path);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(join(output, "typescript/core", path), destination);
  }
  const generatedServer = readFileSync(server, "utf8");
  const fingerprint = generatedServer.match(/^export const manifestVersion = "sha256:[a-f0-9]{64}";$/m)?.[0];
  if (!fingerprint) throw new Error("Missing valid compiler manifest fingerprint export");
  const contract = generatedServer.match(/^export const compiledContract = "[a-f0-9]{64}";$/m)?.[0];
  if (!contract || !generatedServer.includes("export const manifest: BatchManifest = { version: 1, manifestVersion, compiledContract, queries, SessionValidator };"))
    throw new Error("Missing compiled contract in generated BatchManifest");
  writeFileSync(join(fixture, "generated/manifest.ts"), `${fingerprint}\n${contract}\n`);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
