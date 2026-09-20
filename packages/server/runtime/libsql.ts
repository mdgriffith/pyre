import type { Client } from "@libsql/client";

const supportedIntegerClients = new WeakSet<object>();
const INTEGER_MODE_PROBE = "select cast(1 as integer) as _pyre_integer_mode";

/** Local libsql detaches its connection for transactions, losing private memory databases. */
export async function assertPersistentTransaction(db: Client): Promise<void> {
  if (db.protocol !== "file") return;
  const databases = await db.execute("pragma database_list");
  const file = databases.rows.find(row => row.name === "main")?.file;
  if (typeof file !== "string" || !file) throw new Error("Unsupported in-memory transaction");
}

/** Pyre's generated codecs distinguish integer columns from numeric-looking text. */
export async function assertSupportedIntegerMode(db: Pick<Client, "execute">): Promise<void> {
  if (typeof db === "object" && supportedIntegerClients.has(db)) return;
  const result = await db.execute(INTEGER_MODE_PROBE);
  const value = result.rows[0]?._pyre_integer_mode;
  if ((typeof value !== "number" || value !== 1) && (typeof value !== "bigint" || value !== 1n)) {
    if (typeof value === "string") {
      throw new Error('@libsql/client intMode "string" is unsupported; use "number" or "bigint"');
    }
    throw new Error("Unsupported libSQL integer representation");
  }
  if (typeof db === "object") supportedIntegerClients.add(db);
}

export function internalSafeInteger(value: unknown, label: string): number {
  if (typeof value === "string") {
    throw new Error(`Invalid ${label}: @libsql/client intMode "string" is unsupported`);
  }
  const number = typeof value === "bigint" ? Number(value) : value;
  if (typeof number !== "number" || !Number.isSafeInteger(number)) throw new Error(`Invalid ${label}`);
  return number;
}
