# Pyre Query Guide

Pyre query files define typed database operations against a Pyre schema. Query files usually live in the same `pyre/` tree as schema files. Any non-schema `.pyre` file under that tree is treated as a query file. A common convention is `pyre/query.pyre`.

## Select Queries

Use `query` to read records and shape the returned data.

```pyre
query GetUser($id: User.id) {
    user {
        @where { id == $id }
        id
        name
    }
}
```

Nested selections follow schema links.

```pyre
query GetPosts {
    post {
        id
        title
        author {
            id
            name
        }
    }
}
```

## Mutations

Use `insert`, `update`, and `delete` for writes.

```pyre
insert CreateUser($id: User.id, $name: String) {
    user {
        id = $id
        name = $name
    }
}
```

```pyre
update RenameUser($id: User.id, $name: String) {
    user {
        @where { id == $id }
        name = $name
    }
}
```

An update cannot assign a record field marked `@immutable`, even when the assignment would write the existing value. Immutable fields may still be selected in the mutation result. The same rule applies to update steps inside `transaction` blocks and to dynamic queries submitted through MCP.

```pyre
delete DeleteUser($id: User.id) {
    user {
        @where { id == $id }
    }
}
```

## Transaction Blocks

Use `transaction` to define a named, precompiled operation containing ordered `insert`, `update`, and `delete` steps. All steps commit together; validation and execution failures roll back the transaction. A lost connection during commit can leave the outcome unknown: do not assume rollback or automatically retry the write.

```pyre
transaction ReplaceNote($id: Note.id, $newId: Note.id, $body: String) {
    update changed: note {
        @where { id == $id }
        body = $body
        id
    }

    insert created: note {
        id = $newId
        body = $body
    }

    delete removed: note {
        @where { id == $id }
        id
    }
}
```

Transaction steps:

- execute in declaration order
- share the transaction's parameters and `Session` values
- use the normal typechecking and permission rules for their operation
- must all write to the same schema namespace and database
- may use nested inserts where the selected runtime supports them

Each step must explicitly start with `insert`, `update`, or `delete`. Read-only `query` steps are not allowed. A step alias such as `changed`, `created`, or `removed` becomes a key in the combined result:

```json
{
  "changed": [{ "id": "01900000-0000-7000-8000-000000000001", "body": "new body" }],
  "created": [{ "id": "01900000-0000-7000-8000-000000000002", "body": "new body" }],
  "removed": [{ "id": "01900000-0000-7000-8000-000000000001" }]
}
```

Use unique, descriptive aliases when multiple steps target the same record. Without an alias, the record field name is used as the result key, and duplicate result keys are rejected.

These handwritten UUID inserts take an explicit identity parameter. Automatic UUIDv7 capture belongs to the generated create-builder API below, not arbitrary named inserts.

An update or delete that matches no permitted rows returns `[]`. That is not an error and does not stop later steps. If later writes must depend on an earlier step matching a row, express that requirement as a database constraint or redesign the operation; transaction steps cannot reference IDs or rows returned by earlier steps.

Nested inserts currently require temporary tables. They work with local SQLite and embedded libSQL, but are rejected by the native remote-libSQL runtime. Flat named transaction blocks do not have that temporary-table restriction. TypeScript executes a named transaction through an atomic database batch; composed operations use an interactive transaction so generated-write cardinality and results can be checked before commit. Local TypeScript composition requires a file-backed database, not `file::memory:`. The verified composition matrix covers local SQLite/browser execution; it does not establish hosted libSQL deployment conformance.

Dynamic transaction blocks can be inspected with `pyre_preview_query` or `pyre_explain_query` and executed with `pyre_query` through MCP.

## Parameters And Filters

Declare parameters in the operation signature and reference them with `$name`.

```pyre
query SearchUsers($name: String) {
    user {
        @where { name == $name }
        id
        name
    }
}
```

## Local Queries And Session

Server-supplied `Session` values can participate in authenticated server query conditions:

```pyre
query MyNotes {
    note {
        @where { ownerId == Session.userId }
        id
        body
    }
}
```

This explicit `Session` dependency is rejected for local browser execution. The client does not hold a session to substitute. Generated local query sources carry this rejection marker:

```json
{
  "$error": "Local queries cannot reference Session; use explicit inputs or execute on the server."
}
```

Do not remove the marker or silently run an unfiltered query. Use an ordinary input to filter already-authorized local data:

```pyre
query NotesByOwner($ownerId: Int) {
    note {
        @where { ownerId == $ownerId }
        id
        body
    }
}
```

Alternatively, explicitly execute the `Session`-dependent query on the authenticated server. There is no automatic server fallback. Ordinary inputs are filters, not permission grants, and cannot replace server authorization.

A query whose only `Session` dependency is in schema permissions, such as `@allow(query) { ownerId == Session.userId }`, remains a normal local query. The server enforces those permissions when selecting data to sync; local queries operate over that data without receiving the effective server session. See [Sync Setup](./sync.md) for the complete flow.

## Generated CRUD And Composed Operations

Pyre generates precompiled `{Record}Create`, `{Record}Update`, and `{Record}Delete` operations and typed builders for writable records. Prefer these builders for standard application CRUD:

- TypeScript: `Record.create(input)`, `Record.update(id, patch)`, and `Record.delete(id)` from generated `typescript/core/edits`.
- Elm: `Db.Edit.<Record>` constructors and field builders, submitted with `Db.Edit.submit`.

Construction captures values; it does not execute SQL, send a request, or change visible state. Explicit submission sends an ordered list of compiled query IDs and inputs. The server uses its own manifest/metadata for validation, SQL, permissions, and execution in one transaction. Clients do not send SQL or permission rules.

### Choose The Operation Boundary

| Need | API |
| --- | --- |
| Standard CRUD, including several atomic edits chosen by the UI | Generated builders + `client.submit` (TS) or `Db.Edit.submit` (Elm) |
| Custom filters, nested writes, business rules, or a custom return shape | A named `.pyre` mutation or `transaction` |
| Server-owned/seed writes that obey application permissions | Builders + `executeOperations` with an explicit session |
| Trusted initial import that bypasses query permissions and live sync | Generated `seed` helper |

Named commands remain available through `client.run` and generated `Query.*` modules. In TypeScript, `operation(commandMeta, input)` from `@pyre/client/operations` captures a named mutation for composition; import `meta as commandMeta` from its generated `core/queries/metadata/<command>` module. A named transaction can itself be an item in a composed batch. Do not substitute MCP dynamic query execution for the application's compiled submission API.

### Inputs, Identities, And Results

- Generated create inputs retain `@immutable` fields when insertable; updates omit protected fields. Omitted update fields stay unchanged, explicit `null` clears a nullable field, and a supplied value replaces it. Elm builders express nullable values with `Maybe`: omit the builder, pass `Nothing`, or pass `Just value`.
- JSON and tagged unions replace whole logical values, not nested patches or automatic merges.
- UUID create builders allocate canonical lowercase UUIDv7 once: at TS construction or Elm bridge capture before dispatch. Do not supply the primary key. Custom primary-key field names work too. Query-only non-UUID records retain their schema-derived input behavior.
- A batch targets one namespace and one database instance. The namespace evidence is a type/runtime check, not authorization.
- Every generated write must affect exactly one row; zero or multiple rows reject and roll back the whole batch. Named commands retain their existing semantics, including successful empty update/delete results.
- Success returns ordered `{ index, queryId, result }` entries with each operation's result shape. Generated TS validators and Elm receipt accessors decode them. Results describe each operation at its execution point; synced readers receive final authorized rows/removals for the transaction.
- There is no intra-batch result binding or generated-ID placeholder API. To use a newly created ID in another write, read its committed result and submit a later batch; those submissions are separate transactions.

Generated CRUD enforces schema permissions, but it does not enforce business invariants that exist only in a named command. Choosing named commands in application code is not server-enforced exclusive command ownership.

See [Sync Setup](./sync.md#7-submit-composed-writes) for TypeScript submission, optimistic state, and error handling; [Elm + Sync](./elm-sync.md#composed-writes-and-typed-receipts) for Elm wiring; and [Seeding And Server-Owned Writes](./seeding.md) for explicit-session execution without a browser.

## Validation Flow

Use:

```bash
pyre check
```

after editing query files.

MCP note:

- use `pyre_preview_query` to typecheck dynamic query text and inspect generated SQL
- use `pyre_explain_query` to validate params/session and inspect a real query plan
- use `pyre_query` to validate and execute dynamic query text without creating a query file
