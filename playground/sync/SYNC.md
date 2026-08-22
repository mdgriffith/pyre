# Sync Architecture

This document describes the sync architecture implemented in the sync playground, which demonstrates Pyre's real-time data synchronization capabilities.

## Overview

The sync playground implements a client-server architecture where:
1. **Clients** connect via WebSocket and maintain their own local data cache
2. **Server** manages database state and broadcasts changes to clients based on permissions
3. **Sync** ensures clients only receive data they have permission to see

## Architecture Components

### 1. Sync Catchup (Initial Sync)

When a client first connects, it performs a "sync catchup" to fetch all data it has permission to see:

- **Endpoint**: `POST /sync`
- **Request**: 
  ```typescript
  {
    syncCursor: SyncCursor,  // Empty cursor for initial sync: { tables: {} }
    session: Session          // Client's session with userId, role, etc.
  }
  ```
- **Response**: 
  ```typescript
  {
    tables: {
      [tableName]: {
        rows: JsonValue[],           // Array of row objects
        permission_hash: string,     // Hash of permissions for this table
        last_seen_updated_at: number | null,
        last_seen_primary_key: number | string | null // Key of the final row at that timestamp
      }
    },
    has_more: boolean
  }
  ```

**Process**:
1. Client sends empty sync cursor (or cursor from previous sync)
2. Server uses WASM `get_sync_status_sql()` to determine which tables need syncing
3. Server uses WASM `get_sync_sql()` to generate SQL queries based on:
   - Client's session (permissions)
   - Sync cursor state (what client has already seen)
4. Server executes SQL and returns data
5. Client updates its local cache and sync cursor

### 2. WebSocket Connection

Clients maintain a persistent WebSocket connection for real-time updates:

- **Endpoint**: `WS /sync`
- **Connection Flow**:
  1. Client opens WebSocket connection
  2. Server generates unique `sessionId` and assigns random `userId` (for testing)
  3. Server sends `{ type: "connected", sessionId }` message
  4. Client performs sync catchup using the sessionId
  5. Client listens for sync delta messages

### 3. Sync Deltas (Real-time Updates)

When mutations occur, the server calculates which clients should receive updates:

- **Trigger**: Any mutation (insert/update/delete) via `POST /db/:req`
- **Process**:
  1. Server extracts `_affectedRows` from mutation result
  2. Server calls WASM `calculate_sync_deltas(affectedRows, connectedSessions)`
  3. WASM groups clients by which affected rows they can see (based on permissions)
  4. Server broadcasts delta messages to each group:
     ```typescript
     {
       type: "delta",
       data: {
         all_affected_rows: AffectedRow[],
         affected_row_indices: number[]  // Indices into all_affected_rows this client should receive
       }
     }
     ```
  5. Clients update their local cache based on deltas

### 4. Client Data Management

Each client maintains:
- **Sync Cursor**: Tracks what data has been synced per table
  ```typescript
  {
    tables: {
      [tableName]: {
        last_seen_updated_at: number | null,
        last_seen_primary_key: number | string | null,
        permission_hash: string
      }
    }
  }
  ```
- **Local Cache**: In-memory store of synced data
  - User record (their own user based on session.userId)
  - Posts they can see (based on permissions)

### 5. Permissions Model

Permissions are defined in the schema and evaluated per session:

```pyre
record Post {
    @allow(query) {
        Or(
            authorUserId == Session.userId,
            published == True,
        )
    }
    @allow(update, insert, delete) { authorUserId == Session.userId }
    // ...
}
```

- **Query permission**: Determines which rows a client can see
- **Permission hash**: Computed hash of permission expression + session values
- **Sync uses permission hash**: To detect if permissions changed (requires full resync)

## Data Flow

```
┌─────────┐                    ┌─────────┐
│ Client  │                    │ Server  │
└────┬────┘                    └────┬────┘
     │                              │
     │ 1. WS Connect                │
     ├─────────────────────────────>│
     │                              │
     │ 2. { type: "connected" }     │
     │<─────────────────────────────┤
     │                              │
     │ 3. POST /sync (catchup)      │
     │    { syncCursor: {},         │
     │      session: {...} }        │
     ├─────────────────────────────>│
     │                              │
     │ 4. Execute sync SQL          │
     │    (get_sync_status_sql,     │
     │     get_sync_sql)            │
     │                              │
     │ 5. Return sync data          │
     │<─────────────────────────────┤
     │                              │
     │ 6. Update local cache        │
     │                              │
     │                              │
     │ [Later: Mutation occurs]     │
     │                              │
     │ 7. POST /db/:req (mutation)  │
     │<─────────────────────────────┤
     │                              │
     │ 8. Calculate sync deltas     │
     │    (calculate_sync_deltas)   │
     │                              │
     │ 9. Broadcast delta           │
     │<─────────────────────────────┤
     │                              │
     │ 10. Update local cache       │
     │                              │
```

## Tableau Visualization

The playground includes a "Tableau" view that visualizes sync state:

- **Purpose**: See what data each connected client currently has
- **Layout**: Grid of client cards
- **Each Card Shows**:
  - User name (grey, left side, outside card)
  - Mini grid of posts the client can see
  - Post titles (barely readable, super small)
- **Updates**: Automatically refreshes as sync deltas arrive

This visualization helps test and verify that:
- Clients only see data they have permission to see
- Sync deltas are correctly distributed
- Permission changes trigger appropriate updates

## WASM Functions Used

The sync system relies on these WASM functions:

- `get_sync_status_sql(syncCursor, session)`: Returns SQL to check sync status
- `get_sync_sql(statusRows, syncCursor, session, pageSize)`: Returns SQL to fetch sync data
- `calculate_sync_deltas(affectedRows, connectedSessions)`: Groups clients by which deltas they should receive
- `set_schema(introspection)`: Loads schema into WASM cache (required before sync operations)

## Testing the Sync System

1. **Start server**: Creates 20 users in database
2. **Add clients**: Each gets random userId (1-100)
3. **Observe catchup**: Each client syncs initial data on connect
4. **Create posts**: Use mutations to create posts (published/unpublished)
5. **Watch deltas**: See which clients receive updates based on permissions
6. **View tableau**: Visualize what each client currently sees

## Key Patterns

1. **Session-based permissions**: Each client has a session that determines what they can see
2. **Cursor-based syncing**: Clients track what they've seen to enable incremental sync
3. **Permission hashing**: Detects permission changes without re-evaluating expressions
4. **Delta broadcasting**: Efficiently distributes updates only to relevant clients
5. **Local cache**: Clients maintain their own view of the data
