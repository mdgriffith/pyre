# Pyre - 0.1.7

`0.1.7` fixes migration validation for columns with value-based defaults. Schema formatting, comments, and trailing whitespace around an unchanged `@default(...)` no longer cause a false modified-column error, while actual default additions, removals, and value changes are still detected.
