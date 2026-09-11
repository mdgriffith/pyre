# @pyre/server

Server runtime helpers for executing generated Pyre queries.

Typical usage:

- import generated `queries` map from `pyre/generated/typescript/server`
- initialize schema-specific databases through the generated `databases` map
- execute with `run` from `@pyre/server/query`
- seed fixture data with the generated `seed` helper from `pyre/generated/typescript/seed`
- use sync helpers from `@pyre/server/sync` and `@pyre/server/query-sync`

## Optional Session Caching

The optional `createContextManager` from `@pyre/server/context` caches a server-only session
for each authenticated login-session/database pair. Supply `getSessionKey`,
`resolveSession`, `getDatabase`, and `maxAgeMs`; then use
`await manager.get(session, databaseId)` and `context.run(operation, input)`.
The operation receives the application's database handle and the database-specific
session, so it can call the existing executor without changing a global session.

The application keeps connection management and authentication. No session values
are sent to clients, and no routes or operation policies are added. See the
[server contexts guide](../../docs/usage/server-contexts.md) for an
annotated example and invalidation requirements.

For the normal client/server integration, start with the [sync guide](../../docs/usage/sync.md). The context manager is not required for sync.

## Database Provisioning

Generated output embeds each namespaced schema and binds it to the transactional
`ensureDatabase` runtime helper:

```ts
import { createClient } from "@libsql/client";
import { init } from "@pyre/server/wasm";
import { databases } from "./pyre/generated/typescript/server";

await init();

const main = createClient({ url: "file:main.db" });
const campaign = createClient({ url: "file:campaign-123.db" });

await databases.Main.ensureDatabase(main);
await databases.Campaign.ensureDatabase(campaign);
```

The call returns `"created"`, `"migrated"`, or `"up-to-date"`. It is safe to
call whenever a database is opened: introspection, planning, DDL, and migration
recording happen in one write transaction. Pyre rejects non-empty databases
that do not already contain Pyre migration metadata.

## Compiled Batches

`runBatch` from `@pyre/server/query` is the server execution boundary for an
ordered list of allowlisted compiled operations. It also supports server seed
submissions with an explicit validated session, without the permission-bypassing
legacy seed helper. It does not implement client builders or transport routing.

```ts
const result = await runBatch(database, {
  version: 1, manifestVersion, queries, SessionValidator: Decode.SessionValidator,
}, {
  databaseId: "tenant-1", namespace: "Main", manifest: manifestVersion,
  instance: "tab-1", authGeneration: 2,
}, {
  version: 1, instance: "tab-1", authGeneration: 2, requestId: "request-1", sequence: 1,
  databaseId: "tenant-1", namespace: "Main", manifest: manifestVersion, databaseEpoch: epoch,
  operations: [{ operation: compiledOperationId, input }],
}, effectiveSession);
```

The manifest and authority arguments are trusted server configuration. Resolve
the actual connection, database ID, namespace, manifest fingerprint, client
instance and auth generation independently of the request. The manifest's
`version: 1` is its format version, not the fingerprint in the authority/request
`manifest` field. Supply the compiler-emitted `manifestVersion` alongside its
queries; it must equal the trusted binding's `manifest`. The stored epoch is
checked inside the transaction.
Database IDs must uniquely identify databases in
this process; batch execution is queued by that ID. The queue does not serialize
legacy runners, other processes, or other writers; SQLite provides transaction
isolation. The database must already have its `_pyre_sync` metadata.
Use a file-backed or remote libsql database for nonempty batches. The local
adapter detaches its connection during `transaction()`, so `runBatch` checks
the public `Client.protocol` and SQLite's `PRAGMA database_list` before opening
a local transaction. Local databases without a nonempty `main.file` are rejected
with sanitized `TransactionFailed`, without detachment, writes, revision allocation,
or publication. This conservatively includes private/shared in-memory and temporary
databases; no configuration flag bypasses the check. Empty batches still confirm
without database I/O. The check is not cached. Remote clients retain their existing
transaction path. This guard does not protect calls made directly to the adapter's
`transaction()` outside this executor.

Requests are captured before any await and limited to 100 operations and 1 MiB
of UTF-8 serialized request data. All request fields are required; unknown
envelope/member fields and invalid types reject. Numeric fences must be safe
JavaScript integers (`authGeneration >= 0`, `sequence > 0`). The manifest's full
`SessionValidator` validates the effective session once, including empty batches.
Generated `Decode.SessionValidator` includes fields unused by a particular query;
do not substitute a per-query session projection. Application claims outside
declared session fields are ignored, including extra structured claims; they
cannot override declared fields via read-projection prefixes. Existing session
compatibility is retained for `0`/`1` booleans and omitted nullable fields.
Declared nested session fields are validated recursively before projection decoding.
Each member validates input,
including rejection of fields removed by strip-mode codecs. Canonical tag-only
enum objects are allowed to decode to strings, but extra fields still reject.
A query's `primary_db` must equal the trusted `namespace`; attachments are rejected.
Only compiled `insert`, `update`, `delete`, or `transaction` operations are allowed.
An empty batch returns an empty result without I/O or publication.

Compiler metadata uses:

```ts
generatedEdit?: {
  kind: "create" | "update" | "delete";
  writeStatementIndices: number[]; // exactly one zero-based index in sql, include: true
  writableInputs: string[];       // update setters, excluding identity/managed fields
}
json_session_args?: string[];     // session bindings serialized as whole JSON, not union tags
json_session_validators?: Record<string, ZodType>; // canonical typed-JSON session binding codecs
```

The nominated compiler statement returns the raw authorized identity as
`_pyreEditId`. Its direct affected count
must be exactly one. Generated results are `{ id }`, independent of read
visibility and the legacy named `ReturnData` codec. Named results retain their
declared wire shape and validate with `ReturnData` when provided, without
replacing wire timestamps or enum objects with TypeScript codec transformations.
The local libsql adapter
reports zero `rowsAffected` for result-producing statements; the shared executor
captures SQLite `changes()` immediately after nominated returning writes to
recover the direct count, excluding triggers/cascades. Other writes use the
adapter's `rowsAffected` directly. No returned-row count authorizes a write.

Success returns `{ kind: "success", response }`. `response` matches the Rust
wire payload exactly: `{ requestId, databaseId, instance, authGeneration,
databaseEpoch, namespace, manifest, status, results: [{ index, operation, value }],
commitRevision, reconciliation }`. `status` is `accepted` for a nonempty batch;
an empty batch is `confirmed` with `results: []` and no revision/reconciliation.
Neither `version` nor `sequence` is echoed in responses.
Every nonempty committed batch allocates one revision in the
write transaction, even named no-ops. Reconciliation conservatively requests a
full replacement with `invalidate: true` and `minimumSafeRevision` equal to the
commit revision. Errors return `{ kind: "error", error: { errorType, message,
index? } }`, with zero-based member index when known and both strings set to
the sanitized Rust `Error::code()` equivalent: `InvalidRequest`, `InvalidSession`,
`InvalidEdit`, `TargetNotWritable`, or `TransactionFailed`. Session failures are
batch-level, without a member index. No failure contains successful prefixes.
A failed commit acknowledgement returns
`kind: "unknown"`, not a definitive rejection.

An optional final `publish` callback runs after commit and cannot turn commit
evidence into rejection. `runBatchWithSync` from `@pyre/server/query-sync` sends
postcommit replacement hints without allocating another revision or requiring
origin registration. Routing must authenticate the trusted instance/auth binding,
bound the raw body before JSON decoding, wrap errors with the request fence,
and supply only authorized recipients for
the bound database. Existing named-single `run`, `toRunner`, and `runWithSync`
contracts are unchanged.

Actual compiler fixtures live under `fixtures/compiled-batch`; regenerate them
with `bun packages/server/fixtures/compiled-batch/regenerate.ts` after building
the compiler. Regeneration also compiles the generated server module and verifies
its `manifestVersion` export. Rust and TS tests execute the same fixture schema.

### Authoritative Replacement

`catchupReplacement` from `@pyre/server/query-sync` takes a database connection,
generated `manifest`, trusted `BatchAuthority`, replacement request, and effective
session. The request contains `version: 1`, `requestId`, `target`, and the same
flat database/instance/auth/namespace/manifest/epoch fences as a batch. The host
must authenticate the authority independently of the request.

A successful response has `type: "replacement"`, `scope: "database"`,
`complete: true`, `serverRevision`, `tables: { [name]: { rows: [...] } }`, and
the request's fences, ID, and target. All rows and the revision are read in one
transaction. Install the whole scope atomically, deleting absent rows; an empty
scope is not a no-op. Legacy timestamp catchup pages are not replacement evidence.
The generated `compiledContract` must match the captured schema, and malformed
stored rows reject the entire snapshot. Synced linked read permissions require
this replacement path; legacy catchup rejects them with `ReplacementRequired`.

`runBatchWithSync` recipients contain `{ session, fence }` from authenticated
registration. Hints contain each recipient's fence, `type: "syncRequired"`,
`serverRevision`, and conservative `reconciliation` invalidation metadata, never
private rows or origin request IDs. Hints only raise required/security revision;
they cannot advance covered revision. Named `runWithSync` mutations also commit
their revision with the write; call the returned `sync(send)` to publish it.
Failed or delayed SSE delivery does not change known batch acceptance.

The libraries register no routes. The built-in server exposes POST
`/sync/replacement` and POST `/sync/replacement/events` for replacement and fenced
SSE registration, separately from its legacy sync routes. Signed sessions bind
the instance and auth generation as they do for POST `/db`.

Replacement currently materializes a complete scope in memory, not pinned pages.
TypeScript rejects payloads above 64 MiB rather than returning partial coverage;
remote libSQL remains unverified. The opt-in [browser local-edit runtime](../../docs/usage/local-edits.md)
implements client installation, pending-intent replay and receipt settlement through
configured transport adapters. Schema-branded builders and typed seed binding remain downstream.

Build WASM before running the production-boundary fixture:

```sh
npm exec --package=wasm-pack -- wasm-pack build wasm --target web --out-dir ../packages/server/wasm
npm exec --package=bun -- bun packages/server/fixtures/replacement-wasm.ts
```

The focused query-sync suite runs this fixture when the ignored WASM artifact
is present and explicitly skips it otherwise. Mocked WASM unit tests alone do
not establish generated-schema conformance.

Write codecs are separate from permissive read-projection codecs. Both runtimes
require safe integers, canonical boolean inputs, and complete structured variants
with explicit nullable fields. Unknown structured write fields reject rather than
being silently discarded. Typed JSON is normalized recursively: nested dates
become Unix seconds, nested enums retain their tagged-object representation, and
list/dictionary members and nulls are preserved. UUID codecs intentionally accept
strings without UUID syntax validation in both runtimes.

Regenerated query metadata includes `json_session_validators` for typed-JSON
session arguments. These codecs run when binding SQL, after ordinary session
decoding, so nested enum tags have the same object representation as stored
writes. Scalar enum session arguments outside JSON still bind as strings.

## Seed Data

Generated server output includes a schema-bound `seed` helper for server-side fixtures and imports:

```ts
import { createClient } from "@libsql/client";
import { seed } from "./pyre/generated/typescript/seed";

const db = createClient({ url: "file:test.db" });

const result = await seed(db, {
  users: [
    {
      name: "Fred",
      posts: [
        { title: "example post", content: "My content!" },
        { title: "example post2", content: "My content!" },
      ],
    },
  ],
});
```

Top-level keys are table names. Nested keys must be links declared on the parent table; Pyre derives foreign keys from the link metadata. You can also seed flattened layers by setting foreign key columns directly.

The seed call is atomic: if any row fails validation or insertion, Pyre rolls back the transaction. The returned data contains the full inserted rows, including nested rows.

Seed currently bypasses Pyre query permissions and does not update Pyre sync metadata. Use it for setup/import workflows before synced clients rely on live deltas.

## Install

```bash
bun add @pyre/server zod@^4
```

Pyre-generated TypeScript and `@pyre/server` support Zod 4. Zod 3 is not supported.

## Sync Lifecycle Profiling

Run a local in-memory profile:

```bash
bun run profile:sync
```

Run the same profile against Turso:

```bash
TURSO_DATABASE_URL=libsql://... \
TURSO_AUTH_TOKEN=... \
SYNC_PROFILE_ALLOW_REMOTE_WRITES=1 \
bun run profile:sync
```

Useful knobs:

- `SYNC_PROFILE_ROWS`, default `1000`
- `SYNC_PROFILE_PAGE_SIZE`, default `1000`
- `SYNC_PROFILE_ITERATIONS`, default `10`
- `SYNC_PROFILE_SESSIONS`, default `25`
- `SYNC_PROFILE_MIMIC_RTT_MS`, default `20`
- `SYNC_PROFILE_MIMIC_BANDWIDTH_MBPS`, default `25`

The profile creates an isolated `pyre_sync_profile_notes` table and reports total time, average time, and percentage by phase for catch-up and mutation-to-delta sync.
It also compares row-materialized catch-up with a SQLite aggregate JSON catch-up shape and prints a simple remote mimic estimate from measured DB payload bytes.
