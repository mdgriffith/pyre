# Tagged Union And JSON Storage

This document describes how Pyre persists named tagged unions, `Json<T>` values, and raw `JSON` values.

The integer-key examples below illustrate storage in a query-only namespace (`@syncable(false)`). Synced schemas instead require non-null UUID primary keys. Generated edits replace an entire logical JSON/union value; these storage layouts do not imply nested-patch or concurrent field-merge semantics. See the [Query Guide](../usage/query.md#generated-crud-and-composed-operations).

## Scope

This spec covers:

- named `type` declarations used directly in record fields
- tagged unions nested inside `Json<T>`
- raw `JSON` fields

It does not redefine the surface schema syntax. See [schema.md](./schema.md) for the language-level schema reference.

## High-Level Model

Pyre uses two different persistence strategies for structured values:

1. **Expanded structured storage** for named types used directly in record fields.
2. **Single-column document storage** for `Json<T>` and raw `JSON` values.

The main design goal is:

- use columns when Pyre should reason about the structure relationally
- use one JSON-backed column when Pyre should preserve a document as one value

## Direct Tagged Union Fields

When a named tagged union is used directly in a record field, Pyre stores it as expanded columns rather than as one JSON blob.

Example schema:

```pyre
type Status
   = Active
   | Inactive
   | Blocked { reason String }

record User {
    id Int @id
    status Status
}
```

Conceptually, persistence needs:

- a discriminator column for the active variant
- additional columns for any variant fields

For the example above, the storage shape is conceptually like:

```text
status_type
status_reason
```

where:

- `status_type` stores the active variant name
- `status_reason` is populated only for `Blocked { reason ... }`

Consequences:

- migrations can add or remove columns when variant fields change
- Pyre can reason about the field structurally during schema diffing and generation
- the relational layout is an implementation detail; user-facing query and mutation syntax still uses the logical type

## `Json<T>` Storage

`Json<T>` stores one validated structured document in a single column.

Example:

```pyre
type Status
   = Active
   | Inactive
   | Blocked { reason String }

record User {
    id Int @id
    profile Json<Status>
}
```

In this case, `profile` is not expanded into relational columns. The full value is encoded into one persisted JSON-backed value.

Pyre validates the payload shape before writes.

Pyre stores persisted `Json<T>` data in SQLite using a single `BLOB` column and SQLite's JSONB representation.

Consequences:

- schema changes inside `Json<T>` do not map to expanded relational columns
- migrations see one stored document column, not one column per nested field
- query and mutation inputs still use the logical typed JSON shape

## Raw `JSON` Storage

Raw `JSON` is the untyped escape hatch.

Example:

```pyre
record Event {
    id Int @id
    payload JSON
}
```

Unlike `Json<T>`:

- Pyre does not validate the nested shape against a named schema type
- the value is treated as untyped document data

Like `Json<T>`, it is stored as a single JSON-backed persisted value rather than expanded into multiple relational columns.

## Tagged Representation Inside JSON

When a tagged union is serialized inside `Json<T>`, the canonical logical JSON representation uses a discriminator field named `type_`.

Example logical payloads:

```json
{ "type_": "Active" }
```

```json
{ "type_": "Blocked", "reason": "spam" }
```

This is the shape callers should use at the JSON boundary for typed document values.

## Nullability

Nullability is controlled by the containing field type.

Examples:

- `status Status` means the structured value is required
- `status Status?` means the structured value may be absent at the field level
- `profile Json<Status>` means the document column is required
- `profile Json<Status>?` means the document column itself may be `null`

For expanded tagged unions, nullable field behavior applies to the outer field, not to individual variant field columns as an authoring concept.

## Migration Implications

### Direct tagged union fields

Changing a direct tagged union can affect relational storage shape.

Examples:

- adding a new variant field may require a new column
- removing a variant field may remove a column
- renaming or restructuring variants may require schema changes and data migration handling

### `Json<T>` fields

Changing the shape of `Json<T>` does not expand into new relational columns.

However:

- application-level expectations still change
- existing stored documents may no longer match the new intended shape unless you migrate the data separately
- Pyre validates writes, but does not retroactively rewrite persisted documents for you

## Query And Mutation Boundary

Pyre's query and mutation surfaces use the logical type shape, not the low-level storage layout.

That means:

- direct tagged unions are authored as typed values, not manual discriminator/column sets
- `Json<T>` fields are authored as typed JSON payloads
- callers should not depend on the exact internal relational column names used for expanded storage unless a lower-level storage spec explicitly guarantees them

## Compatibility Guidance

Users should treat the exact low-level storage representation as an implementation detail unless Pyre explicitly documents a stable compatibility guarantee for a particular layout.

The stable mental model is:

- direct named types become relational structure
- `Json<T>` and raw `JSON` become single persisted document values
- tagged values inside JSON use `type_` as the discriminator
