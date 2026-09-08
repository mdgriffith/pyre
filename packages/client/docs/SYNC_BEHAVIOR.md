# Sync Behavior

1. Restore rows, cursors, epoch, and revision atomically from IndexedDB.
2. Register the live stream and wait for the server's `connected` acknowledgement.
3. Fetch updatedAt/key catchup pages and durable tombstones, buffering live and
   mutation-response deltas during HTTP reads and local writes.
4. Commit changes and cursors atomically before applying authoritative memory,
   replaying optimistic state, notifying queries, or requesting another page.
5. Reconcile buffered deltas per key against the snapshot that actually returned
   that key. Never discard a change just because another key has a newer revision.
6. Apply steady-state live deltas through the same local commit path, without HTTP.

New catchup rounds use a conservative persisted timestamp checkpoint and replay
retained tombstones from zero. Same-epoch reconnect always starts a round. Wakes
during pagination reuse the original round-start boundary, after draining a bounded
batch of buffered live data. Overflow restarts both rows and tombstones, not just rows.
The persisted checkpoint cannot pass the first page's writer-barrier clock fence;
pagination progress alone is not proof that earlier timestamp buckets are complete.

The epoch scopes all revisions. An epoch mismatch clears authoritative memory and
optimistic state, atomically replaces persisted state, and fetches a new baseline.
Failed storage writes do not advance authoritative memory or the persisted cursor.

Mutation responses contain a transaction-stamped authoritative delta as well as
the normal query result. Server hosts must await `result.sync(...)` before returning
the response; this publishes already committed metadata, not a new revision.

Public sync state is `not_started`, `catching_up`, or `live`. Queries run against
authoritative memory with pending optimistic mutations replayed over it. Deletions
remove memory/index entries, persisted rows, and entity-stream rows; unknown IDs
are harmless. No permission filter is applied to database-scoped deletion IDs.

See [Durable Deletion Sync](../../../docs/durable-deletion-sync.md) for cursor
semantics, timestamp assumptions, database authorization, retention, and upgrades.
