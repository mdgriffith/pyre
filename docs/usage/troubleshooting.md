# Troubleshooting

This page collects common first-run and local-development issues.

## `Schema Not Found`

Make sure your schema is in one of these locations:

- `pyre/schema.pyre`
- `pyre/schema/<Namespace>/schema.pyre`

CLI shortcut: `pyre docs project-structure`

## Namespace Errors Or `Unknown Schema`

If you have multiple schema namespaces, migration-related commands usually require `--namespace <Name>`.

See [Namespacing](./namespacing.md).

CLI shortcut: `pyre docs namespacing`

## `pyre serve` Says Generated Artifacts Are Missing

Run:

```bash
pyre generate
```

before starting the server.

See [pyre serve](./pyre-serve.md).

CLI shortcut: `pyre docs serve`

## Session Errors In `pyre serve`

If your schema includes a `session { ... }` block, provide session data with either:

- `--dev-session` for local development
- `--session-header` and `--session-secret` behind trusted upstream auth

See [pyre serve](./pyre-serve.md) for the secure deployment model.

CLI shortcut: `pyre docs serve`

## Confusion About `migrate`, `migration`, And `--push`

These commands do different things:

- `pyre migrate db/dev.db --push`: update an explicitly disposable database from the working schema
- `pyre migration create add_users`: author a bundle against finalized history
- `pyre migrate db/app.db`: apply finalized bundles through the target's stored catalog binding

See [Migration Guide](./migrations.md).

CLI shortcut: `pyre docs migrations`

## Existing Database, No Pyre Schema Yet

If the database already exists and you want Pyre to derive a starting schema from it, use:

```bash
pyre introspect db/app.db
```

Then review the generated schema and run `pyre check`.

CLI shortcut: `pyre docs getting-started`
