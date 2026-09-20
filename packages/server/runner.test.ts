import { expect, test } from "bun:test";
import { existsSync } from "node:fs";

test.skipIf(!existsSync(new URL("./wasm/pyre_wasm_bg.wasm", import.meta.url)))("runners check persisted contracts for reads and writes and validate before commit", async () => {
  const child = Bun.spawn([process.execPath, new URL("./fixtures/runner-schema.ts", import.meta.url).pathname], {
    stdout: "pipe", stderr: "pipe",
  });
  const [stdout, stderr, status] = await Promise.all([
    new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
  ]);
  expect({ status, stdout, stderr }).toEqual({ status: 0, stdout: "", stderr: "" });
});
