import { expect, test } from "bun:test";
import { existsSync } from "node:fs";

test.skipIf(!existsSync(new URL("./wasm/pyre_wasm_bg.wasm", import.meta.url)))("persisted schema authority fences stale real-libsql operations", async () => {
  const child = Bun.spawn([process.execPath, new URL("./fixtures/schema-authority.ts", import.meta.url).pathname], {
    stdout: "pipe", stderr: "pipe",
  });
  try {
    const [stdout, stderr, status] = await Promise.all([
      new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
    ]);
    expect({ status, stdout, stderr }).toEqual({ status: 0, stdout: "", stderr: "" });
  } finally { child.kill(); }
}, 30_000);
