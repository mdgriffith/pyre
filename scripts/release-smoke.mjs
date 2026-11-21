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

const localPackages = {
  "@pyre/core": `file:${coreTarball}`,
  "@pyre/server": `file:${serverTarball}`,
  "@pyre/client": `file:${clientTarball}`,
};

writeFileSync(
  join(smokeDir, "package.json"),
  JSON.stringify(
    {
      name: "pyre-smoke-install",
      private: true,
      type: "module",
      dependencies: {
        ...localPackages,
        "@libsql/client": "^0.14.0",
        zod: "^4.1.12",
      },
      // Transitive dependencies must also use the unpublished local tarballs.
      overrides: localPackages,
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
  `import assert from 'node:assert/strict';
import * as core from '@pyre/core';
import { initDisposableDatabase, seed } from '@pyre/server';
import * as client from '@pyre/client';
import { init } from '@pyre/server/wasm';
import { createClient } from '@libsql/client';

assert.ok(core && client, 'Failed to import @pyre/core or @pyre/client');
await init();

const db = createClient({ url: 'file:smoke.db' });
try {
  await initDisposableDatabase(db, 'Smoke', \`@syncable(false)
record Note {
    id Id.Int @id
    body String
    @public
}\n\`);
  const result = await seed(db, { notes: [{ body: 'packed fixture' }] }, { namespace: 'Smoke' });
  assert.equal(result.kind, 'success', JSON.stringify(result));
  assert.equal(result.response.notes[0].body, 'packed fixture');
  assert.equal((await db.execute('select body from notes')).rows[0].body, 'packed fixture');
} finally {
  db.close();
}
`,
);

const verify = Bun.spawnSync(["bun", "run", "verify.mjs"], {
  cwd: smokeDir,
  stdout: "inherit",
  stderr: "inherit",
});

if (verify.exitCode !== 0) {
  throw new Error("Installed package verification failed");
}

console.log("Smoke install passed in .artifacts/smoke-install");
