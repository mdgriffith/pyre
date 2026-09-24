import { expect, test } from "bun:test";
import { createClient } from "@libsql/client";
import type { SchemaMetadata } from "@pyre/core";
import { seed } from "./query";

test("seed leaves sequence allocation to SQLite and rejects explicit values", async () => {
  const db = createClient({ url: ":memory:" });
  try {
    await db.execute("create table events (key TEXT NOT NULL UNIQUE, sequence INTEGER PRIMARY KEY AUTOINCREMENT, payload TEXT NOT NULL)");
    const schema: SchemaMetadata = {
      tables: {
        events: {
          name: "events",
          columns: [
            { name: "key", type: "Id.Uuid", nullable: false, primary: true, unique: true, indexed: true },
            { name: "sequence", type: "Sequence.Int", nullable: false, primary: false, unique: true, indexed: true },
            { name: "payload", type: "String", nullable: false, primary: false, unique: false, indexed: false },
          ],
          links: {},
          indices: [{ field: "key", primary: true, unique: true }],
        },
      },
      queryFieldToTable: { event: "events" },
    };
    const key = "01900000-0000-7000-8000-000000000001";
    const result = await seed(db, schema, { events: [{ key, payload: "first" }] });
    expect(result).toEqual({ kind: "success", response: { events: [{ key, sequence: 1, payload: "first" }] } });
    const invalid = await seed(db, schema, { events: [{ key: "01900000-0000-7000-8000-000000000002", sequence: 99, payload: "forged" }] });
    expect(invalid.kind).toBe("error");
    if (invalid.kind === "error") {
      expect(invalid.error?.errorType).toBe("InvalidInput");
      expect(invalid.error?.message).toContain("server-managed sequence");
    }
    expect((await db.execute("select count(*) as count from events")).rows[0].count).toBe(1);
  } finally {
    db.close();
  }
});
