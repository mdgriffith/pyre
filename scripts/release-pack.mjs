import { mkdirSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const rootDir = process.cwd();
const artifactsDir = join(rootDir, ".artifacts", "tarballs");
const packageDirs = ["core", "server", "client"];

rmSync(artifactsDir, { recursive: true, force: true });
mkdirSync(artifactsDir, { recursive: true });

const clientBuild = Bun.spawnSync(
  ["bun", "run", "--cwd", "packages/client", "build"],
  { cwd: rootDir, stdout: "inherit", stderr: "inherit" }
);

if (clientBuild.exitCode !== 0) {
  throw new Error("Failed to build @pyre/client before packing");
}

for (const pkg of packageDirs) {
  const cwd = join(rootDir, "packages", pkg);
  const packed = Bun.spawnSync(
    ["bun", "pm", "pack", "--destination", artifactsDir],
    { cwd, stdout: "pipe", stderr: "pipe" }
  );

  if (packed.exitCode !== 0) {
    throw new Error(`Failed to pack ${pkg}: ${new TextDecoder().decode(packed.stderr)}`);
  }
}

const tarballs = readdirSync(artifactsDir)
  .filter((name) => name.endsWith(".tgz"))
  .sort();

if (tarballs.length === 0) {
  throw new Error("No tarballs produced in .artifacts/tarballs");
}

console.log("Packed tarballs:");
for (const tarball of tarballs) {
  console.log(`- .artifacts/tarballs/${tarball}`);
}
