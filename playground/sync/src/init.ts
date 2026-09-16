import { createClient } from "@libsql/client";
import type { Client } from "@libsql/client";
import { readFileSync, unlinkSync } from "fs";
import { join } from "path";
import { existsSync } from "fs";
import init, {
    migrate,
    sql_is_initialized,
    sql_introspect,
    sql_introspect_uninitialized,
    set_schema,
} from "../../../wasm/pkg/pyre_wasm.js";
import { SEEDED_USER_COUNT, userIdForIndex } from "./userIds";

// Initialize WASM
const wasmPath = join(process.cwd(), "..", "..", "wasm", "pkg", "pyre_wasm_bg.wasm");
const wasmBuffer = readFileSync(wasmPath);
await init({ wasm: wasmBuffer });

const DB_PATH = join(process.cwd(), "test.db");

async function runMigration(dbPath?: string) {
    const targetDbPath = dbPath || DB_PATH;
    const db = createClient({
        url: `file:${targetDbPath}`,
    });

    // Read schema file
    const schemaPath = join(process.cwd(), "pyre", "schema.pyre");
    if (!existsSync(schemaPath)) {
        throw new Error(`Schema file not found: ${schemaPath}`);
    }
    const schemaSource = readFileSync(schemaPath, "utf-8");

    // Load schema into WASM cache BEFORE migration (required for migrate to work)
    // Check if database is initialized and introspect accordingly
    await loadSchema(db);

    // Run migration using WASM
    console.log("Running migration...");
    const migrationResult = migrate("init", schemaSource);

    if ("Ok" in migrationResult) {
        try {
            if (migrationResult.Ok.sql.length > 0) {
                await db.batch(migrationResult.Ok.sql);
                await db.execute(migrationResult.Ok.mark_success);
                console.log("Migration completed");
            } else {
                console.log("No migration changes needed");
            }
        } catch (error) {
            console.error("Migration execution failed:", error);
            const markFailure = migrationResult.Ok.mark_failure;
            markFailure.args.push(JSON.stringify(error));
            await db.execute(markFailure);
            throw error;
        }
    } else {
        console.error("Migration failed:", migrationResult.Err);
        throw new Error(`Migration error: ${JSON.stringify(migrationResult.Err)}`);
    }

    // Reload schema into WASM cache after migration (to update cache with new state)
    await loadSchema(db);
}

// Initialize database (delete if exists, migrate, and seed)
export async function initializeDatabase(dbPath?: string) {
    const targetDbPath = dbPath || DB_PATH;

    // Delete database if it exists
    if (existsSync(targetDbPath)) {
        console.log("Deleting existing database...");
        unlinkSync(targetDbPath);
    }

    await runMigration(targetDbPath);
    await seedDatabase(targetDbPath);
    console.log("Database initialization completed");
}

async function loadSchema(db: Client) {
    const isInitializedQuery = sql_is_initialized();
    const isInitializedResult = await db.execute(isInitializedQuery);

    if (isInitializedResult.rows.length === 0) {
        throw new Error("Failed to check if database is initialized");
    }

    const isInitialized = isInitializedResult.rows[0].is_initialized === 1;

    let introspection;
    if (isInitialized) {
        const introspectionQuery = sql_introspect();
        const introspectionResult = await db.execute(introspectionQuery);
        if (introspectionResult.rows.length === 0) {
            throw new Error("Failed to get introspection result");
        }
        introspection = JSON.parse(introspectionResult.rows[0].result as string);
    } else {
        const uninitializedIntrospect = sql_introspect_uninitialized();
        const introspectionResult = await db.execute(uninitializedIntrospect);
        if (introspectionResult.rows.length === 0) {
            throw new Error("Failed to get introspection result");
        }
        introspection = JSON.parse(introspectionResult.rows[0].result as string);
    }

    set_schema(introspection);
    console.log("Schema loaded into WASM cache");
}

async function seedDatabase(dbPath?: string) {
    const targetDbPath = dbPath || DB_PATH;
    const db = createClient({
        url: `file:${targetDbPath}`,
    });

    console.log("Seeding database...");

    const users = Array.from({ length: SEEDED_USER_COUNT }, (_, offset) => {
        const index = offset + 1;
        return {
            sql: "INSERT INTO users (id, name, email) VALUES (?, ?, ?)",
            args: [userIdForIndex(index), `User ${index}`, `user${index}@example.com`],
        };
    });
    await db.batch(users);
    console.log(`Seeded database with ${users.length} users`);

    console.log("Seeding completed");
}

// Main execution (when run as a script)
if (import.meta.main) {
    try {
        await initializeDatabase();
        console.log("✅ Initialization completed successfully");
    } catch (error) {
        console.error("❌ Initialization failed:", error);
        process.exit(1);
    }
}
