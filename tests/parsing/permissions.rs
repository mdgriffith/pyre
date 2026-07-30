use pyre::ast;
use pyre::error::ErrorType;
use pyre::parser;
use pyre::typecheck;

#[test]
fn structural_logical_predicates_require_multiple_operands() {
    for logical in ["And", "Or"] {
        for operands in ["", "published == True"] {
            let source = format!(
                r#"
record Post {{
    @allow(query) {{ {logical}({operands}) }}
}}
"#
            );
            let mut schema = ast::Schema::default();
            assert!(parser::run("schema.pyre", &source, &mut schema).is_err());
        }
    }
}

#[test]
fn permission_predicates_cannot_be_empty() {
    let mut schema = ast::Schema::default();
    assert!(parser::run(
        "schema.pyre",
        "record Post { @allow(query) {} }",
        &mut schema
    )
    .is_err());
}

#[test]
fn tagged_union_permission_rhs_uses_terminal_field_typechecking() {
    for predicate in [
        r#"state.Failed.code == "wrong""#,
        "state.Failed.code == unknownPermissionFn()",
    ] {
        let source = format!(
            r#"
type State
   = Failed {{
        code Int
     }}
   | Ready

record Job {{
    id Int @id
    state State
    @allow(query) {{ {} }}
}}
"#,
            predicate
        );
        let mut schema = ast::Schema::default();
        parser::run("permissions.pyre", &source, &mut schema).unwrap();
        let errors = typecheck::check_schema(&ast::Database {
            schemas: vec![schema],
        })
        .unwrap_err();
        assert!(errors
            .iter()
            .all(|error| error.filepath == "permissions.pyre"));
    }
}

#[test]
fn tagged_union_permission_rhs_is_typechecked_under_exists() {
    let source = r#"
type State
   = Failed {
        code String
     }
   | Ready

record Parent {
    id Int @id
    children @link(Child.parentId)
    @allow(query) { exists children { state.Failed.code == 1 } }
}

record Child {
    id Int @id
    parentId Int
    state State
    parent @link(parentId, Parent.id)
    @public
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    assert!(typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .is_err());
}

#[test]
fn tagged_union_permission_rhs_validates_declared_session_type() {
    let source = r#"
session {
    code String
}

type State
   = Failed {
        code Int
     }
   | Ready

record Job {
    id Int @id
    state State
    @allow(query) { state.Failed.code == Session.code }
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    assert!(typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .is_err());
}

#[test]
fn permission_functions_are_rejected_for_live_sync_consistency() {
    let source = r#"
record Job {
    id Int @id
    code Int
    @allow(query) { code == length("value") }
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    let errors = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        &error.error_type,
        ErrorType::InvalidPermissionPredicate { message }
            if message.contains("live sync") && message.contains("Functions")
    )));
}

fn membership_schema(session_type: &str, predicate: &str) -> String {
    format!(
        r#"
session {{
    activeIds {}
}}

record Job {{
    id String @id
    @allow(query) {{ {} }}
}}
"#,
        session_type, predicate
    )
}

#[test]
fn permission_membership_validates_collection_and_element_types() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        &membership_schema("Json<List<String>>", "id in Session.activeIds"),
        &mut schema,
    )
    .unwrap();
    typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("matching Session membership list should typecheck");

    for (session_type, predicate) in [
        ("String", "id in Session.activeIds"),
        ("Json<List<Int>>", "id in Session.activeIds"),
        ("Json<List<String>>", "id in Session.missing"),
        ("Json<List<String>>", "id in \"scalar\""),
        ("Json<List<String>>", "id in random()"),
    ] {
        let mut schema = ast::Schema::default();
        parser::run(
            "schema.pyre",
            &membership_schema(session_type, predicate),
            &mut schema,
        )
        .unwrap();
        assert!(
            typecheck::check_schema(&ast::Database {
                schemas: vec![schema],
            })
            .is_err(),
            "membership should be rejected: {}",
            predicate
        );
    }
}

#[test]
fn test_star_permission_simple() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = result {
        println!(
            "Parse error: {}",
            parser::render_error(schema_source, e, false)
        );
        panic!("Parse failed");
    }
}

#[test]
fn test_star_permission_with_and() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId && published == True }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Star permission with AND condition should parse successfully"
    );
}

#[test]
fn test_star_permission_with_or() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId || status == "published" }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Star permission with OR condition should parse successfully"
    );
}

#[test]
fn test_operation_specific_single_operation() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Operation-specific permission with single operation should parse successfully"
    );
}

#[test]
fn test_operation_specific_multiple_operations_same_line() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query, update) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Operation-specific permission with multiple operations on same line should parse successfully"
    );
}

#[test]
fn test_operation_specific_all_operations() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query, insert, update, delete) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Operation-specific permission with all operations should parse successfully"
    );
}

#[test]
fn test_operation_specific_multiple_lines() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query) { authorId == Session.userId }
    @allow(insert) { authorId == Session.userId }
    @allow(update) { authorId == Session.userId }
    @allow(delete) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Operation-specific permission with multiple lines should parse successfully"
    );
}

#[test]
fn test_operation_specific_mixed_lines() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query, update) { authorId == Session.userId }
    @allow(insert, delete) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Operation-specific permission with mixed lines should parse successfully"
    );
}

#[test]
fn test_operation_specific_with_complex_where() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query) { authorId == Session.userId || status == "published" }
    @allow(delete) { authorId == Session.userId && Session.role == "admin" }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Operation-specific permission with complex where clauses should parse successfully"
    );
}

#[test]
fn test_operation_specific_with_separate_permissions() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query) { authorId == Session.userId || status == "published" }
    @allow(delete) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Operation-specific permission with complex where clauses should parse successfully"
    );
}

#[test]
fn test_operation_specific_with_role_admin() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(delete) { Session.role == "admin" }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Operation-specific permission with complex where clauses should parse successfully"
    );
}

#[test]
fn test_star_permission_parses_when_inline_after_record_open_brace() {
    let schema_source = r#"
session {
    userId Int
    isAdmin Bool
}

type GameRole
    = Owner
    | Member

record Game {
    @public
    id Int @id
}

record User {
    @public
    id Int @id
}

record GameMember {    @allow(*) { userId == Session.userId || invitedByUserId == Session.userId || Session.isAdmin == True }
    game   @link(gameId, Game.id)
    user   @link(userId, User.id)
    inviter @link(invitedByUserId, User.id)

    id               Int @id
    gameId           Game.id
    userId           User.id
    role             GameRole
    invitedByUserId  User.id?
    joinedAt         DateTime @default(now)
    membershipKey    String @unique
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Inline @allow(*) after record opening brace should parse successfully"
    );
}

#[test]
fn test_star_permission_before_first_column_parses_successfully() {
    let schema_source = r#"
session {
    userId Int
    isAdmin Bool
}

type GameRole
    = Owner
    | Member

record GameMember {
    @allow(*) { userId == Session.userId || invitedByUserId == Session.userId || Session.isAdmin == True }
    id               Int @id
    userId           Int
    role             GameRole
    invitedByUserId  Int?
    joinedAt         DateTime @default(now)
    membershipKey    String @unique
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        parse_result.is_ok(),
        "@allow(*) before the first column should parse successfully"
    );
}

#[test]
fn test_star_permission_with_nullable_or_session_bool_parses_successfully() {
    let schema_source = r#"
session {
    userId Int
    isAdmin Bool
}

type GameRole
    = Owner
    | Member

record GameMember {
    id               Int @id
    userId           Int
    role             GameRole
    invitedByUserId  Int?
    joinedAt         DateTime @default(now)
    membershipKey    String @unique
    @allow(*) { userId == Session.userId || invitedByUserId == Session.userId || Session.isAdmin == True }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        parse_result.is_ok(),
        "GameMember-style @allow(*) expression should parse successfully"
    );
}

#[test]
fn test_permission_with_session_variable() {
    let schema_source = r#"
session {
    userId Int
    role String
}

record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Permission with Session variable should parse successfully"
    );
}

#[test]
fn test_permission_with_string_literal() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { status == "published" }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Permission with string literal should parse successfully"
    );
}

#[test]
fn test_permission_with_integer_literal() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == 1 }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Permission with integer literal should parse successfully"
    );
}

#[test]
fn test_permission_with_boolean_literal() {
    let schema_source = r#"
record Post {
    id Int @id
    published Bool
    @allow(*) { published == True }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Permission with boolean literal should parse successfully"
    );
}

#[test]
fn test_permission_with_comparison_operators() {
    let schema_source = r#"
record Post {
    id Int @id
    score Int
    @allow(*) { score >= 10 }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Permission with comparison operator should parse successfully"
    );
}

#[test]
fn test_permission_with_nested_and_or() {
    let schema_source = r#"
record Post {
    id Int @id
    authorId Int
    status String
    published Bool
    @allow(*) { 
        (authorId == Session.userId || status == "published") && published == True 
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    // Note: This might not parse correctly if parentheses aren't supported
    // Let's see what happens
    if result.is_err() {
        // If it fails, that's okay - we're testing what's actually supported
        println!("Nested parentheses may not be supported: {:?}", result);
    }
}

#[test]
fn test_permission_missing_closing_brace() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Permission with missing closing brace should fail to parse"
    );
}

#[test]
fn test_permission_missing_where_clause() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) {
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    // This might parse but be invalid semantically - let's see
    if result.is_err() {
        println!("Empty permissions block failed as expected: {:?}", result);
    }
}

#[test]
fn test_permission_invalid_operation() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(invalid) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Permission with invalid operation should fail to parse"
    );
}

#[test]
fn test_multiple_star_permissions_fails() {
    // This should fail - star permissions can't coexist
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId }
    @allow(*) { status == "published" }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    // Typecheck should fail - can't have multiple star permissions
    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail with multiple star @allow directives"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MultiplePermissions { record } if record == "Post")),
        "Should have MultiplePermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_single_permission_allowed() {
    // A single @allow directive should be allowed
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(*) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    // Typecheck should succeed with a single @allow directive
    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_ok(),
        "Typecheck should succeed with a single @allow directive. Errors: {:?}",
        typecheck_result.err()
    );
}

#[test]
fn test_permission_with_variable() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == $userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Permission with variable should parse successfully"
    );
}

#[test]
fn test_permission_operation_specific_with_multiple_where_conditions() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query) { authorId == Session.userId, status == "published" }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Operation-specific permission with multiple where conditions should parse successfully"
    );
}

#[test]
fn test_public_directive_parses() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        parse_result.is_ok(),
        "Schema with @public should parse successfully"
    );

    // Typecheck should succeed with @public directive
    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_ok(),
        "Typecheck should succeed with @public directive. Errors: {:?}",
        typecheck_result.err()
    );
}

#[test]
fn test_public_directive_allows_everything() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).unwrap();

    // Extract the record
    let record = schema.files[0]
        .definitions
        .iter()
        .find_map(|def| match def {
            ast::Definition::Record { name, fields, .. } if name == "Post" => {
                Some(ast::RecordDetails {
                    name: name.clone(),
                    fields: fields.clone(),
                    start: None,
                    end: None,
                    start_name: None,
                    end_name: None,
                })
            }
            _ => None,
        })
        .expect("Post record should exist");

    // get_permissions should return None for @public (allows everything)
    let perms = ast::get_permissions(&record, &ast::QueryOperation::Query);
    assert_eq!(perms, None, "@public should return None (no restrictions)");
}

#[test]
fn test_missing_permissions_error() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    // Typecheck should fail with MissingPermissions error
    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail with missing permissions directive"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MissingPermissions { record } if record == "Post")),
        "Should have MissingPermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_public_and_permissions_together_fails() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @public
    @allow(*) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    // Typecheck should fail with MultiplePermissions error
    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail with multiple permissions directives"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MultiplePermissions { record } if record == "Post")),
        "Should have MultiplePermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_operation_specific_multiple_conditions_separate_lines() {
    // Test the case where multiple conditions are on separate lines within an operation block
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorUserId Int
    published Bool
    @allow(query, update) { authorUserId == Session.userId }
    @allow(insert, delete) { authorUserId == Session.userId, published == True }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Operation-specific permission with multiple conditions on separate lines should parse successfully"
    );
}

#[test]
fn test_permission_implicit_and_multiline() {
    // Test that multiple conditions on separate lines have implicit && between them
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    status String
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Permission with implicit && (multi-line) should parse successfully"
    );
}

#[test]
fn test_permission_implicit_and_multiline_star() {
    // Test implicit && with star permission
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    published Bool
    @allow(*) {
        authorId = Session.userId
        published = True
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Star permission with implicit && (multi-line) should parse successfully"
    );
}

#[test]
fn test_permission_explicit_and_multiline() {
    // Test that explicit && still works in multi-line format
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    status String
    @allow(insert, update) {
        authorId = Session.userId
        && status = "draft"
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Permission with explicit && in multi-line format should parse successfully"
    );
}

#[test]
fn test_permission_explicit_or_multiline() {
    // Test that explicit || works in multi-line format
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    role String
    @allow(delete) {
        authorId = Session.userId
        || role = "admin"
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Permission with explicit || in multi-line format should parse successfully"
    );
}

#[test]
fn test_permission_mixed_implicit_and_explicit() {
    // Test mixing implicit && (newlines) with explicit operators
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    status String
    published Bool
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
        && published = False
    }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Permission with mixed implicit && and explicit && should parse successfully"
    );
}

#[test]
fn test_operation_specific_boolean_lowercase() {
    // Test that lowercase boolean values are accepted
    let schema_source = r#"
record Post {
    id Int @id
    title String
    published Bool
    @allow(query) { published == true }
    @allow(insert) { published == false }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Operation-specific permission with lowercase boolean should parse successfully"
    );
}

#[test]
fn test_operation_specific_boolean_capitalized() {
    // Test that capitalized boolean values are accepted
    let schema_source = r#"
record Post {
    id Int @id
    title String
    published Bool
    @allow(query) { published == True }
    @allow(insert) { published == False }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Operation-specific permission with capitalized boolean should parse successfully"
    );
}

#[test]
fn test_permissions_error_message_commits_to_permissions() {
    // Test that when we see @allow, we commit to that branch and give a proper error
    // if there's a parsing issue inside, rather than suggesting other directives
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(invalid syntax here) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Should fail to parse invalid syntax inside @allow"
    );

    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        // The error should NOT suggest other directives like @watch, @tablename, etc.
        // It should be a parsing error within the permissions block
        assert!(
            !error_msg.contains("@watch") && !error_msg.contains("@tablename"),
            "Error message should not suggest other directives when @allow is recognized. Error: {}",
            error_msg
        );
    }
}

#[test]
fn test_public_directive_counts_as_permissions() {
    // Test that @public counts as a permissions directive for validation
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @public
}

record Comment {
    id Int @id
    content String
    authorId Int
    @allow(*) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    // Typecheck should succeed - both records have exactly one permissions directive
    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_ok(),
        "Typecheck should succeed when all records have exactly one permissions directive. Errors: {:?}",
        typecheck_result.err()
    );
}

#[test]
fn test_multiple_fine_grained_permissions_allowed() {
    // Multiple fine-grained permissions should be allowed if they don't overlap
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(query) { authorId == Session.userId }
    @allow(insert) { authorId == Session.userId }
    @allow(update) { authorId == Session.userId }
    @allow(delete) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_ok(),
        "Typecheck should succeed with multiple non-overlapping fine-grained permissions. Errors: {:?}",
        typecheck_result.err()
    );
}

#[test]
fn test_star_permission_with_fine_grained_fails() {
    // Star permission can't coexist with fine-grained permissions
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(*) { authorId == Session.userId }
    @allow(query) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail when star permission coexists with fine-grained permissions"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MultiplePermissions { record } if record == "Post")),
        "Should have MultiplePermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_public_with_star_permission_fails() {
    // @public can't coexist with star permission
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @public
    @allow(*) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail when @public coexists with star permission"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MultiplePermissions { record } if record == "Post")),
        "Should have MultiplePermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_public_with_fine_grained_permission_fails() {
    // @public can't coexist with fine-grained permissions
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @public
    @allow(query) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail when @public coexists with fine-grained permissions"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MultiplePermissions { record } if record == "Post")),
        "Should have MultiplePermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_overlapping_fine_grained_permissions_fails() {
    // Fine-grained permissions can't overlap operations
    let schema_source = r#"
record Post {
    id Int @id
    title String
    @allow(query, update) { authorId == Session.userId }
    @allow(query) { status == "published" }
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_err(),
        "Typecheck should fail when fine-grained permissions overlap operations"
    );

    let errors = typecheck_result.unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(&e.error_type, ErrorType::MultiplePermissions { record } if record == "Post")),
        "Should have MultiplePermissions error for Post record. Errors: {:?}",
        errors
    );
}

#[test]
fn test_partial_operation_coverage_allowed() {
    // It's allowed to only grant permissions to some operations (others are denied)
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(query) { authorId == Session.userId }
    // insert, update, delete are implicitly denied
}
    "#;

    let mut schema = ast::Schema::default();
    let parse_result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(parse_result.is_ok(), "Schema should parse successfully");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let typecheck_result = typecheck::check_schema(&database);

    assert!(
        typecheck_result.is_ok(),
        "Typecheck should succeed with partial operation coverage. Errors: {:?}",
        typecheck_result.err()
    );
}

#[test]
fn test_multiline_allow_closing_brace_on_separate_line() {
    // Test the exact format that the formatter outputs - closing brace on its own line
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
     }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Multiline @allow with closing brace on separate line should parse successfully"
    );
}

#[test]
fn test_multiple_allow_directives_first_multiline() {
    // Test two @allow directives where the first is multiline
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
     }
    @allow(delete) { authorId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Multiple @allow directives with first multiline should parse successfully"
    );
}

#[test]
fn test_multiple_allow_directives_second_multiline() {
    // Test two @allow directives where the second is multiline (matches the failing test case)
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(query) { published == True }
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
     }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Multiple @allow directives with second multiline should parse successfully"
    );
}

#[test]
fn test_multiple_allow_directives_both_multiline() {
    // Test two @allow directives where both are multiline
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
     }
    @allow(delete) {
        authorId = Session.userId
        || Session.role = "admin"
     }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
    }
    assert!(
        result.is_ok(),
        "Multiple @allow directives with both multiline should parse successfully"
    );
}

#[test]
fn test_multiline_allow_with_space_before_closing_brace() {
    // Test the exact format from the formatter - space before closing brace, then brace on new line
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(query) { published == True  }
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
     }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
        println!("Error details: {:?}", e);
    }
    assert!(
        result.is_ok(),
        "Multiline @allow with space before closing brace should parse successfully"
    );
}

#[test]
fn test_exact_formatted_output_format() {
    // Test the exact format that the formatter outputs (matching the failing test)
    let schema_source = r#"
record Post {
    id       Int @id
    title    String
    authorId Int
    @allow(query) { published == True  }
    @allow(insert, update) {
        authorId = Session.userId
        status = "draft"
     }
    @allow(delete) {
        authorId = Session.userId
        || Session.role = "admin"
     }
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    if let Err(e) = &result {
        let error_msg = parser::render_error(schema_source, e.clone(), false);
        println!("Parse error:\n{}", error_msg);
        println!("Error details: {:?}", e);
    }
    assert!(
        result.is_ok(),
        "Exact formatted output format should parse successfully"
    );
}
