import { createClient } from "@libsql/client";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import initWasm from "../../wasm/pyre_wasm.js";
import { databases } from "../../../../target/local-edits-fixture/typescript/databases";

export async function openDatabase() {
  const directory = mkdtempSync(fileURLToPath(new URL("../../../../target/local-edits-", import.meta.url)));
  const database = createClient({ url: `file:${directory}/seed.db` });
  const close = () => { database.close(); rmSync(directory, { recursive: true, force: true }); };
  try {
    await initWasm({ module_or_path: readFileSync(new URL("../../wasm/pyre_wasm_bg.wasm", import.meta.url)) });
    await databases._default.ensureDatabase(database);
    return { database, close };
  } catch (error) { close(); throw error; }
}
