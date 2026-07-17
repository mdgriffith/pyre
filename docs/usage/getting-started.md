# Getting Started with Pyre

Pyre is a schema and query language for building typesafe persistence using SQLite-compatible databases.

This guide teaches the main CLI-first workflow:

1. Define a schema.
2. Apply it to a database.
3. Write queries.
4. Validate and generate artifacts.
5. Choose how to integrate Pyre into your app.

## Built-In Docs

The canonical documentation lives in this `docs/usage/` folder and is designed to work well on GitHub or any normal docs site.

Pyre also ships the same docs through the CLI, which makes them easy to inspect from a shell or agent environment:

```bash
pyre docs
pyre docs schema
pyre docs query
pyre docs migrations
pyre docs serve
pyre docs mcp
```

Running `pyre docs` without a topic lists the available doc names.

## Step 1: Create A Schema

You have two common ways to get started:

### Option 1: Start A Fresh Pyre Project

`pyre init` creates a fresh starter setup in `./pyre`:

```bash
pyre init
```

### Option 2: Start From An Existing Database

`pyre introspect` connects to an existing database and generates a starting schema from it:

```bash
pyre introspect db/playground.db
```

### Option 3: Write The Schema Manually

Create a `pyre/` directory in your project root and add a `schema.pyre` file.

For the manual path, start with a file like this:

```pyre
record User {
    accounts @link(Account.userId)
    posts    @link(Post.authorUserId)

    id        Int      @id
    name      String?
    status    Status
    createdAt DateTime @default(now)
    @public
}

record Account {
    id     Int    @id
    userId Int
    name   String
    status String
    user   @link(userId, User.id)
    @public
}

record Post {
    id           Int      @id
    createdAt    DateTime @default(now)
    authorUserId Int
    title        String
    content      String
    status       Status
    author       @link(authorUserId, User.id)
    @public
}

type Status
   = Active
   | Inactive
   | Special { reason String }
```

This defines:

- records, which become tables
- typed columns like `Int`, `String`, and `DateTime`
- links between records
- reusable domain types like `Status`

For a deeper language reference, see [Schema Guide](./schema.md).

CLI shortcut: `pyre docs schema`

## Step 2: Apply The Schema To A Database

For a new local project, the simplest workflow is a direct push:

```bash
pyre migrate db/playground.db --push
```

Why start here:

- it is the shortest path from schema to working database
- it keeps the getting-started loop simple
- it avoids introducing migration-file workflow too early

If you want checked-in SQL migration files instead, see [Migration Guide](./migrations.md).

CLI shortcut: `pyre docs migrations`

## Step 3: Write Queries

Create a query file under `pyre/`. Any non-schema `.pyre` file in that tree is treated as a query file. A common convention is `pyre/query.pyre`.

```pyre
query GetUser($id: Int) {
    user {
        @where { id == $id }
        id
        createdAt
        username: name
        accounts {
            id
            name
            status
        }
    }
}

insert CreateUser($name: String, $status: Status) {
    user {
        name = $name
        status = $status
    }
}

update UpdatePostStatus($postId: Int, $status: Status) {
    post {
        @where { id == $postId }
        status = $status
    }
}

delete DeleteAccount($accountId: Int) {
    account {
        @where { id == $accountId }
    }
}
```

For a deeper language reference, see [Query Guide](./query.md).

CLI shortcut: `pyre docs query`

## Step 4: Validate And Generate

Typecheck your schema and queries:

```bash
pyre check
```

Then generate artifacts:

```bash
pyre generate
```

Generated output typically includes:

```text
pyre/generated/
├── client/
│   └── elm/
│       ├── Pyre.elm
│       └── Query/
└── typescript/
    ├── core/
    ├── run.ts
    └── server.ts
```

High-level purpose:

- `typescript/core/`: shared schema/query metadata
- `typescript/run.ts`: typed query functions for direct execution
- `typescript/server.ts`: query metadata used by `@pyre/server/query` and `@pyre/server/sync`
- `client/elm/`: generated Elm surfaces for sync-enabled clients

## Step 5: Choose An Integration Style

After generation, you have a few reasonable ways to use Pyre.

### Install TypeScript Dependencies

Pyre's TypeScript packages are distributed as versioned GitHub Release artifacts rather than through npm. Install all Pyre packages from the same release:

```json
{
  "dependencies": {
    "@libsql/client": "^0.14.0",
    "@pyre/core": "https://github.com/mdgriffith/pyre/releases/download/version-0.1.5/pyre-core-0.1.5.tgz",
    "@pyre/server": "https://github.com/mdgriffith/pyre/releases/download/version-0.1.5/pyre-server-0.1.5.tgz",
    "zod": "^4.1.12"
  },
  "overrides": {
    "@pyre/core": "https://github.com/mdgriffith/pyre/releases/download/version-0.1.5/pyre-core-0.1.5.tgz"
  }
}
```

Then run `bun install`. Generated artifacts currently consume the packages as TypeScript source, so use Bun or a build tool that transpiles TypeScript dependencies. Commit the resulting lockfile and keep all `@pyre/*` packages on the same release version.

### Option 1: Use The Built-In Server

If you want a working HTTP server quickly:

```bash
pyre serve db/playground.db
```

`pyre serve` is intended for local development, demos, and simple deployments. It is not a production-safe default by itself.

For the full operational guide and secure deployment model, see [pyre serve](./pyre-serve.md).

CLI shortcut: `pyre docs serve`

### Option 2: Use Generated TypeScript In Your Own Server

```typescript
import { createClient } from "@libsql/client";
import { GetUser } from "./pyre/generated/typescript/run";

const db = createClient({
    url: "file:./db/playground.db",
    authToken: undefined,
});

const result = await GetUser(db, { id: 1 });

console.log(result.user);
```

Generated functions accept an existing libSQL client and return the decoded, typed query result. For session-aware queries, pass the session before the input: `GetUser(db, session, { id: 1 })`.

This is the most flexible path when your app already has its own HTTP server and auth model. If the server dispatches generated client requests dynamically, use the `queries` map from `typescript/server.ts` with `@pyre/server/query` or `@pyre/server/sync` instead.

### Option 3: Use Live Sync With `@pyre/client`

If you want client-side sync, generated Elm query modules, or the standard Pyre sync runtime, continue with [Sync Setup](./sync.md).

See [Project Structure](./project-structure.md).

CLI shortcut: `pyre docs project-structure`

See [Troubleshooting](./troubleshooting.md).

CLI shortcut: `pyre docs troubleshooting`
