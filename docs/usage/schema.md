# Pyre Schema Guide

Pyre schema files define the database shape that Pyre typechecks, migrates, and uses to generate query APIs. A single-schema project usually stores this in `pyre/schema.pyre`; namespaced projects use `pyre/schema/<Namespace>/schema.pyre`.

## Records

Records become database tables.

```pyre
record User {
    id   Int    @id
    name String
    @public
}
```

Common scalar types are `Int`, `Float`, `String`, `Bool`, `DateTime`, `Date`, and `JSON`. Add `?` for nullable fields, for example `deletedAt DateTime?`.

## Links

Links describe relationships between records.

```pyre
record Post {
    id       Int @id
    authorId Int
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
    id        Int @id
    orgId     Int
    userId    Int
    deletedAt DateTime?

    @unique(orgId, userId)
    @index(orgId asc) where { deletedAt = null }
    @public
}
```

Useful directives include `@id`, `@default(...)`, `@unique(...)`, `@index(...)`, `@public`, permission directives, `@timestamps`, and `@syncable(false)`.

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

Session definitions describe values supplied by the application runtime, often for authorization-aware queries. Define the single shared session in `pyre/session.pyre`; it is available to every schema namespace.

```pyre
session {
    userId Int
}
```

## Validation Flow

After editing schema files, run:

```bash
pyre check
```

MCP note:

- use `pyre_check` to typecheck a project through MCP
- use `pyre_init` to create a new project from schema source through MCP
