/**
 * Pyre Server Helpers
 * 
 * Utilities for building Pyre-powered servers with query execution,
 * mutation handling, and permission-aware syncing.
 */

export { run, seed } from "./query";
export { DatabaseOutcomeUnknown, ReseedError, ensureDatabase, initDisposableDatabase, inspectDatabasePolicy, reseedDatabase, resetDatabase, setDatabaseDisposable } from "./schema";
export { databaseIdFromUrl, requireDatabaseId, withDatabaseId } from "./database-id";

export type { DatabaseId } from "./database-id";
export type { DatabasePolicy, EnsureDatabaseOutcome } from "./schema";
export { applyFinalizedMigrations, auditDatabaseSchema, baselineDatabase, FinalizedMigrationError, initPreservedDatabase, inspectMigrationOutcome, setDatabasePreserved } from "./schema";
export type { FinalizedMigrationBundle, MigrationApplication, MigrationCatalog, MigrationInspection, MigrationReceipt, MigrationSource } from "./schema";

// Export only the types that are part of the public API for the functions above
export type {
    QueryResult,
    QueryMap,
    QueryMetadata,
    OperationDescriptor,
    SeedInput,
    SeedOptions,
    SeedResult,
    Session,
    SessionValue,
} from "./query";
