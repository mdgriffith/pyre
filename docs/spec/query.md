# Pyre Query Language Specification

## Overview

Pyre query files define database operations: `query` (select), `insert`, `update`, and `delete`. Query files use the `.pyre` extension and are typically named `query.pyre` or `queries.pyre`, or organized in a `queries/` directory.

Pyre can also provide schema-derived built-in CRUD mutations for writable tables. See the [Generated CRUD Mutations Specification](./generated-crud-mutations.md).

## Syntax Rules

- **Indentation**: Query definitions must start at column 1 (beginning of line).
- **Comments**: Single-line comments using `//` are supported.
- **Whitespace**: Blank lines are allowed between queries.

## Query Operations

### Query (Select)

Selects data from records.

**Basic Syntax:**
```pyre
query QueryName {
    recordName {
        field1
        field2
    }
}
```

**With Parameters:**
```pyre
query GetUser($id: Int) {
    user {
        @where { id == $id }
        id
        name
    }
}
```

**With Nested Fields:**
```pyre
query GetPost($id: Int) {
    post {
        @where { id = $id }
        id
        title
        author {
            id
            name
            email
        }
    }
}
```

**Field Aliases:**
```pyre
query GetUsers {
    user {
        id
        username: name
        emailAddress: email
    }
}
```

**Multiple Root Fields:**
```pyre
query GetData {
    user {
        id
        name
    }
    post {
        id
        title
    }
}
```

### Select All Scalar Fields (`*`)

Use `*` to select all scalar fields for a record. This excludes relationship fields by default.

```pyre
query ListTasks {
    task {
        *
    }
}
```

**Notes**
- `*` expands to all scalar columns (non-relationship fields).
- Relationship traversal still requires explicit nested selection blocks.

### Insert

Inserts new records.

**Basic Syntax:**
```pyre
insert CreateUser($name: String, $email: String) {
    user {
        name = $name
        email = $email
    }
}
```

**With Session Variables:**
```pyre
insert CreatePost($title: String, $content: String) {
    post {
        authorUserId = Session.userId
        title = $title
        content = $content
        published = False
    }
}
```

**With Nested Inserts:**
```pyre
insert CreateUserWithPosts($name: String) {
    user {
        name = $name
        posts {
            title = "First Post"
            content = "Content here"
        }
    }
}
```

**With Union Type Values:**
```pyre
// Simple variant
insert CreateRecord($name: String) {
    record {
        name = $name
        status = Active
    }
}

// Variant with fields
insert CreateRecord($name: String, $reason: String) {
    record {
        name = $name
        status = Pending { reason = $reason }
    }
}
```

### Update

Updates existing records.

**Basic Syntax:**
```pyre
update UpdateUser($id: Int, $name: String?) {
    user {
        @where { id == $id }
        name = $name
    }
}
```

**Multiple Fields:**
```pyre
update UpdatePost($id: Int, $title: String?, $content: String?, $published: Bool?) {
    post {
        @where { id == $id }
        title = $title
        content = $content
        published = $published
    }
}
```

**Note**: Nullable parameters (`String?`) allow omitting fields in updates. Non-nullable parameters require values.

### Delete

Deletes records.

**Basic Syntax:**
```pyre
delete DeleteUser($id: Int) {
    user {
        @where { id == $id }
        id
    }
}
```

**Note**: Delete queries must include at least one field in the selection (typically `id`) for the return value.

## Parameters

Parameters are declared in the query signature:

```pyre
query QueryName($param1: Type, $param2: Type?) {
    // ...
}
```

**Supported Parameter Types:**
- `Int`
- `Float`
- `String`
- `Bool`
- `DateTime`
- `Date`
- Custom types (tagged unions)
- Schema-derived types (see "Schema-Derived Query Inputs")

**Nullable Parameters:**
Append `?` to make parameters optional:
```pyre
update UpdateUser($id: Int, $name: String?) {
    // $name can be omitted
}
```

## Field Selection

### Simple Fields

```pyre
id
name
email
createdAt
```

### Nested Fields (Links)

```pyre
author {
    id
    name
}
```

### Field Aliases

```pyre
username: name
emailAddress: email
myAuthor: author {
    id
    name
}
```

## Query Arguments

### @where

Filters records using conditions.

**Basic Syntax:**
```pyre
@where { field == value }
```

**With Variables:**
```pyre
@where { id == $id }
```

**With Session Variables:**
```pyre
@where { authorId == Session.userId }
```

**Multiple Conditions (AND):**
```pyre
@where { 
    published == True
    authorId == Session.userId
}
```

**OR Conditions:**
```pyre
@where { 
    status == "active" || status == "pending"
}
```

**Complex Conditions:**
```pyre
@where {
    (authorId == Session.userId || Session.role == "admin") &&
    published == True
}
```

**Note**: Multiple `@where` clauses are combined with AND. Use `||` within a single `@where` for OR conditions.

**Note**: `@where(Null)` means no conditions (equivalent to omitting `@where`).

### Tagged Union Predicates

Use a variant-qualified path to filter on fields inside a tagged union payload:

```pyre
query RetryablePayments($errorCode: String) {
    payment {
        @where {
            state.Failed.errorCode == $errorCode &&
            state.Failed.retryable == True
        }

        id
        state
    }
}
```

Each variant segment narrows and guards the rest of the path. For example,
`state.Failed.errorCode == $errorCode` only matches when `state` is `Failed`;
payload data from any inactive variant is ignored. Nested unions repeat the same
field/variant pattern:

```pyre
@where {
    state.Failed.reason.ProviderRejected.code == $code
}
```

Payload fields must always be variant-qualified, even when multiple variants
declare a field with the same name and type. Write the variants explicitly when
more than one should match:

```pyre
@where {
    state.Pending.externalId == $externalId ||
    state.Failed.externalId == $externalId
}
```

Discriminator comparisons and membership checks remain available directly on
the union field:

```pyre
@where {
    state == Failed
}

@where {
    state in [Pending, Failed]
}
```

Variant guards are retained for every comparison operator. In particular,
`state.Failed.errorCode != "declined"` matches only `Failed` values with a
different error code. Unary negation such as
`!(state.Failed.errorCode == "declined")` is not supported.

### @sort

Orders results.

**Ascending:**
```pyre
@sort(name, Asc)
```

**Descending:**
```pyre
@sort(createdAt, Desc)
```

**Multiple Sorts:**
```pyre
@sort(createdAt, Desc)
@sort(name, Asc)
```

### @limit

Limits the number of results.

```pyre
@limit(10)
@limit($limitValue)
```

### @if

Conditionally includes a field or nested block based on a boolean parameter.

```pyre
query TaskGetWithRelations($id: Task.id, $includeSubtasks: Bool) {
    task {
        @where { id == $id }
        id
        description
        subtasks @if($includeSubtasks) {
            id
            description
            status
            priority
            createdAt
        }
    }
}
```

Here is an example querying a `Task` table.

**Notes**
- If the condition is false, the field is omitted from the result.
- `@if` can be applied to scalar fields or nested selections.

## Where Clause Operators

**Comparison Operators:**
- `==` - Equal (also accepts `=` for backward compatibility, but writes `==`)
- `!=` - Not equal
- `>` - Greater than
- `<` - Less than
- `>=` - Greater than or equal
- `<=` - Less than or equal
- `in` - In array (e.g., `id in [1, 2, 3]`)

**Logical Operators:**
- `&&` - AND
- `||` - OR

**Note**: Parentheses can be used for grouping: `(a == 1 || a == 2) && b == 3`

## Values

### Literals

**Strings:**
```pyre
name = "John"
title = "My Post"
```

**Integers:**
```pyre
count = 42
id = 1
```

**Floats:**
```pyre
price = 19.99
ratio = 0.5
```

**Booleans:**
```pyre
published = True
active = False
```

**Null:**
```pyre
name = Null
```

### Variables

**Query Parameters:**
```pyre
name = $name
id = $id
```

**Session Variables:**
```pyre
authorId = Session.userId
role = Session.role
```

### Type Values

**Simple Variants:**
```pyre
status = Active
action = Delete
```

**Variants with Fields:**
```pyre
status = Pending { 
    reason = $reason 
}

action = Create { 
    name = $name
    description = $description
}
```

**Note**: All fields of a variant must be provided when using variants with fields.

### Functions

SQLite functions can be used in values:

```pyre
// String functions
name = upper($name)
substring = substr($text, 0, 10)

// Math functions
maxValue = max($a, $b)
rounded = round($value)

// Date functions
dateStr = date("now")
```

**Common Functions:**
- String: `upper`, `lower`, `length`, `substr`, `trim`, `replace`
- Math: `max`, `min`, `abs`, `round`, `floor`, `ceil`
- Date: `date`, `time`, `datetime`, `strftime`

Draft and postponed features live in `docs/spec/query.draft.md`.

## Examples

### Complete Query File

```pyre
// Get a single user
query GetUser($id: Int) {
    user {
        @where { id == $id }
        id
        name
        email
        createdAt
    }
}

// List users with sorting
query ListUsers {
    user {
        @sort(createdAt, Desc)
        id
        name
        email
    }
}

// Get post with author
query GetPost($id: Int) {
    post {
        @where { id == $id }
        id
        title
        content
        published
        createdAt
        author {
            id
            name
            email
        }
    }
}

// Create user
insert CreateUser($name: String, $email: String) {
    user {
        name = $name
        email = $email
    }
}

// Create post with author from session
insert CreatePost($title: String, $content: String) {
    post {
        authorUserId = Session.userId
        title = $title
        content = $content
        published = False
    }
}

// Update post
update UpdatePost($id: Int, $title: String?, $content: String?) {
    post {
        @where { id == $id }
        title = $title
        content = $content
    }
}

// Delete post
delete DeletePost($id: Int) {
    post {
        @where { id == $id }
        id
    }
}

// Complex query with filters
query GetPublishedPosts($limit: Int) {
    post {
        @where { 
            published == True &&
            (authorId == Session.userId || Session.role == "admin")
        }
        @sort(createdAt, Desc)
        @limit($limit)
        id
        title
        content
        author {
            id
            name
        }
    }
}
```

## Generated Elm Mutation Bridge Contract

When Pyre generates Elm client modules for mutations (`insert`, `update`, `delete`), each `Query.*` mutation module must expose enough metadata for the runtime bridge to execute the mutation without handwritten host logic.

Each generated mutation module must expose:

- `id : String` using the mutation interface hash
- `name : String` using the mutation name from the `.pyre` definition
- `mutationRequest : String -> String -> Input -> Encode.Value`
- `decodeMutationResult : Decode.Decoder MutationResult`

`mutationRequest databaseId requestId input` must encode this payload shape:

```json
{
  "type": "mutate",
  "databaseId": "main",
  "requestId": "client-generated-request-id",
  "mutationId": "stable-mutation-interface-hash",
  "mutationName": "CreatePost",
  "mutationInput": {
    "title": "Hello"
  }
}
```

Rules:

- `requestId` is caller-owned and is used only for client-side bookkeeping
- `databaseId` is server-defined and routes the mutation to the correct source database
- `mutationId` is the stable server mutation identity and is the value `PyreClient` uses to construct the server request
- `mutationName` is descriptive metadata for debugging and Elm-side routing; it is not the execution key
- `mutationInput` is the normal generated Elm encoding for the mutation input record

When `PyreClient` receives a `mutate` bridge message with that shape, it must be able to execute the mutation directly against the configured query endpoint without requiring an application-defined `onMutation` handler.

The bridge result payload must preserve `requestId` so Elm can correlate request lifecycle state:

```json
{
  "type": "mutation-result",
  "requestId": "client-generated-request-id",
  "mutationId": "stable-mutation-interface-hash",
  "mutationName": "CreatePost",
  "result": {
    "ok": true,
    "value": {
      "post": [
        { "id": 1, "title": "Hello" }
      ]
    }
  }
}
```

The mutation result is an acknowledgement channel, not the primary read path. Authoritative client reads continue to arrive through sync catchup/live deltas.

## Unexpected Behaviors

1. **Field selection in deletes**: Delete queries require at least one field selection (typically `id`) even though the record is being deleted.

2. **Multiple @where clauses**: Multiple `@where` directives are combined with AND, not OR. Use `||` within a single `@where` for OR conditions.

3. **Nullable parameters**: In updates, nullable parameters (`String?`) allow omitting fields. Non-nullable parameters must be provided.

4. **Nested inserts**: When inserting nested records, the parent record must exist or be created in the same operation. Foreign key constraints apply.

5. **Union variant fields**: When using union variants with fields, all fields must be provided. Partial field assignment is not supported.

6. **Session variable access**: Session variables are accessed via `Session.fieldName`, not `$Session.fieldName` or other syntax.

7. **Function arguments**: SQLite functions accept specific types. Type mismatches will cause errors at query execution time, not parse time.

8. **Sort order**: Multiple `@sort` directives are applied in order (first sort is primary, subsequent sorts are secondary, etc.).

9. **Limit placement**: `@limit` can appear anywhere in the field list, but typically appears after `@where` and `@sort`.

10. **Field aliases**: Aliases only affect the output structure, not the query logic. You cannot use aliases in `@where` clauses.
