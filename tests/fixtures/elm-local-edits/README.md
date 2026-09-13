# Local Edit Conformance

`tests/elm_local_edits.rs` generates the shared TS descriptors and Elm bridge,
compiles the Elm fixture, and runs the real SQLite executor conformance suite.
The optional native Chromium suite uses the same generated fixture and the
TypeScript executor. It does not add a production browser dependency.

The executor suite checks permissions and strict rollback, structured/nullable
values, protected fields, server normalization/timestamps, related UUID writes,
integer identity results, named commands, empty batches and no-SSE replacement.
Two namespaces use the same UUID in separate databases to verify isolated rows,
readers, revisions and replacement contracts. Invalid fences and cross-namespace
operations reject. Existing worker/service transition tests cover deterministic
overlap, duplicate delivery, quarantine, unknown outcomes and cleanup permutations.
Generated modules compile systematic `set<Field>` update setters, and effects carry
the caller-provided model incarnation in their request IDs across bridge dedupe.

From the repository root, with the existing repository-local Playwright install:

```sh
env \
  PYRE_PLAYWRIGHT_MODULE="$PWD/target/indexeddb-browser-verify/node_modules/playwright/index.mjs" \
  PLAYWRIGHT_BROWSERS_PATH="$PWD/target/indexeddb-browser-verify/browsers" \
  npm_config_cache="$PWD/target/npm-cache" \
  cargo test --test elm_local_edits generated_local_edits_compile_and_run -- --nocapture
```

The native suite copies the client into the generated fixture under `target/`,
runs its actual build script with `elm@0.19.1-6 --optimize`, then bundles the
built engine, PyreClient, and generated edit descriptors for Chromium. No
release-pack invocation or changes to production build artifacts are needed.
Without `PYRE_PLAYWRIGHT_MODULE`, the native test is skipped.

Coverage includes native IndexedDB authoritative-only persistence while edits
are pending (including a visible optimistic deletion), TS/Elm create/update/delete
reader agreement, rollback without ghosts, unawaited receipt failures, read-only
startup/catchup, and disposal followed by a new auth-fenced client. A native
EventSource adapter delivers a real executor commit's reconciliation hint and
the client installs the resulting complete replacement.

HTTP routes and auth rotation are test-only loopback adapters to route-independent
libraries. This is focused integration verification, not coverage of deployed
authentication, production SSE routing, or exhaustive real-network races.
Built-in server HTTP/session tests remain in the CLI suite. Remote libSQL is not
verified; the TypeScript executor rejects unsupported local in-memory transactions.
Regenerate manifests, client metadata and WASM together before rollout. Legacy
noncanonical UUID data requires explicit correction, not identity coercion.
