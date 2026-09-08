# Optimistic Mutations

Pyre optimistic mutations let the client apply a mutation locally before the server confirms it.

## In-Flight Mutation

Each optimistic mutation is tracked as an in-flight record:

```ts
type InFlightMutation = {
  requestId: string
  mutationId: string
  forward: TableDelta[]
  inverse: TableDelta[]
}
```

- `forward` is applied immediately to the local table cache.
- `inverse` reverts the local change if the server rejects the mutation.
- `requestId` ties the local optimistic state to the server response.

## Flow

1. Generated client code builds `forward` deltas from the mutation input.
2. Store the in-flight mutation.
3. Build `inverse` deltas from the current local rows.
4. Apply `forward` locally and update active queries.
5. Send the existing mutation request to the server with a stable client mutation id (`requestId`).
6. Resolve the in-flight mutation from the server response.
7. When authoritative live sync or catchup data arrives, apply it first, then replay still-unsettled optimistic mutations in request order.

## Local-Only Deltas

Optimistic deltas are not part of the mutation protocol. They are generated client-side and used only to update the local cache.

- The server never receives client-generated deltas.
- The server never trusts client-generated deltas.
- The server still receives only the normal mutation input at `POST /db/:mutationId`.
- Server authority still comes from normal mutation execution plus live sync/catchup.

Generated optimistic deltas are predictions. They should cover the local rows Pyre can safely derive from the mutation input.

## Server Request

V1 uses the existing mutation endpoint unchanged:

```http
POST /db/:mutationId
```

The HTTP body remains the normal mutation input. No optimistic delta data is sent.

## Server Result

- Success: mark the in-flight mutation acknowledged with the response `serverRevision` and return the normal app result. Acknowledged optimistic layers are pruned only from the front of the request-order queue, so a later drag/update mutation that responds first continues to protect newer local intent until earlier requests settle.
- Failure: apply `inverse`, drop the in-flight mutation, and surface the error.

The mutation's normal response is available to app code. Protocol-level responses should include the client mutation id and, once supported by the server, a monotonic server revision.

## Server Confirmation

Server confirmation happens through the mutation response and the existing live sync or catchup path. When authoritative table data arrives, the normal sync code applies it to the local cache, then replays any still in-flight optimistic mutations so stale or out-of-order server events cannot overwrite newer local intent.

This also handles common server amendments, such as `createdAt`, `updatedAt`, derived values, or normalized data, as long as the optimistic row uses the same primary key as the server row.

## Ordering Contract

- Network arrival order is not causal order.
- `requestId`/client mutation id is for idempotency, dedupe, and acknowledgement.
- Canonical ordering comes from `serverRevision`, a server-assigned monotonic revision on live sync events backed by Pyre internal metadata.
- Clients compare authoritative revisions per key, not against a global page watermark. Catchup pages certify only keys they actually return.
- Clients persist the last applied server revision in local IndexedDB metadata.
- Mutation response envelopes advance the client's last applied server revision and acknowledge the matching optimistic layer without immediately dropping later acknowledged layers behind older unsettled requests.
- Clients must tolerate receiving their own mutation through live sync, even if the server normally suppresses it.

Protocol 2 requires authoritative revisions. Live and mutation-response deltas are buffered during catchup and share its atomic local commit path. Pending optimistic deltas are replayed over separate authoritative memory after each successful commit.

## Origin Suppression

Servers may skip sending live sync events to the connection that originated a mutation. This is an optimization, not a correctness guarantee. Clients must still handle duplicates, retries, reconnect catchup, and multi-tab delivery.

## IDs

Optimistic creates are simplest when synced tables use client-generatable IDs, such as UUIDv7 or ULID.

Server-assigned integer IDs require temp IDs, ID remapping, relationship rewrites, and more complex reconciliation. Pyre should only require client-generated IDs for tables that opt into local optimistic creates, not for every synced table.
