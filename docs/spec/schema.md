# Pyre Schema Language Specification

## Overview

Pyre schema files define database structure using records (tables), types (tagged unions), and sessions. Schema files use the `.pyre` extension and are typically named `schema.pyre` or organized in a `schema/` directory.

## Syntax Rules

- **Indentation**: Top-level definitions (`record`, `type`, `session`) must start at column 1 (beginning of line). Indentation is not allowed.
- **Comments**: Single-line comments using `//` are supported.
- **Whitespace**: Blank lines are allowed between definitions.

## Records

Records define database tables. Each record maps to a SQLite table.

### Basic Syntax

```pyre
record RecordName {
    fieldName Type
    fieldName Type?
}
```

### Field Types

Field types are type expressions. Pyre supports primitive types, named types, generic document types, and nullable types.

**Primitive Types:**
- `Int` - Integer (stored as INTEGER in SQLite)
- `Float` - Floating point number (stored as REAL)
- `String` - Text (stored as TEXT)
- `Bool` - Boolean (stored as INTEGER, 0 or 1)
- `DateTime` - Timestamp (stored as INTEGER, Unix epoch)
- `Date` - Date (stored as TEXT)
- `JSON` - Untyped raw JSON data (stored as BLOB)

**Named Types:**
Reference `type` declarations defined elsewhere:
```pyre
status Status
action Action
```

Named types currently refer to tagged union `type` declarations.

**JSON Document Types:**
Use `Json<T>` to store a validated value in a single JSON-backed column.
```pyre
status Json<Status>
tags Json<List<String>>
metadata Json<Dict<String>>
```

`Json<T>` stores a single structured value in one column and validates its shape before writes. Pyre does not re-validate `Json<T>` values on reads. `T` may be any JSON-encodable type.

Pyre stores `Json<T>` values in SQLite as `BLOB`, using SQLite's JSONB representation for persisted document data.

`JSON` remains available as an escape hatch for untyped raw JSON values. Unlike `Json<T>`, raw `JSON` does not guarantee a validated schema shape.

**Container Types:**
- `List<T>` - Ordered JSON array of values of type `T`
- `Dict<T>` - JSON object with string keys and values of type `T`

Container types are document-only. They may only appear inside `Json<...>`.

Examples:
```pyre
tags Json<List<String>>
optionalNotes Json<List<String?>>
settings Json<Dict<String>>
```

These are invalid because `List<T>` and `Dict<T>` are not expandable:
```pyre
tags List<String>
settings Dict<String>
```

If a named type contains `List<T>` or `Dict<T>` anywhere inside it, that type is also document-only and must be wrapped in `Json<...>` when used in a record field.

**JSON-Encodable Types:**
All value types are JSON-encodable except relationships.

This means `Json<...>` may contain:
- Primitive types
- Nullable types
- Named types
- `List<T>` and `Dict<T>`
- Branded IDs such as `User.id`

This means `Json<...>` may not contain:
- `@link` relationship fields
- Any type that includes relationships transitively

Branded IDs used inside `Json<...>` are treated as values only. They do not create foreign keys, links, or indexes.

**Nullable Types:**
Append `?` to any type expression to make it nullable:
```pyre
name String?
email String?
status Json<Status>?
tags Json<List<String?>>
```

`?` applies to the immediately preceding type expression. Use parentheses if needed to make the intended grouping explicit.

**ID Types:**
Branded type-safe identifiers that prevent mixing IDs from different tables:
```pyre
id Id.Int @id         -- Integer primary key (branded as TableId)
externalId Id.Uuid    -- UUID identifier (branded as Uuid Table)
```

ID types are stored in the database using their underlying representation (`INTEGER` for `Id.Int`, `TEXT` for `Id.Uuid`), but the generated client code uses branded types to prevent accidental misuse. For example, a `UserId` cannot be passed where a `PostId` is expected.

**Foreign Key Field References:**
Reference the ID field of another table to get the correct branded type:
```pyre
record User {
    id Id.Int @id
    name String
}

record Post {
    id Id.Int @id
    authorId User.id     -- Has type UserId, references User.id
    title String
}
```

The syntax `TableName.fieldName` creates a foreign key that uses the same branded ID type as the referenced field. This ensures type safety when working with relationships.

### Column Directives

Directives modify column behavior:

**`@id`** - Primary key
```pyre
id Int @id
```

**`@unique`** - Unique constraint
```pyre
email String @unique
```

Column-level `@unique` is equivalent to a single-column unique index with ascending order.

**`@index`** - Create an index
```pyre
createdAt DateTime @index
```

Column-level `@index` is equivalent to a single-column non-unique index with ascending order.

**`@default(value)`** - Default value
```pyre
// String literal
name String @default("Unknown")

// Numeric literal
count Int @default(0)

// Boolean literal
published Bool @default(False)

// DateTime now
createdAt DateTime @default(now)
```

**Note**: Multiple directives can be applied to a single field:
```pyre
createdAt DateTime @default(now) @index
```

`@default(...)` is not allowed on `Json<...>` fields or raw `JSON` fields.

### Record-Level Directives

These directives apply to the entire record:

**`@unique(...)`** - Table-level unique index (1+ columns)
```pyre
record Membership {
    id     Int @id
    orgId  Int
    userId Int

    @unique(orgId, userId)
    @public
}
```

**`@index(...)`** - Table-level index (1+ columns)
```pyre
record Event {
    id        Int @id
    orgId     Int
    createdAt DateTime
    deletedAt DateTime?

    @index(orgId asc, createdAt desc) where {
        deletedAt = null
    }
    @public
}
```

`@index(...)` column syntax supports optional sort direction per column:
- `fieldName` (defaults to `asc`)
- `fieldName asc`
- `fieldName desc`

Partial indexes use an optional `where { ... }` clause.

Validation rules for table-level `@index` / `@unique`:
- Referenced fields must exist on the same record.
- Duplicate field names inside a single directive are not allowed.
- At least one field is required.

For partial index predicates:
- Session references are not allowed (for example `Session.userId`).
- Query variables/functions are not allowed in predicate values.
- Use literal values (for example `null`, strings, numbers, booleans).

**`@tablename("name")`** - Override default table name
```pyre
record User {
    @tablename("users")
    id Int @id
    name String
}
```

By default, table names are the pluralized, decapitalized record name (e.g., `User` → `users`).

**`@public`** - Make all operations public (no permission checks)
```pyre
record Post {
    @public
    id Int @id
    title String
}
```

**`@allow(operations) { conditions }`** - Permission rules
```pyre
record Post {
    // Single operation
    @allow(query) { published == True }
    
    // Multiple operations
    @allow(insert, update) { authorId == Session.userId }
    
    // All operations
    @allow(*) { authorId == Session.userId }
    
    // Complex conditions
    @allow(delete) { 
        authorId == Session.userId || Session.role == "admin" 
    }
}
```

**Operations**: `query`, `insert`, `update`, `delete`, or `*` for all.

**Conditions**: Use `Session.fieldName` to reference session variables. Supported operators: `==` (equal), `&&` (and), `||` (or).

Permissions may test related records through declared links with `exists`:

```pyre
record Document {
    @allow(query) {
        exists workspace.members {
            userId == Session.userId
            role == Admin || role == Member
        }
    }

    workspaceId Workspace.id
    workspace @link(workspaceId, Workspace.id)
}
```

The path starts at the protected record, every path segment must be a declared
link, and unqualified fields in the block belong to the final linked record.
Linked record permissions are not applied implicitly. Nested `exists`
expressions, cross-namespace paths, and relational insert permissions are not
currently supported. Updates may not modify columns used by the first link in
an `exists` path; relational update policies currently authorize the existing
row rather than a proposed relationship change.

Relational query permissions are currently limited to query-only namespaces
(`@syncable(false)`). Synced namespaces require dependency invalidation and
client-side revocation before related-table changes can be synchronized safely.

**`@watch`** - Enable change watching for inserts
```pyre
record Post {
    @watch
    id Int @id
    title String
}
```

### Links

Links define relationships between records. They appear as field-level directives.

**Syntax:**
```pyre
record Post {
    authorId Int
    author @link(authorId, User.id)
}
```

Both the local field name and the foreign table's primary key must be explicitly specified.

**Multi-field links** (for composite keys):
```pyre
record Post {
    authorId Int
    authorType String
    author @link(authorId, authorType, User.id, User.type)
}
```

**Note**: Links are bidirectional. Defining a link on one record automatically creates a reverse link on the referenced record.

## Types (Tagged Unions)

Types define custom union types (sum types) similar to Rust enums or TypeScript discriminated unions.

### Basic Syntax

```pyre
type TypeName
   = Variant1
   | Variant2
   | Variant3
```

**Note**: The `=` and `|` separators can be on separate lines. Leading whitespace before variants is allowed.

### Variants with Fields

Variants can contain fields:

```pyre
type Status
   = Active
   | Inactive
   | Pending {
        reason String
        createdAt DateTime
    }
```

### Multiple Fields

```pyre
type Action
   = Create {
        name String
        description String
    }
   | Update {
        id Int
        changes String
    }
   | Delete {
        id Int
        reason String
    }
```

**Note**: Variants without fields are simple tags. Variants with fields use `{ }` syntax.

Tagged union types may also be used inside `Json<...>`. In that case, the full tagged value is stored as validated JSON in a single column instead of expanding into columns.

The canonical JSON representation of a tagged union uses a `type_` field as the discriminator.

For example:
```json
{ "type_": "Pending", "reason": "manual review", "createdAt": 1712966400 }
```

Query and mutation inputs for `Json<...>` fields use the same logical payload shape as the corresponding named type outside `Json<...>`. The storage representation is an implementation detail.

Generated TypeScript and Elm clients surface `Json<T>` fields as structured client types. For example, `Json<List<String>>` becomes `Array<string>` in TypeScript query inputs and `List String` in generated Elm query modules.

## Session

Sessions define runtime context variables available in queries and permissions.

### Syntax

```pyre
session {
    userId Int
    role String
    organizationId Int?
}
```

Session fields can be nullable using `?`.

**Usage in permissions:**
```pyre
@allow(query) { userId = Session.userId }
@allow(delete) { role = Session.role || Session.role = "admin" }
```

**Usage in queries:**
```pyre
insert CreatePost($title: String) {
    post {
        authorId = Session.userId
        title = $title
    }
}
```

## Examples

### Complete Schema

```pyre
session {
    userId Int
    role String
}

type Status
   = Active
   | Inactive
   | Pending {
        reason String
    }

record User {
    @tablename("users")
    @public
    
    id Id.Int @id
    name String?
    email String? @unique
    status Status
    createdAt DateTime @default(now)
    
    posts @link(Post.authorId)
    accounts @link(Account.userId)
}

record Post {
    @watch
    @allow(query) { published = True }
    @allow(insert, update, delete) { authorId = Session.userId }
    
    id Id.Int @id
    createdAt DateTime @default(now)
    authorId User.id
    title String
    content String
    published Bool @default(False)
    
    users @link(authorId, User.id)
}
```

## Unexpected Behaviors

1. **Table name pluralization**: Record names are automatically pluralized and decapitalized for table names. `User` becomes `users`, `Person` becomes `people` (if pluralization rules apply).

2. **Link direction**: Links are always bidirectional. Defining `@link` on one record creates a reverse link automatically.

3. **Indentation strictness**: Top-level definitions cannot be indented. They must start at column 1.

4. **Nullable syntax**: The `?` must come immediately after the type expression, before any directives: `name String? @unique`, `settings Json<Preferences>?`, not `name String @unique?`.

5. **Default values**: Only specific literal types are supported in `@default()`. Complex expressions are not allowed.

6. **Permission conditions**: Only `Session.*` variables can be referenced in permission conditions. Record fields cannot be compared directly in permissions.

7. **Type variants**: Variants with fields require all fields to be provided when used in queries. There's no partial field support.

8. **Document-only container types**: `List<T>` and `Dict<T>` may only appear inside `Json<...>`. Any named type containing either container transitively is also document-only.

9. **Comments**: Only single-line `//` comments are supported. Multi-line comments are not available.

10. **Composite index order matters**: For `@index(a, b, c)`, SQLite can use leftmost prefixes (`a`, `a+b`, `a+b+c`) efficiently. Put the most selective leading conditions first.

11. **Unique + nullable columns**: SQLite allows multiple `NULL` values in unique indexes. If strict uniqueness is required, make indexed columns non-nullable.
