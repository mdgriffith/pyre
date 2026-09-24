use pyre::ast;
use pyre::error::ErrorType;
use pyre::parser;
use pyre::typecheck;

#[test]
fn test_insert_with_session_variable() {
    let schema_source = r#"
@syncable(false)
session {
    userId Int
    role   String
}

record Post {
    @allow(*) {userId == Session.userId}
    id           Int     @id
    createdAt    DateTime @default(now)
    authorUserId Int
    title        String
    content      String
    published    Bool    @default(False)
    users        @link(authorUserId, User.id)
}

record User {
    @public
    id        Int     @id
    name      String?
    email     String?
    createdAt DateTime @default(now)
    posts     @link(Post.authorUserId)
}
    "#;

    // Parse schema
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    // Schema checking should fail because permissions reference userId which doesn't exist on Post
    let schema_result = typecheck::check_schema(&database);
    match schema_result {
        Ok(_) => {
            panic!("Expected schema typechecking to fail due to invalid permissions (userId doesn't exist on Post), but it succeeded");
        }
        Err(errors) => {
            // Check if the error is about userId not existing on Post in permissions
            let has_unknown_field_error = errors.iter().any(|e| {
                matches!(
                    &e.error_type,
                    ErrorType::UnknownField { found, record_name, .. }
                    if found.contains("userId") && record_name == "Post"
                )
            });

            if !has_unknown_field_error {
                panic!(
                    "Expected UnknownField error for userId on Post in permissions, but got: {:?}",
                    errors
                );
            }
            // Test passes - permissions validation caught the error during schema checking
            return;
        }
    }
}
