pub const STATUS_TYPE: &str = r#"
type Status
   = Active
   | Inactive
   | Special {
        reason String
     }
"#;

pub const SCHEMA_V1: &str = r#"
record User {
    id   Int    @id
    name String
    status Status
    @public
}
"#;

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
