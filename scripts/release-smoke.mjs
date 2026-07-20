import { mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const rootDir = process.cwd();
const tarballsDir = join(rootDir, ".artifacts", "tarballs");
const smokeDir = join(rootDir, ".artifacts", "smoke-install");

const pack = Bun.spawnSync(["bun", "run", "release:pack"], {
  cwd: rootDir,
  stdout: "inherit",
  stderr: "inherit",
});

if (pack.exitCode !== 0) {
  throw new Error("release:pack failed");
}

const tarballs = readdirSync(tarballsDir)
  .filter((name) => name.endsWith(".tgz"))
  .map((name) => join(tarballsDir, name))
  .sort();

if (tarballs.length === 0) {
  throw new Error("No tarballs available for smoke install");
}

const coreTarball = tarballs.find((path) => path.includes("pyre-core-"));
const serverTarball = tarballs.find((path) => path.includes("pyre-server-"));
const clientTarball = tarballs.find((path) => path.includes("pyre-client-"));

if (!coreTarball || !serverTarball || !clientTarball) {
  throw new Error("Missing one or more required tarballs (core/server/client)");
}

rmSync(smokeDir, { recursive: true, force: true });
mkdirSync(smokeDir, { recursive: true });

writeFileSync(
  join(smokeDir, "package.json"),
  JSON.stringify(
    {
      name: "pyre-smoke-install",
      private: true,
      type: "module",
      dependencies: {
        "@pyre/core": `file:${coreTarball}`,
        "@pyre/server": `file:${serverTarball}`,
        "@pyre/client": `file:${clientTarball}`,
      },
      overrides: {
        "@pyre/core": `file:${coreTarball}`,
      },
    },
    null,
    2
  )
);

const install = Bun.spawnSync(["bun", "install"], {
  cwd: smokeDir,
  stdout: "inherit",
  stderr: "inherit",
});

if (install.exitCode !== 0) {
  throw new Error("Failed to install packed tarballs with Bun in smoke project");
}

writeFileSync(
  join(smokeDir, "verify.mjs"),
  [
    "import * as core from '@pyre/core';",
    "import * as server from '@pyre/server';",
    "import * as client from '@pyre/client';",
    "import { init } from '@pyre/server/wasm';",
    "",
    "if (!core || !server || !client) {",
    "  throw new Error('Failed to import one or more @pyre packages');",
    "}",
    "",
    "await init();",
    "",
    "console.log('Smoke imports OK:', Object.keys({ core, server, client }).join(', '));",
  ].join("\n")
);

const verify = Bun.spawnSync(["bun", "run", "verify.mjs"], {
  cwd: smokeDir,
  stdout: "inherit",
  stderr: "inherit",
});

if (verify.exitCode !== 0) {
  throw new Error("Smoke import verification failed");
}

console.log("Smoke install passed in .artifacts/smoke-install");
