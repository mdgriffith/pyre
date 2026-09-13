// Run from the repository root. All generated artifacts stay under target/.
import { mkdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("../../../../", import.meta.url));
mkdirSync(`${root}/target/local-edits-fixture`, { recursive: true });
const result = spawnSync(`${root}/target/debug/pyre`, ["--in", "packages/server/fixtures/local-edits", "generate",
  "--out", "target/local-edits-fixture"], { cwd: root, stdio: "inherit" });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error("Fixture generation failed");
