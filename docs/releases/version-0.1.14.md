# Pyre - 0.1.14

`0.1.14` fixes `pyre migrate --push` diagnostics when a schema stored in the target database no longer parses or typechecks. The CLI now identifies the stored schema as the source of the failure and reports actionable diagnostics with locations, the requested namespace, and a credential-safe database target.
