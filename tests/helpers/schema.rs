/// Status type definition
pub const STATUS_TYPE: &str = r#"
type Status
   = Active
   | Inactive
   | Special {
        reason String
     }
"#;

/// Schema version 1: User record
pub const SCHEMA_V1: &str = r#"
record User {
    id   Int    @id
    name String
    status Status
    @public
}
"#;

/// Schema version 2: Post record
pub const SCHEMA_V2: &str = r#"
record Post {
    id        Int    @id
    title     String
    content   String
    authorId  Int
    author    @link(authorId, User.id)
    @public
}
"#;

/// Schema version 3: Account record
pub const SCHEMA_V3: &str = r#"
record Account {
    id     Int   @id
    userId Int
    name   String
    status String
    user   @link(userId, User.id)
    @public
}
"#;

/// Get schema version 1 as a complete schema (User + Status)
pub fn schema_v1_complete() -> String {
    format!(
        r#"
@syncable(false)
{}

{}
"#,
        SCHEMA_V1.trim(),
        STATUS_TYPE.trim()
    )
}

/// Get schema version 2 as a complete schema (V1 + V2 + Status)
pub fn schema_v2_complete() -> String {
    format!(
        r#"
@syncable(false)
record User {{
    id   Int    @id
    name String
    status Status
    posts @link(Post.authorId)
    @public
}}

{}

{}
"#,
        SCHEMA_V2.trim(),
        STATUS_TYPE.trim()
    )
}

/// Get schema version 3 as a complete schema (V1 + V2 + V3)
pub fn schema_v3_complete() -> String {
    full_schema()
}

/// Get the full schema (version 3) for tests that don't need migrations
pub fn full_schema() -> String {
    format!(
        r#"
@syncable(false)
record User {{
    id   Int    @id
    name String
    status Status
    posts @link(Post.authorId)
    accounts @link(Account.userId)
    @public
}}

{}

{}

{}
"#,
        SCHEMA_V2.trim(),
        SCHEMA_V3.trim(),
        STATUS_TYPE.trim()
    )
}

/// Union type for testing column reuse: multiple variants with same field name and type
pub const UNION_COLUMN_REUSE_TYPE: &str = r#"
type Result
   = Success {
        message String
     }
   | Warning {
        message String
     }
   | Error {
        code Int
     }
"#;

/// Schema for testing column reuse
pub fn union_column_reuse_schema() -> String {
    format!(
        r#"
@syncable(false)
record TestRecord {{
    id Int @id
    result Result
    @public
}}

{}
"#,
        UNION_COLUMN_REUSE_TYPE.trim()
    )
}

/// Union type for testing separate columns: same field name but different types
pub const UNION_SEPARATE_COLUMNS_TYPE: &str = r#"
type MixedResult
   = TextResult {
        value String
     }
   | NumberResult {
        value Int
     }
   | FloatResult {
        value Float
     }
"#;

/// Schema for testing separate columns
pub fn union_separate_columns_schema() -> String {
    format!(
        r#"
@syncable(false)
record TestRecord {{
    id Int @id
    result MixedResult
    @public
}}

{}
"#,
        UNION_SEPARATE_COLUMNS_TYPE.trim()
    )
}

/// Union type for testing required sub-records
pub const UNION_REQUIRED_FIELDS_TYPE: &str = r#"
type Action
   = Create {
        name String
        description String
     }
   | Update {
        id Int
        changes String
     }
   | Delete {
        id Int
        reason String
     }
   | Simple
"#;

/// Schema for testing required sub-records
pub fn union_required_fields_schema() -> String {
    format!(
        r#"
@syncable(false)
record TestRecord {{
    id Int @id
    action Action
    @public
}}

{}
"#,
        UNION_REQUIRED_FIELDS_TYPE.trim()
    )
}
