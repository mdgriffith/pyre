/**
 * Pyre Server Helpers
 * 
 * Utilities for building Pyre-powered servers with query execution,
 * mutation handling, and permission-aware syncing.
 */

export { run, seed } from "./query";
export { ensureDatabase } from "./schema";
export { databaseIdFromUrl, requireDatabaseId, withDatabaseId } from "./database-id";
export { createDatabaseRuntime, DatabaseRuntimeError } from "./ephemeral";

export type { DatabaseId } from "./database-id";
export type { EnsureDatabaseOutcome } from "./schema";
export type {
    Contract,
    DatabaseRuntime,
    DatabaseRuntimeConfig,
    EphemeralChange,
    EphemeralDelivery,
    EphemeralSnapshot,
} from "./ephemeral";

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
