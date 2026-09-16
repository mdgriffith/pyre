pub const STATUS_TYPE: &str = r#"
type Status
   = Active
   | Inactive
   | Special {
        reason String
     }
"#;

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
