import { copyFileSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const rootDir = process.cwd();
const artifactsDir = join(rootDir, ".artifacts", "tarballs");
const packageDirs = ["core", "server", "client"];
const wasmDir = join(rootDir, "wasm");
const serverWasmDir = join(rootDir, "packages", "server", "wasm");

rmSync(artifactsDir, { recursive: true, force: true });
mkdirSync(artifactsDir, { recursive: true });

const wasmBuild = Bun.spawnSync(["wasm-pack", "build", "--target", "web"], {
  cwd: wasmDir,
  stdout: "inherit",
  stderr: "inherit",
});

if (wasmBuild.exitCode !== 0) {
  throw new Error("Failed to build the WASM runtime before packing");
}

rmSync(serverWasmDir, { recursive: true, force: true });
mkdirSync(serverWasmDir, { recursive: true });
for (const file of [
  "pyre_wasm.js",
  "pyre_wasm_bg.wasm",
  "pyre_wasm.d.ts",
  "pyre_wasm_bg.wasm.d.ts",
]) {
  copyFileSync(join(wasmDir, "pkg", file), join(serverWasmDir, file));
}

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
