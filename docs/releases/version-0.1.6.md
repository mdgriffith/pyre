# Pyre - 0.1.6

`0.1.6` fixes SQL generation for nested relationship queries that reach the same model through different paths. Generated CTE names now include their full query path, preventing duplicate CTE names while preserving the query response shape.
