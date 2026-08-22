# Pyre Query Guide

Pyre query files define typed database operations against a Pyre schema. Query files usually live in the same `pyre/` tree as schema files. Any non-schema `.pyre` file under that tree is treated as a query file. A common convention is `pyre/query.pyre`.

## Select Queries

Use `query` to read records and shape the returned data.

```pyre
query GetUser($id: Int) {
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
insert CreateUser($name: String) {
    user {
        name = $name
    }
}
```

```pyre
update RenameUser($id: Int, $name: String) {
    user {
        @where { id == $id }
        name = $name
    }
}
```

An update cannot assign a record field marked `@immutable`, even when the assignment would write the existing value. Immutable fields may still be selected in the mutation result. The same rule applies to update steps inside `transaction` blocks and to dynamic queries submitted through MCP.

```pyre
delete DeleteUser($id: Int) {
    user {
        @where { id == $id }
    }
}
```

## Transaction Blocks

Use `transaction` to group ordered `insert`, `update`, and `delete` steps into one atomic operation. All steps commit together. If a constraint, SQL, or connection error occurs, Pyre rolls back every step.

```pyre
transaction ReplaceNote($id: Note.id, $body: String) {
    update changed: note {
        @where { id == $id }
        body = $body
        id
    }

    insert created: note {
        body = $body
        id
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
  "changed": [{ "id": 1, "body": "new body" }],
  "created": [{ "id": 2, "body": "new body" }],
  "removed": [{ "id": 1 }]
}
```

Use unique, descriptive aliases when multiple steps target the same record. Without an alias, the record field name is used as the result key, and duplicate result keys are rejected.

An update or delete that matches no permitted rows returns `[]`. That is not an error and does not stop later steps. If later writes must depend on an earlier step matching a row, express that requirement as a database constraint or redesign the operation; transaction steps cannot reference IDs or rows returned by earlier steps.

Nested inserts currently require temporary tables. They work with local SQLite and embedded libSQL, but are rejected by the native remote-libSQL runtime. Flat transaction blocks are remote-compatible, and TypeScript runtimes submit their statements in one atomic `db.batch(...)` call.

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

Session values can also participate in query conditions:

```pyre
query MyNotes {
    note {
        @where { ownerId == Session.userId }
        id
        body
    }
}
```

## Generated CRUD

Pyre can expose schema-derived CRUD mutations for writable tables. Generated create inputs retain `@immutable` fields when they are otherwise insertable, while generated update inputs omit them.

Use handwritten queries when you need custom filters, nested writes, business rules, or a response shape that differs from the default generated operation. Handwritten updates remain subject to `@immutable` checking.

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
