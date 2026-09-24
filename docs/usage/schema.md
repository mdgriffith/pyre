# Pyre Schema Guide

Pyre schema files define the database shape that Pyre typechecks, migrates, and uses to generate query APIs. A single-schema project usually stores this in `pyre/schema.pyre`; namespaced projects use `pyre/schema/<Namespace>/schema.pyre`.

## Records

Records become database tables.

```pyre
record User {
    id   Id.Uuid @id
    name String
    @public
}
```

Common scalar types are `Int`, `Float`, `String`, `Bool`, `DateTime`, `Date`, and `JSON`. Add `?` for nullable fields, for example `deletedAt DateTime?`.

### Identity And Syncability

Namespaces are syncable by default. Every record in a synced namespace requires exactly one non-null `Id.Uuid @id` field. Its name can be `id`, `documentKey`, or another field name; the runtime uses the schema primary key, not a hardcoded `id` column. Reference identities through field types such as `User.id` so foreign keys and query parameters follow the schema.

Generated CRUD builders allocate a canonical lowercase UUIDv7 for creates; application code omits the primary key. Trusted imports and named commands can use explicit UUIDs of other versions. UUID ordering improves index locality; it is not business ordering.

For server-only/request-response databases with integer or plain-string primary keys, put `@syncable(false)` at namespace scope. Such records cannot be synchronized by the UUID-based browser worker. All records still require a primary key. See [Namespacing](./namespacing.md#sync-policy) and [Migration Guide](./migrations.md#upgrading-synced-identities-and-clients).

### Insertion Sequences

Use `Sequence.Int` for a server-assigned insertion order independent of UUID identity:

```pyre
record GameEvent {
    @public
    id       Id.Uuid @id
    sequence Sequence.Int
    payload  String
}
```

Each table in each database has an independent sequence. SQLite assigns it on insert;
generated create/update inputs and seed inputs omit it. Queries can select, sort, and
filter it as an integer. Use `Int` (or `GameEvent.sequence`) for a query parameter.
Generated result types use Rust `i64`, TypeScript `number`, and Elm `Int`.

The UUID remains the identity used by relationships, CRUD, and synchronization. Physically,
Pyre makes the sequence `INTEGER PRIMARY KEY AUTOINCREMENT` and enforces `NOT NULL` and
uniqueness on the UUID. Allocation participates in the insert transaction, including
nested and batched writes. Deleted committed sequence numbers are not reused; gaps are
allowed. Rolled-back allocations need not be retained.

Only one `Sequence.Int` field is allowed per record, alongside a non-null `Id.Uuid @id`.
The sequence must be non-nullable, cannot have `@id`, a default, or timestamp directives,
and cannot appear in sessions or structured payloads. It is immutable through Pyre write
APIs. Insert permissions cannot depend on the not-yet-assigned sequence; query, update,
and delete permissions can use the stored value. Direct SQL and explicit migrations can
still supply or change sequence values.

The authoritative database assigns sequences. Synced clients receive those values; they
do not allocate their own. Generated creates for these records have no optimistic row
prediction and become visible when the server responds.

Creating new sequence-enabled tables is supported by automatic migrations. Adding a
sequence to an existing table requires an explicit table-rebuild migration and a chosen
backfill order. Pyre reports this rather than guessing historical event order. Stored Pyre
schema metadata preserves the distinction between UUID identity and the physical primary
key when the database is reopened.

## Links

Links describe relationships between records.

```pyre
record Post {
    id       Id.Uuid @id
    authorId User.id
    author   @link(authorId, User.id)
    @public
}
```

In namespaced schemas, cross-namespace links use `Namespace.Record.field`.

```pyre
author @link(authorId, Auth.User.id)
```

## Directives

Use directives to describe table behavior and constraints.

```pyre
record Membership {
    id        Id.Uuid @id
    orgId     Int
    userId    Int
    deletedAt DateTime?

    @unique(orgId, userId)
    @index(orgId asc) where { deletedAt = null }
    @public
}
```

Useful directives include `@id`, `@default(...)`, `@immutable`, `@unique(...)`, `@index(...)`, `@singleton`, `@public`, permission directives, `@timestamps`, and `@syncable(false)`.

### Immutable Fields

Use `@immutable` for a record field that may be assigned when a row is inserted but must not be assigned by a Pyre update:

```pyre
record Document {
    id      Id.Uuid @id
    ownerId Int @immutable
    title   String
    @public
}
```

Immutable fields may use schema defaults and remain selectable in mutation results. The restriction applies to handwritten updates, transaction update steps, dynamic queries, and generated CRUD. Generated create inputs retain immutable fields when they are otherwise writable; generated update inputs omit them.

`@immutable` is valid only on record fields. For a structured record field, such as a tagged union, it protects the complete logical value, including its discriminator and payload columns.

This is a Pyre write-path invariant, not a SQLite constraint. Direct SQL, migrations, triggers, and foreign-key cascades can still change the stored columns.

## Singleton Records

Use `@singleton` when a table may contain zero or one row:

```pyre
record ApplicationSettings {
    @singleton
    @public

    id    Id.Uuid @id
    theme String
}
```

Pyre enforces the invariant with a unique SQLite index on a constant expression. The record keeps its normal ID type; `@singleton` does not create or pin an ID. A second row fails with a unique-constraint error, including when it is inserted inside a larger transaction.

## Types

Use `type` declarations for tagged unions and reusable domain values.

```pyre
type Status
   = Active
   | Inactive
   | Blocked { reason String }
```

## Tagged Unions And JSON Storage

Pyre supports two broad storage strategies for structured values:

- named `type` values used directly in records are stored in regular table columns
- `Json<T>` values are stored as a single JSON-backed column

At a high level:

- a tagged union used directly in a record is flattened into columns so Pyre can typecheck, migrate, and query it like normal structured data
- a tagged union used inside `Json<T>` stays inside one validated document value instead of expanding into multiple columns
- raw `JSON` is the untyped escape hatch when you do not want Pyre to validate the shape

That means the same logical type can have two different persistence strategies depending on where it is used:

- as a record field: expanded into columns
- inside `Json<T>`: stored as one validated JSON value

For the exact persisted representation, discriminator layout, and migration implications, see [Tagged Union And JSON Storage](../spec/tagged-union-storage.md).

## Sessions

Session definitions describe trusted values supplied by the server from its authenticated request context for permissions and server queries. Define the single shared session in `pyre/session.pyre`; it is available to every schema namespace.

```pyre
session {
    userId User.id
}
```

The browser client does not hold the effective Pyre session. Using `Session` only in schema permissions does not prevent local queries: the server enforces permissions when selecting synced data. For explicit local query filters, see [Local Queries And Session](./query.md#local-queries-and-session); for server and client setup, see [Sync Setup](./sync.md).

## Permissions

Permissions filter operations using fields from the current row and trusted,
typed session values:

```pyre
record Post {
    @allow(query) {
        Or(
            published == True,
            authorId == Session.userId,
        )
    }
    @allow(insert, update, delete) { authorId == Session.userId }

    id Id.Uuid @id
    authorId User.id
    published Bool
}
```

Fine-grained permissions must explicitly cover `query`, `insert`, `update`, and
`delete`. Use `True` to leave an operation unrestricted or `False` to deny it:

```pyre
@allow(query) { True }
@allow(insert, update, delete) { False }
```

Use `@allow(*) { False }` to deny every operation on a record.

Use `exists` to authorize through declared links. The path starts at the
protected record, and unqualified fields inside the block belong to the final
linked record:

```pyre
type WorkspaceRole
    = Admin
    | Member
    | Guest

record Document {
    @allow(query) {
        exists workspace.members {
            And(
                userId == Session.userId,
                Or(
                    role == Admin,
                    role == Member,
                ),
            )
        }
    }
    @allow(insert, update, delete) { False }

    id Id.Uuid @id
    workspaceId Workspace.id
    workspace @link(workspaceId, Workspace.id)
}
```

`exists workspace.members` is one multi-hop expression: `Document` to
`Workspace` to `WorkspaceMember`. An `exists` expression inside another
`exists` block is not currently supported.

Put the relational-permission example in a namespace declared with `@syncable(false)` and provide the referenced `Workspace`/membership records. Current relational-permission boundaries:

- relational query permissions require a query-only namespace (`@syncable(false)`)
- relational insert permissions are not supported
- updates cannot modify columns used by the first link in the path
- paths cannot cross namespaces
- linked record permissions are not applied implicitly inside the block
- `exists` is permission-only and cannot be used in a query `@where`

## Validation Flow

After editing schema files, run:

```bash
pyre check
```

MCP note:

- use `pyre_check` to typecheck a project through MCP
- use `pyre_init` to create a new project from schema source through MCP
