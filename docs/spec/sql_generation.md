# SQL Generation Specification

## Overview

Pyre generates SQL that directly produces the final JSON response shape, similar to GraphQL with automatically written resolvers. The SQL itself composes the final JSON structure, eliminating the need for post-processing in application code.

## Queries

### Core Principle

A query generates SQL that returns data in exactly the shape specified by the query definition. The SQL uses JSON aggregation functions to construct the response structure directly.

### Example

Given a query:
```pyre
query ListPosts {
    post {
        id
        title
        content
        authorUserId
        users {
            id
            name
            email
        }
    }
}
```

The generated SQL should produce a result set with columns named after the top-level query fields, containing JSON that matches the query shape:

```json
{
  "post": [
    {
      "id": 1,
      "title": "Hello",
      "content": "World",
      "authorUserId": 5,
      "users": [
        {
          "id": 5,
          "name": "Alice",
          "email": "alice@example.com"
        }
      ]
    }
  ]
}
```

### SQL Structure

- Uses `json_object()` and `json_group_array()` to build nested structures
- Uses CTEs (Common Table Expressions) to organize complex queries
- Uses joins to fetch related data

### Return Format

The database runner receives result sets with:
- Column names matching the top-level query field names (e.g., `post`, `user`)
- Values: JSON strings containing JSON arrays (e.g., `[{...}, {...}]`) for each field
- For queries with multiple top-level fields, each field gets its own column

The application code parses these JSON values and constructs the response object with keys matching the column names, then returns it directly to the client.

### Multiple Top-Level Fields Example

For queries with multiple top-level fields:
```pyre
query GetPostsAndUsers {
    post {
        id
        title
    }
    user {
        id
        name
    }
}
```

The SQL generates multiple columns, one per top-level field:
- Column: `post` - JSON array of posts
- Column: `user` - JSON array of users

The response structure matches:
```json
{
  "post": [{"id": 1, "title": "Hello"}],
  "user": [{"id": 5, "name": "Alice"}]
}
```

## Mutations

### Dual Requirements

Mutations have two distinct requirements:

1. **Query-like Response**: Return the mutated data in the same format as a query would, matching the mutation's return type definition. This includes nested relationships - mutations return the same nested JSON structure as queries would.
2. **Sync Metadata**: Return `_affectedRows` data structure for efficient synchronization with other clients

All mutation types (insert, update, delete) follow this same pattern.

### Example

Given a mutation:
```pyre
insert CreatePost($title: String, $content: String, $published: Bool) {
    post {
        authorUserId = Session.userId
        title = $title
        content = $content
        published = $published
    }
}
```

The mutation should return:
```json
{
  "post": [
    {
      "authorUserId": 1,
      "title": "Hello",
      "content": "World",
      "published": true
    }
  ]
}
```

And also provide `_affectedRows` for sync:
```json
{
  "_affectedRows": [
    {
      "table_name": "posts",
      "headers": ["id", "createdAt", "authorUserId", "title", "content", "published", "updatedAt"],
      "rows": [
        [5001, 1768063249, 1, "Hello", "World", 1, 1768063249]
      ]
    }
  ]
}
```

**Key Differences:**
- **Mutation Response**: Returns only fields specified in the mutation definition (`authorUserId`, `title`, `content`, `published`), using Pyre types (e.g., `published: true` as boolean)
- **`_affectedRows`**: Returns all table columns (including `id`, `createdAt`, `updatedAt`), using serialized database types (e.g., `published: 1` as integer). Rows are arrays ordered to match the `headers` array for efficient transfer and processing.

### SQL Execution Strategy

Mutations execute multiple SQL statements in a batch:

1. **Insert/Update/Delete Statement**: Performs the actual mutation
   - Uses temporary tables to track affected row IDs
   - Example: `create temp table inserted_post as select last_insert_rowid() as id`

2. **Query Response Statement**: Generates the query-like return data
   - Selects the mutated rows
   - Formats them as JSON matching the mutation's return type (including nested relationships)
   - Returns only fields specified in the mutation definition
   - Uses Pyre types (e.g., booleans as `true`/`false`, dates as numbers)
   - Column name: `post` (or the mutation field name)
   - Value: JSON array of the mutated records
   - For mutations with multiple top-level fields, each field gets its own column
   - If no rows are affected, returns an empty array `[]`

3. **Sync Metadata Statement**: Generates `_affectedRows` for synchronization
   - Selects the affected rows with full table metadata
   - Includes all table columns (not just fields specified in mutation)
   - Uses serialized database types (e.g., booleans as `0`/`1`, dates as Unix timestamps)
   - Formats as JSON with `table_name`, `headers`, and `rows` (grouped by table)
   - Column name: `_affectedRows`
   - Value: JSON array of table groups, each containing:
     - `table_name`: The table name
     - `headers`: Array of column names in order
     - `rows`: Array of row arrays, where each row array contains values in the same order as `headers`
   - If multiple tables are affected (e.g., cascading inserts, multi-table operations), includes entries for each affected table
   - If no rows are affected, returns an empty array `[]`
   - **Always executed last** in the batch (guaranteed to be the final result set)

### Batch Execution

All three statements execute in a single batch transaction:
- Ensures atomicity
- Efficient single round-trip to the database
- All results available together

### Result Set Structure

After batch execution, the result sets are:
- **Result Set 1** (insert/update/delete): Excluded from response (marked with `include: false`)
- **Result Set 2+** (query response): Included (marked with `include: true`)
  - Columns: One column per top-level mutation field (e.g., `post`, `user`)
  - Values: JSON arrays matching mutation return type (including nested relationships)
  - For mutations with multiple top-level fields, multiple columns are returned
- **Final Result Set** (sync metadata): Included (marked with `include: true`)
  - Column: `_affectedRows`
  - Value: JSON array of table groups (includes all affected tables)
  - Each table group contains `table_name`, `headers`, and `rows` arrays
  - **Always the last result set** in the batch (guaranteed ordering)

### Application Handling

The database runner processes included result sets:
- Parses JSON from each included column
- Constructs a response object with keys matching column names
- Returns: `{ post: [...], _affectedRows: [...] }` (or multiple fields if mutation has multiple top-level fields)

The application code then:
1. Extracts `_affectedRows` for sync broadcasting (used internally, not returned to client)
2. Returns the query response fields to the client (e.g., `{ post: [...] }`), excluding `_affectedRows`

The `_affectedRows` field is handled separately by sync machinery and is never exposed to the client.

## Key Design Principles

### 1. SQL Composes JSON

The SQL directly produces the final JSON structure. No post-processing or transformation in application code is needed for the query response shape.

### 2. Batch Efficiency

Mutations use batch execution to:
- Minimize database round-trips
- Ensure atomicity
- Return both query response and sync metadata together

### 3. Separation of Concerns

- **Query Response**: Matches the query/mutation return type definition
- **Sync Metadata**: Separate `_affectedRows` structure for efficient client synchronization
- Application code separates these concerns, using sync metadata internally and returning query response to clients

### 4. Consistency

Mutations return data in the same format as queries would, ensuring a consistent API surface. The only difference is the additional `_affectedRows` metadata for mutations.

## Implementation Notes

### Column Naming

- Query responses use the query field name as the column name (e.g., `post`, `user`)
- Sync metadata always uses `_affectedRows` as the column name
- The `_` prefix indicates internal/metadata fields that should not be exposed to clients

### JSON Aggregation

SQL uses SQLite's JSON functions:
- `json_object()`: Creates JSON objects
- `json_group_array()`: Aggregates rows into JSON arrays
- `coalesce()`: Handles empty results (returns `[]` instead of `null`)

### Temporary Tables

Mutations use temporary tables to:
- Track affected row IDs after insert/update/delete
- Join back to fetch full row data
- Ensure we capture the exact rows that were modified

### Type Representation

- **Mutation Response**: Uses Pyre types - booleans are `true`/`false`, dates are numbers, etc. Only includes fields specified in the mutation definition.
- **`_affectedRows`**: Uses serialized database types - booleans are `0`/`1`, dates are Unix timestamps, etc. Includes all table columns for complete row metadata needed for synchronization.

### Row Array Format

The `rows` field in `_affectedRows` uses arrays instead of objects for efficiency:
- **Smaller JSON**: No repeated column names for each row
- **Faster SQLite generation**: `json_array(col1, col2, ...)` is simpler than `json_object('col1', col1, ...)`
- **Faster parsing**: No key lookups needed
- **Less network data**: More efficient for bulk transfers
- **Schema provided**: The `headers` array provides the column names, so objects can be reconstructed if needed

Each row array contains values in the exact order specified by the `headers` array.

## Permissions

### Overview

Pyre's permission system integrates directly with SQL generation, automatically enforcing access control at the database level. Permissions are defined in the schema using `@allow` directives and are automatically incorporated into generated SQL.

### Permission Types

Permissions can be defined in three ways:

1. **`@public`**: No restrictions - all operations are allowed
2. **`@allow(*) { conditions }`**: Applies to all operations (query, insert, update, delete)
3. **`@allow(query, insert, update, delete) { conditions }`**: Fine-grained permissions per operation type

### Permission Conditions

Permission conditions use WHERE clause syntax and can reference:
- **Table columns**: `authorId == Session.userId`
- **Session variables**: `Session.role == "admin"`
- **Literals**: `published == True`
- **Logical operators**: `And(...)`, `Or(...)`
- **Comparison operators**: `==`, `!=`, `>`, `<`, `>=`, `<=`

### Queries

For queries, permissions are automatically added as WHERE clauses to filter results:

**Schema:**
```pyre
record Post {
    id Int @id
    title String
    authorId Int
    published Bool
    @allow(query) {
        Or(
            authorId == Session.userId,
            published == True,
        )
    }
}
```

**Query:**
```pyre
query ListPosts {
    post {
        id
        title
    }
}
```

**Generated SQL:**
The permission condition is automatically added to the WHERE clause:
```sql
SELECT ... FROM posts
WHERE (authorId = $session_userId OR published = true)
```

The query `@where` clauses (if any) are combined with permission conditions using AND.

### Mutations

For mutations, permissions are enforced by adding conditions to the mutation's WHERE clause:

**Schema:**
```pyre
record Post {
    id Int @id
    title String
    authorId Int
    @allow(update) { authorId == Session.userId }
}
```

**Mutation:**
```pyre
update UpdatePost($id: Int, $title: String) {
    post {
        @where { id == $id }
        title = $title
    }
}
```

**Generated SQL:**
The permission condition is combined with the mutation WHERE clause:
```sql
UPDATE posts
SET title = $title
WHERE id = $id AND authorId = $session_userId
```

If the permission condition is not satisfied, the mutation affects 0 rows, effectively preventing unauthorized operations.

### Session Variable Replacement

When generating SQL, session variables in permissions are replaced with literal values from the current session:

- `Session.userId` → `$session_userId` (parameter) or literal value if available
- `Session.role` → `$session_role` (parameter) or literal value if available

This allows the database to efficiently evaluate permissions without additional application-level checks.

### Sync and Permissions

For sync operations, permissions are evaluated against affected rows to determine which clients should receive updates:

1. **Affected Rows**: When a mutation completes, `_affectedRows` contains all affected rows
2. **Permission Evaluation**: For each connected client, permissions are evaluated against each affected row
3. **Filtering**: Only rows that pass the permission check for a given client are included in that client's sync delta

This ensures that clients only receive updates for data they have permission to see, maintaining data privacy and security.

### Permission Evaluation

Permission evaluation happens at multiple levels:

1. **SQL Generation**: Permissions are baked into SQL WHERE clauses, ensuring database-level enforcement
2. **Sync Filtering**: Permissions are evaluated in application code (using WASM) to filter sync deltas per client
3. **Type Safety**: Permission conditions are validated during schema typechecking to ensure they reference valid columns and session variables

### Examples

**Public Access:**
```pyre
record Post {
    @public
    id Int @id
    title String
}
```
No WHERE clauses are added - all operations are unrestricted.

**Owner-Only Access:**
```pyre
record Post {
    id Int @id
    authorId Int
    @allow(*) { authorId == Session.userId }
}
```
All operations require the user to be the owner.

**Conditional Query Access:**
```pyre
record Post {
    id Int @id
    authorId Int
    published Bool
    @allow(query) {
        Or(
            authorId == Session.userId,
            published == True,
        )
    }
    @allow(insert, update, delete) { authorId == Session.userId }
}
```
Users can query their own posts or published posts, but can only modify their own posts.

**Role-Based Access:**
```pyre
record Post {
    id Int @id
    authorId Int
    @allow(query, insert, update) { authorId == Session.userId }
    @allow(delete) {
        Or(
            authorId == Session.userId,
            Session.role == "admin",
        )
    }
}
```
Only admins can delete posts they don't own.
