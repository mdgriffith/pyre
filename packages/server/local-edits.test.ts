import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

// Keep the production WASM boundary isolated from the query-sync unit mocks.
test("generated local edits execute against production SQLite", () => {
  const cwd = fileURLToPath(new URL("../..", import.meta.url));
  for (const args of [
    ["packages/server/fixtures/local-edits/generate.ts"],
    ["test", "./packages/server/fixtures/local-edits/check.ts"],
  ]) {
    const result = spawnSync(process.execPath, args, { cwd, encoding: "utf8" });
    expect(result.error).toBeUndefined();
    expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(0);
  }
}, 30_000);
