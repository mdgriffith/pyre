# SQLite Quirks and Limitations

This document summarizes SQLite-specific behaviors and limitations that affect Pyre's SQL generation and execution.

## Unique Indexes and NULL Semantics

SQLite treats `NULL` values as distinct for uniqueness checks. This means a unique index can contain multiple rows where one or more indexed columns are `NULL`.

**Example:**
```sql
CREATE UNIQUE INDEX uniq_memberships_org_user ON memberships(orgId, userId);
```

If `userId` is nullable, rows like `(orgId = 1, userId = NULL)` can appear more than once.

**Why This Matters:**
- Pyre's `@unique(...)` follows SQLite behavior.
- If you need strict uniqueness, make indexed columns `NOT NULL`.

## RETURNING Clause Limitations

### Cannot Use RETURNING in Subqueries

SQLite supports the `RETURNING` clause (since version 3.35.0) for `INSERT`, `UPDATE`, and `DELETE` statements. However, **RETURNING cannot be used within subqueries**.

**Not Supported:**
```sql
-- This will fail with a syntax error
INSERT INTO temp_table
SELECT * FROM (
  INSERT INTO posts (title, content) VALUES ('Title', 'Content')
  RETURNING *
);
```

**Why This Matters:**
- Pyre cannot directly capture `RETURNING` results into temporary tables using `INSERT INTO ... SELECT FROM (INSERT ... RETURNING *)`
- This prevents an optimization where we could populate temp tables with full row data immediately after mutations using pure SQL
- Instead, Pyre must use alternative approaches:
  - For INSERTs: Store `last_insert_rowid()` in temp table, then join back to main table later
  - For UPDATEs: Query the table after UPDATE using the same WHERE clause
  - For DELETEs: Capture rows BEFORE deletion into temp table

**Alternative Approaches:**

1. **Application-Level RETURNING Capture (Not Currently Used):**
   - Execute `INSERT ... RETURNING *` as a query (not execute)
   - Capture the returned rows in application code
   - Insert those rows into a temp table using parameterized INSERT
   - Continue with the rest of the batch
   - **Pros**: Avoids `last_insert_rowid()` and join-backs, gets full row data immediately
   - **Cons**: Requires application-level coordination (breaking pure SQL batch model), more complex execution logic

2. **Query by Unique Fields (Limited Applicability):**
   - After INSERT, query the table using unique field combinations
   - Example: `SELECT rowid FROM posts WHERE title = 'Title' AND content = 'Content'`
   - **Pros**: Pure SQL, no `last_insert_rowid()` needed
   - **Cons**: Only works if there's a unique constraint on the queried fields, race conditions possible, not generalizable

3. **Query by Timestamp/Sequence (Unreliable):**
   - After INSERT, query using `MAX(timestamp)` or similar
   - Example: `SELECT * FROM posts WHERE createdAt = (SELECT MAX(createdAt) FROM posts)`
   - **Pros**: Pure SQL, no `last_insert_rowid()` needed
   - **Cons**: Race conditions (two inserts at same time), requires timestamp field, unreliable in concurrent scenarios

4. **Explicit IDs (When Available):**
   - If the mutation provides explicit ID values, use those directly
   - **Pros**: No need to track IDs, works immediately
   - **Cons**: Not all inserts have explicit IDs, still need to track which rows were inserted for `_affectedRows`

**Current Workaround (Pure SQL):**
```sql
-- Pyre's current approach (pure SQL, no application coordination):
INSERT INTO posts (title, content) VALUES ('Title', 'Content');
CREATE TEMP TABLE inserted_post AS SELECT last_insert_rowid() AS id;
-- Later: SELECT * FROM posts WHERE rowid IN (SELECT id FROM inserted_post);
```

## CTE Limitations for Nested Inserts

### Cannot Nest INSERTs in CTEs

SQLite does not allow nested `INSERT` statements within Common Table Expressions (CTEs).

**Not Supported:**
```sql
WITH inserted_user AS (
  INSERT INTO users (name) VALUES ('Alice')
  RETURNING *
), inserted_posts AS (
  INSERT INTO posts (userId, title)
  SELECT id, 'My Post' FROM inserted_user
  RETURNING *
)
SELECT * FROM inserted_user;
```

**Why This Matters:**
- Pyre's nested insert mutations (e.g., inserting a user with related posts and accounts) cannot use CTEs
- Instead, Pyre uses temporary tables to track inserted row IDs and chain dependent inserts

**Pyre's Approach:**
```sql
-- Insert parent
INSERT INTO users (name) VALUES ('Alice');

-- Store parent ID in temp table
CREATE TEMP TABLE temp_ids AS SELECT last_insert_rowid() AS userId;

-- Insert children using temp table
INSERT INTO posts (userId, title)
SELECT userId, 'My Post' FROM temp_ids;

-- Clean up
DROP TABLE temp_ids;
```

## last_insert_rowid() Behavior

### Only Returns Most Recent Row

The `last_insert_rowid()` function returns the rowid of the most recently inserted row **in the current database connection**. This means:

- Each `INSERT` statement overwrites the previous value
- For multi-row INSERTs, only returns the **last** row's rowid
- Cannot be called multiple times to track multiple inserts
- Must be captured immediately after each `INSERT` if you need to track multiple row IDs

**Example:**
```sql
-- Insert 3 rows in one statement
INSERT INTO posts (title) VALUES ('Post 1'), ('Post 2'), ('Post 3');
SELECT last_insert_rowid(); -- Returns 3 (only the last row's rowid)
-- Rowids 1 and 2 are lost!
```

**Why This Matters:**
- **Single-row inserts**: Works fine - `last_insert_rowid()` captures the one inserted row
- **Multi-row inserts**: Only the last row's ID is captured - all other row IDs are lost
- For nested inserts with multiple dependent tables, Pyre must create temporary tables to store each parent's rowid before inserting children
- Without temp tables, `last_insert_rowid()` would only return the last inserted row, losing track of parent row IDs

**Current Limitation:**
Pyre's mutation INSERTs are currently **single-row only** (one `VALUES` clause per INSERT statement). This works with `last_insert_rowid()` because there's only one row to track. If Pyre were to support multi-row INSERTs in a single statement (e.g., `INSERT INTO posts VALUES (...), (...), (...)`), `last_insert_rowid()` would only capture the last row's ID, requiring a different approach like:
- Application-level RETURNING capture (to get all row IDs)
- Separate single-row INSERTs (one per row)
- Using a different tracking mechanism

**Alternative Approaches:**
1. **Current (Pure SQL)**: Use `last_insert_rowid()` + temp tables + join-back
   - **Pros**: Pure SQL, reliable, works in all cases
   - **Cons**: Requires join-back to get full row data

2. **Application-Level RETURNING**: Execute `INSERT ... RETURNING *` as query, capture rows in application code, insert into temp table
   - **Pros**: More efficient (no join-back needed), gets full row data immediately
   - **Cons**: Requires breaking pure SQL batch model, application-level coordination

3. **Query by Unique Fields**: Query table using unique field combinations after INSERT
   - **Pros**: Pure SQL, no `last_insert_rowid()` needed
   - **Cons**: Only works with unique constraints, race conditions possible, not generalizable

4. **Query by Timestamp**: Query using `MAX(timestamp)` after INSERT
   - **Pros**: Pure SQL, no `last_insert_rowid()` needed
   - **Cons**: Race conditions, requires timestamp field, unreliable

5. **Explicit IDs**: Use provided ID values directly
   - **Pros**: No ID tracking needed
   - **Cons**: Not always available, still need to track rows for `_affectedRows`

**Example:**
```sql
-- Without temp table (WRONG - loses track of user ID):
INSERT INTO users (name) VALUES ('Alice');
INSERT INTO posts (userId, title) VALUES (last_insert_rowid(), 'Post 1');
INSERT INTO accounts (userId, name) VALUES (last_insert_rowid(), 'Account'); -- WRONG! This gets posts rowid, not user rowid

-- With temp table (CORRECT - current approach):
INSERT INTO users (name) VALUES ('Alice');
CREATE TEMP TABLE temp_user AS SELECT last_insert_rowid() AS userId;
INSERT INTO posts (userId, title) SELECT userId, 'Post 1' FROM temp_user;
INSERT INTO accounts (userId, name) SELECT userId, 'Account' FROM temp_user;

-- Alternative (not currently used - requires application coordination):
-- 1. Execute: INSERT INTO users (name) VALUES ('Alice') RETURNING *;
-- 2. Capture row in application code
-- 3. Execute: INSERT INTO temp_user SELECT * FROM (captured row);
-- 4. Continue with dependent inserts using temp_user
```

## Database Locking Behavior

### Write Locks Can Prevent Immediate Reads

SQLite uses locking to manage concurrent access. In certain contexts, reading from a table immediately after a write operation can result in a "database table is locked" error.

**Observed Behavior:**
- After an `INSERT`, attempting to `SELECT` from the same table immediately can fail with lock errors
- This appears to be more common in batch/transaction execution contexts
- The lock is typically released after the statement completes, but timing can vary

**Why This Matters:**
- Pyre cannot reliably read from the main table immediately after `INSERT` to populate temp tables
- Instead, Pyre stores row IDs in temp tables and defers full row reads until later in the batch
- Query response and `_affectedRows` statements join back to the main table after all mutations complete

**Pyre's Approach:**
```sql
-- Step 1: INSERT (acquires write lock)
INSERT INTO posts (title) VALUES ('Title');

-- Step 2: Store rowid only (doesn't read from table)
CREATE TEMP TABLE inserted_post AS SELECT last_insert_rowid() AS id;

-- Step 3: Later in batch, join back to get full data (lock released)
SELECT * FROM posts WHERE rowid IN (SELECT id FROM inserted_post);
```

## Temporary Tables

### Session-Scoped Storage

SQLite temporary tables (`CREATE TEMP TABLE`) are:
- Scoped to the database connection/session
- Automatically dropped when the connection closes
- Useful for tracking intermediate results across multiple statements

**Why This Matters:**
- Pyre uses temp tables extensively to work around SQLite limitations:
  - Tracking inserted row IDs for nested inserts
  - Capturing affected rows for mutations
  - Storing intermediate query results
- Temp tables allow multi-statement operations that would be impossible with CTEs alone

**Cleanup Behavior:**
- **Local SQLite**: Temp tables persist until the connection closes. Pyre drops them explicitly when safe (no active result sets).
- **Remote libsql**: When using `@libsql/client` with `batch()`, temp tables **persist across batches** when reusing the same client connection. They are only dropped when the connection closes. See [sql_remote.md](./sql_remote.md) for details.
- Temp tables are connection-specific, so they don't interfere with other connections
- **Important**: When tracking affected rows, Pyre does NOT drop temp tables explicitly (to avoid lock errors), which means they persist across batches and can cause `table already exists` errors

**Best Practices:**
- Use descriptive names to avoid conflicts: `inserted_post`, `deleted_user`, etc.
- **When reusing the same client**: Either drop temp tables explicitly after each batch, use unique names per execution, or use `CREATE TEMP TABLE IF NOT EXISTS` / `DROP TABLE IF EXISTS` patterns
- For local SQLite, drop temp tables explicitly only when no result sets are active
- Consider using unique temp table names (e.g., with timestamps or UUIDs) if you need to avoid conflicts across batches

## JSON Functions

### SQLite Supports Both `json_*` and `jsonb_*` Functions

SQLite provides both `json_*` and `jsonb_*` functions (since version 3.45.0, released January 2024). The difference:

- **`json_*` functions** (`json_object()`, `json_group_array()`, etc.): Return JSON as TEXT
- **`jsonb_*` functions** (`jsonb_object()`, `jsonb_group_array()`, etc.): Return JSON in binary format (JSONB)

**Available Functions:**
- `json_object(key1, value1, ...)` / `jsonb_object(...)` - Creates a JSON object
- `json_group_array(value)` / `jsonb_group_array(value)` - Aggregates values into a JSON array
- `json_array(value1, value2, ...)` / `jsonb_array(...)` - Creates a JSON array
- `json(value)` / `jsonb(value)` - Parses a JSON string and returns the JSON value

**Pyre's Usage Pattern:**
- **Intermediate results (CTEs, temp tables)**: Uses `jsonb_*` functions for efficiency
  - Binary format reduces parsing overhead
  - Faster aggregation operations
  - Example: `jsonb_group_array(jsonb_object(...))` in CTEs
- **Final results**: Uses `json_*` functions for compatibility
  - TEXT format is compatible with JSON strings
  - Ensures type compatibility when returning final results
  - Example: `json_object('field', json_group_array(...))` in final SELECT

**Why This Matters:**
- JSONB functions are more efficient for intermediate processing
- JSON functions are needed for final results to ensure compatibility
- Mixing requires type conversion: `json(jsonb_value)` converts JSONB to JSON
- SQLite does not have a separate declared JSON or JSONB column type. Pyre stores typed document values as `BLOB` and uses SQLite's JSONB representation for persisted `Json<T>` data.

## Type System

### Dynamic Typing vs. Pyre Types

SQLite uses dynamic typing (type affinity) rather than strict types. Pyre maps its types to SQLite storage classes:

| Pyre Type | SQLite Storage |
|----------|----------------|
| `Int` | INTEGER |
| `Float` | REAL |
| `String` | TEXT |
| `Bool` | INTEGER (0 or 1) |
| `DateTime` | INTEGER (whole Unix epoch seconds) |
| `Date` | TEXT |
| `JSON` | BLOB |
| `Json<T>` | BLOB |

`List<T>` and `Dict<T>` are document-only container types. They are only valid inside `Json<...>` and are stored using the enclosing `Json<T>` column.

Pyre validates `Json<T>` payload shapes before writes. It does not re-validate typed JSON payloads on reads.

**Why This Matters:**
- SQLite will accept values of different types in columns (with type affinity)
- Pyre enforces types at the schema level, but SQLite doesn't enforce them at the database level
- Type mismatches may not be caught until query execution time

## Rowid vs. Primary Keys

### Internal Rowid vs. Explicit Primary Keys

SQLite automatically creates an internal `rowid` column for every table (unless using `WITHOUT ROWID`). This `rowid`:
- Is distinct from user-defined primary keys
- Can be used in `WHERE rowid = ...` clauses
- Is what `last_insert_rowid()` returns
- May differ from an explicit `id` column

**Why This Matters:**
- Pyre uses `rowid` for joins when tracking inserted rows: `WHERE t.rowid IN (SELECT id FROM temp_table)`
- When using `@id` directives, Pyre creates explicit primary key columns, but `rowid` still exists
- For `INTEGER PRIMARY KEY` columns, SQLite uses the same value as `rowid` (aliased)

## Transaction Behavior

### Immediate Transactions

Pyre uses `TransactionBehavior::Immediate` for mutations, which:
- Acquires a write lock immediately, even before any writes occur
- Prevents other connections from writing until the transaction commits
- Ensures atomicity of multi-statement mutations

**Why This Matters:**
- All statements in a mutation batch execute atomically
- Locks are held for the duration of the transaction
- This can contribute to lock conflicts if reads happen too soon after writes

## Summary

SQLite's limitations around RETURNING, CTEs, and locking require Pyre to use temporary tables extensively. While this adds complexity, it enables Pyre to:
- Support nested inserts across multiple tables
- Track affected rows for synchronization
- Generate efficient batch operations
- Work within SQLite's constraints while maintaining correctness

The key patterns Pyre uses:
1. **Store rowids in temp tables** instead of using RETURNING in subqueries
2. **Defer full row reads** until after mutations complete to avoid lock conflicts
3. **Explicitly drop temp tables** to avoid conflicts
4. **Use joins** to reconstruct full row data from temp table rowids
