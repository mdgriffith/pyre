# Sync mutation responses are not typed mutation responses

## Part 1: Problem and original implementation reasoning

### Context

Lore uses Pyre's Rust server runtime directly. It has a generic typed wrapper around generated queries:

```rust
pub(crate) async fn run_clocktower_pyre_typed<Q>(
    &self,
    input: Q::Input,
    session: &PyreSession,
    origin_session_id: Option<&str>,
    sync_mode: bool,
) -> Result<Q::Output, AppError>
where
    Q: PyreQuery,
```

The wrapper chooses `pyre::server::query::run_sync` when `sync_mode` is true, performs live-sync fanout with `SyncServer::calculate_deltas`, and then decodes `QueryResult.response` as the generated `Q::Output` type.

The original reasoning was that normal and synchronized execution represented the same query operation with an additional synchronization side effect:

```text
generated mutation
    -> execute sync SQL
    -> receive the generated mutation response
    -> derive affected rows
    -> broadcast those rows
    -> decode and return the generated mutation response
```

That interpretation appeared reasonable for several reasons:

1. `run` and `run_sync` have the same arguments and both return `QueryResult`.
2. `QueryResult` always has both `response` and `affected_rows` fields.
3. The `run` documentation says the runtime performs "response formatting" and "`_affectedRows` extraction for live sync deltas."
4. Generated sync SQL returns both the normal mutation column and `_affectedRows`. For example, it returns columns shaped like:

   ```sql
   returning
     json_object(...) as "clocktowerParticipant",
     json_array(...) as _affectedRows
   ```

5. A generated Rust output type still exists for the mutation, so it was natural to expect synchronized execution to remain compatible with that output type.

### Actual behavior

The Rust runtime deliberately discards the formatted mutation response in sync mode:

```rust
Ok(QueryResult {
    response: if sync_mode {
        JsonValue::Object(serde_json::Map::new())
    } else {
        format_response(&included_result_sets)?
    },
    affected_rows: extract_affected_rows(&included_result_sets)?,
})
```

The test `run_insert_mutation_extracts_affected_rows_in_sync_mode` explicitly asserts:

```rust
assert_eq!(result.response, json!({}));
```

`SyncServer::calculate_deltas` later replaces that empty response with a synchronization envelope:

```json
{
  "serverRevision": 12,
  "sync": { "type": "delta", "data": [] },
  "result": {}
}
```

Neither `{}` nor the synchronization envelope has the shape of the generated mutation output. A generated decoder expecting this:

```json
{
  "clocktowerParticipant": [
    {
      "id": 7,
      "gameId": "...",
      "status": { "_type": "ClocktowerParticipantApproved" }
    }
  ]
}
```

therefore fails with errors such as:

```text
CreateClocktowerGame output decode failed: missing field `clocktowerGame`
ClocktowerParticipantCreate output decode failed: missing field `clocktowerParticipant`
```

The SQL mutation has already committed when this decoding failure occurs. In Lore, game creation inserted the game and then returned HTTP 500. This left partially created games behind and made the failure look like a database or generated-code problem.

### Why the first diagnosis was misleading

The first working theory was that delta calculation consumed or rewrote a previously valid response because `calculate_deltas` takes `&mut QueryResult`. Preserving `result.response` before fanout seemed like a reasonable defensive change.

That only preserved the empty object created by `run_sync`, however. The later error moved from a missing field inside a sync envelope to a missing field at column 2 of `{}`, which exposed that the mutation response had never been formatted in the first place.

Inspecting `src/server/query.rs`, `src/server/sync.rs`, and the Rust query tests clarified the actual contract: current sync mode is an affected-row/delta operation, not a typed mutation-result operation.

### Current Lore workaround

Lore now separates the two execution paths:

- Typed Pyre execution is non-sync and may be decoded as `Q::Output`.
- Server-owned synchronized mutations use a dedicated runner that returns `QueryResult` without attempting typed output decoding.
- Mutations that require inserted data recover it from `affected_rows`.
- The typed Clocktower wrapper rejects `sync_mode = true` to prevent this mistake from recurring.

This works, but recovering domain records from table headers and raw affected-row values duplicates generated decoding logic and weakens the type-safety Pyre normally provides.

## Part 2: Suggested fix

### Preferred behavior

Provide a synchronized execution path that retains the normal formatted mutation response while also extracting affected rows.

The sync SQL already returns both sets of information, and `format_response` already ignores underscore-prefixed columns such as `_affectedRows`. The Rust runtime can therefore format the response in both modes:

```rust
Ok(QueryResult {
    response: format_response(&included_result_sets)?,
    affected_rows: extract_affected_rows(&included_result_sets)?,
})
```

After delta calculation, the protocol response would become:

```json
{
  "serverRevision": 12,
  "sync": { "type": "delta", "data": [] },
  "result": {
    "clocktowerParticipant": [
      {
        "id": 7,
        "gameId": "...",
        "status": { "_type": "ClocktowerParticipantApproved" }
      }
    ]
  }
}
```

Server integrations could then decode `response.result` as the generated output while clients still receive the synchronization metadata.

### Preserve compatibility through explicit APIs

If omitting mutation results in sync mode is an intentional bandwidth or optimistic-update contract, avoid silently changing `run_sync`. Instead, make the distinction explicit:

```rust
pub async fn run_sync(...) -> Result<SyncQueryResult, Error>;

pub async fn run_sync_with_result(...) -> Result<SyncMutationResult, Error>;
```

Suggested result types:

```rust
pub struct SyncQueryResult {
    pub affected_rows: Vec<AffectedRowTableGroup>,
}

pub struct SyncMutationResult {
    pub response: JsonValue,
    pub affected_rows: Vec<AffectedRowTableGroup>,
}
```

This is preferable to returning the same `QueryResult` type with a `response` field whose meaning changes according to an execution-mode boolean.

The TypeScript runtime should expose the same distinction. It currently has the equivalent behavior:

```ts
const includeResult = !useSyncMode;
const response = includeResult ? formatResultData(activeSql, resultSets) : {};
```

Rust and TypeScript should agree on whether a synchronized mutation can return its generated result and on the exact envelope shape.

### Generated typed helper

The server runtime could provide a helper that makes the intended operation difficult to misuse:

```rust
pub async fn run_typed_sync<Q>(...) -> Result<TypedSyncResult<Q::Output>, Error>
where
    Q: GeneratedQuery;
```

Conceptually:

```rust
pub struct TypedSyncResult<T> {
    pub result: T,
    pub affected_rows: Vec<AffectedRowTableGroup>,
    pub server_revision: Option<i64>,
}
```

This would let an application perform one mutation, broadcast its deltas, and retain generated output decoding without manually reconstructing records from affected rows.

### Documentation changes

At minimum, document the current contract directly on `run_sync`:

- Sync mode executes `syncSql`.
- The normal generated query result is currently discarded.
- `response` is `{}` until delta calculation and then becomes a sync protocol envelope.
- `response` must not be decoded as the generated query output.
- Applications that need returned mutation rows must use non-sync execution or read `affected_rows`.

The current `run`/`run_sync` signatures and shared `QueryResult` type otherwise strongly suggest substitutability.

### Regression tests

Add tests covering the intended contract end to end:

1. A synchronized insert returns affected rows.
2. The synchronized insert retains the normal formatted mutation result when using the new result-preserving API.
3. Delta calculation wraps that result under `result` without replacing it with `{}`.
4. A generated Rust output decoder can decode the preserved `result` value.
5. Rust and TypeScript runtimes produce equivalent response envelopes.

The key invariant should be explicit: if Pyre exposes a result-preserving synchronized mutation API, synchronization must not force server applications to abandon generated output types.
