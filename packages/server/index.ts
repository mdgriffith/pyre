/**
 * Pyre Server Helpers
 * 
 * Utilities for building Pyre-powered servers with query execution,
 * mutation handling, and permission-aware syncing.
 */

export { run, runBatch, seed, MAX_BATCH_OPERATIONS, MAX_BATCH_PAYLOAD_BYTES } from "./query";
export { ensureDatabase } from "./schema";
export { databaseIdFromUrl, requireDatabaseId, withDatabaseId } from "./database-id";

export type { DatabaseId } from "./database-id";
export type { EnsureDatabaseOutcome } from "./schema";

// Export only the types that are part of the public API for the functions above
export type {
    QueryResult,
    QueryMap,
    QueryMetadata,
    BatchAuthority,
    BatchManifest,
    BatchRequest,
    BatchResult,
    BatchResponse,
    SeedInput,
    SeedOptions,
    SeedResult,
    Session,
    SessionValue,
} from "./query";
