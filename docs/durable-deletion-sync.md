# Durable Deletion Sync

Protocol 2 adds durable hard deletes, including SQLite cascades, while retaining
updatedAt-based catchup and direct low-latency live deltas. There is no current-row
revision mapping, upsert log, or new application-row column. Permission revocation
is not a deletion event and remains outside this protocol.

## Storage

Generated triggers advance `_pyre_sync.server_revision` in the writing transaction.
Deletes also insert `_pyre_sync_tombstones(sequence, table_name, primary_key)`.
Keys retain their integer/text type. Tombstones contain no old row JSON and never
evaluate row permissions. Primary-key changes capture the old key as a deletion.
Inserts and updates retain no durable per-row revision metadata.

## Catchup and Handoff

1. Restore rows, cursor, epoch, and revision from one IndexedDB snapshot.
2. Register the SSE/WebSocket stream. The server sends `connected` only after
   registration; only then does the client start HTTP catchup.
3. Acquire a short writer barrier, read the database clock, then read each page
   with one SQLite SELECT, including rows, tombstones, epoch, and
   the snapshot's global revision. Rows paginate by `(updatedAt, primary key)`;
   tombstones paginate independently by `last_seen_delete_sequence`. Release the
   transaction after that page. No snapshot transaction spans HTTP requests.
4. Buffer live and mutation-response authoritative deltas while reading or
   persisting catchup pages. Remember the snapshot revision for each returned key.
5. Commit each page's changes, cursor, epoch, and revision atomically in IndexedDB.
   Only storage acknowledgement updates authoritative memory and starts the next
   page. Optimistic state is replayed separately, without resurrecting deleted rows.
6. Drain buffered deltas, accepting a key only if its revision is newer than the
   last observation of that key. A later page's global watermark does not establish
   that a key from an earlier page was observed again.

Ordinary live deltas and mutation-response deltas use the same serialized local
commit path, without an HTTP request. Per-key observations also protect against
out-of-order delivery in steady state. They are ephemeral client state, not a
server-side row mapping. The live buffer is limited to 5,000 rows; overflow forces
a new full row scan AND tombstone replay from sequence zero. This repairs missing
rows with retained deletion evidence, not arbitrary cache corruption for keys that
never existed. Epoch replacement remains the operation for a true cache reset.

Every new catchup round, including same-epoch reconnect, clears the primary-key
component of its starting timestamp cursor and requests retained tombstones from
zero. Replaying tombstones rebuilds absent-key observations after process restart;
even a publication delayed until steady state cannot resurrect a key whose delete
was already consumed before restart. Delete/current-row reinsertion pairs still
come from the same snapshot. The persistent deletion cursor never regresses; replay
positions are transient scan state.

The timestamp boundary is inclusive. Pending wakes and failed reads reuse the
original round-start boundary, not the final page's timestamp. Within a round,
pages use strict timestamp/key ordering. At the end of a round, the client drains
the batch already buffered before servicing a pending wake. Subsequent arrivals
cannot extend that batch indefinitely, so repeated wakes cannot starve live data.
Live delivery never advances the timestamp or deletion cursors on its own.

The persisted checkpoint is separate from the pagination cursor. Its timestamp
cannot exceed the first page's `snapshotTimestamp` clock fence, and its key is
cleared. A crash after scanning a later second therefore still rereads writes in
the earlier interval, including publications not yet delivered. A page without a
clock certificate conservatively retains the starting timestamp boundary. Live
upserts strictly before a safe persisted timestamp interval are already covered;
equal-second and disjoint-key updates are not filtered by a global revision.

Native catchup uses `BEGIN IMMEDIATE`. TypeScript opens a deferred transaction and
acquires its writer lock with an unchanged-value update of `_pyre_sync`, so a failed
lock attempt can also be disposed cleanly by the local libSQL driver. It changes no
revision or epoch. The writer barrier is necessary: a long transaction may assign updatedAt before
catchup starts but commit afterward. Catchup must wait for that writer or fail and
retry, not certify a timestamp which skips it. This requires the authoritative
database, not a read-only replica or a connection inside an older transaction.
It adds database coordination during catchup only; ordinary live deltas do not
issue HTTP requests. Retained tombstone history is replayed on each new round, so
large histories increase reconnect/wake cost. Continuous writes can prevent a
round from finishing; there is no claimed bounded convergence under unbounded load.

Tombstone pages emit the deletion and, if the key currently exists and is visible,
its current row from the same snapshot. This prevents an old tombstone from erasing
a later reinsert, including reinserts with identical timestamps. Deleting an unknown
key is an idempotent no-op in memory, indexes, storage, and entity consumers.

### Timestamp Contract

As with the original updatedAt catchup, writers must maintain updatedAt on every
upsert, with timestamps not moving behind an already completed catchup interval.
Generated mutations use Unix seconds. Equal timestamps are supported by overlap;
arbitrary backdating or a server clock rollback across seconds is not repaired by
that overlap. Such writes require a full resync/epoch reset. Strictly incrementing
each row's timestamp would not solve ordering between different rows, and assigning
a global sequence to updatedAt would stop it being a wall-clock timestamp. Neither
semantic change is installed here.

External SQL writers get durable deletion capture, but must maintain updatedAt and
publish a scoped wake for immediate refresh. A reliable stream is required during
handoff; reconnect always repeats catchup. No stateless timestamp protocol can
recover arbitrary backdated upserts without a scan or additional durable history.

## Database Scope

The host authenticates and authorizes the session for its selected physical
database before query execution, catchup, or stream registration. Native `pyre
serve` has one registry per database. TypeScript connections must carry a
server-assigned `databaseId`; unscoped and other-database connections are excluded.
`SyncServer` callers must pass only that database's authorized sessions. Catchup
rejects multi-namespace project contexts; load the selected database's context.

Table/key activity disclosure within that authorized database is intentional.
Even filtered entity subscriptions receive deletion IDs without checking old rows.

## Retention and Epochs

Administrators explicitly call `rotate_database_epoch` (Rust) or
`rotateDatabaseEpoch` (TypeScript). A single write transaction replaces the epoch,
resets the global revision, and clears tombstones. Any failure rolls back everything.
Surviving application rows and their updatedAt values are unchanged.

There is no automatic rotation, retention threshold, timer, or scheduler. Never
truncate tombstones independently. A scoped `syncRequired` after rotation refreshes
connected clients immediately; otherwise reconnect or the next sync detects it.
Epoch mismatch atomically replaces local persisted state before fetching a baseline.

## Integration

Run `ensure_database` / `ensureDatabase`, CLI migration push, or newly generated
migrations to install storage and triggers, even on an unchanged application schema.
Low-level migration users must execute internal setup and
`sync_tombstone_trigger_sql` in the migration transaction.

Rebuild WASM and regenerate query/client artifacts together. Requests include
`syncCursor: { version: 2, tables: ... }`; responses include `syncVersion: 2`.
Timestamp cursors use a `timestamp-v2:` permission-hash prefix. IndexedDB version 4
invalidates earlier caches once: version 3's unfenced pagination checkpoints cannot
prove that same-second changes survived a restart. This does not rotate the server epoch. Nonempty
persisted cursors must be accompanied by their database epoch.

TypeScript mutations use `runWithSync` and invoke `result.sync(sendToSession)`.
Pass the maintained live connection registry, not a pre-mutation snapshot;
recipients are selected from that registry at publication time.
Native mutations use `query::run_sync` and `SyncServer::calculate_deltas`. Mutation
revisions and cascade tombstones are captured before commit. Publication never
allocates a new postcommit revision. Oversized live payloads fall back to a wake.

Entity changes are `{ op: "row", tableName, id, row }` or
`{ op: "delete", tableName, id }`. Generated Elm streams expose typed constructors
such as `PostRow Posts.Row` and `PostDeleted Posts.Id`.
Compact live table groups represent deletes with `headers: ["$delete"]` and one
typed key per row; `$delete` cannot collide with a schema column name.
