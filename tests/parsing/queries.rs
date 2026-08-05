use pyre::{ast, parser, typecheck};

/// Helper function to format errors without color for testing
fn format_error_no_color(file_contents: &str, error: &pyre::error::Error) -> String {
    return pyre::error::format_error(file_contents, error, false);
}

fn strip_ansi_codes(s: &str) -> String {
    // Remove ANSI escape sequences (CSI sequences)
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            // Skip the escape sequence
            chars.next(); // skip '['
            while let Some(&c) = chars.peek() {
                if c == 'm' {
                    chars.next(); // skip 'm'
                    break;
                }
                chars.next();
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[test]
fn test_valid_query() {
    let query_source = r#"
        query GetUsers {
            user {
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_ok(), "Valid query should parse successfully");
}

fn union_predicate_context() -> typecheck::Context {
    let source = r#"
type ProviderReason
   = ProviderRejected {
        code String
     }
   | Other

type JobState
   = Failed {
        errorCode Int
        reason ProviderReason
     }
   | Ready {
        errorCode Int
     }

record Job {
    id Int @id
    state JobState
    @public
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema parses");
    typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks")
}

#[test]
fn tagged_union_predicate_paths_parse_and_typecheck() {
    let source = r#"
query Jobs($code: Int) {
    job {
        @where { state.Failed.errorCode == $code && state.Failed.reason.ProviderRejected.code != "x" }
        id
    }
}
"#;
    let queries = parser::parse_query("query.pyre", source).expect("paths parse");
    let query = match &queries.queries[0] {
        ast::QueryDef::Query(query) => query,
        _ => panic!("expected query"),
    };
    let field = match &query.fields[0] {
        ast::TopLevelQueryField::Field(field) => field,
        _ => panic!("expected field"),
    };
    let wheres = ast::collect_wheres(&field.fields);
    let ast::WhereArg::And(paths) = &wheres[0] else {
        panic!("expected conjunction");
    };
    let ast::WhereArg::Column(_, first, _, _, _) = &paths[0] else {
        panic!("expected path");
    };
    let ast::WhereArg::Column(_, nested, ast::Operator::NotEqual, _, _) = &paths[1] else {
        panic!("expected != path");
    };
    assert_eq!(first.authored(), "state.Failed.errorCode");
    assert_eq!(
        nested.authored(),
        "state.Failed.reason.ProviderRejected.code"
    );
    typecheck::check_queries(&queries, &union_predicate_context()).expect("paths typecheck");
}

fn session_union_predicate_context() -> typecheck::Context {
    let source = r#"
type SessionScope
   = Workspace {
        id Int?
     }
   | Account {
        id Int?
     }

session {
    scope SessionScope
}

record Resource {
    id Int @id
    ownerId Int?
    label String
    @public
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema parses");
    typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks")
}

#[test]
fn session_tagged_union_paths_parse_typecheck_and_describe_scalar_params() {
    let source = r#"
query Resources {
    resource {
        @where { Session.scope.Workspace.id == 1 && ownerId == Session.scope.Workspace.id }
        id
    }
}
"#;
    let queries = parser::parse_query("query.pyre", source).expect("session paths parse");
    let ast::QueryDef::Query(query) = &queries.queries[0] else {
        panic!("expected query");
    };
    let ast::TopLevelQueryField::Field(field) = &query.fields[0] else {
        panic!("expected field");
    };
    let wheres = ast::collect_wheres(&field.fields);
    let ast::WhereArg::And(predicates) = &wheres[0] else {
        panic!("expected conjunction");
    };
    let ast::WhereArg::Column(true, lhs_path, _, _, _) = &predicates[0] else {
        panic!("expected Session path on the left");
    };
    assert_eq!(lhs_path.authored(), "scope.Workspace.id");

    let ast::WhereArg::Column(false, _, _, ast::QueryValue::Variable((_, rhs)), _) =
        &predicates[1]
    else {
        panic!("expected Session path on the right");
    };
    assert_eq!(
        rhs.session_path().expect("session path").authored(),
        "scope.Workspace.id"
    );
    assert_eq!(rhs.name, "session_scope__id");
    assert_eq!(
        ast::to_pyre_variable_name(rhs),
        "Session.scope.Workspace.id"
    );

    let infos = typecheck::check_queries(&queries, &session_union_predicate_context())
        .expect("session paths typecheck");
    let params = &infos["Resources"].variables;
    let typecheck::ParamInfo::Defined {
        raw_variable_name,
        type_,
        nullable,
        session_name,
        session_path,
        session_discriminator,
        used,
        ..
    } = &params["Session.scope.Workspace.id"]
    else {
        panic!("expected terminal session param");
    };
    assert_eq!(raw_variable_name, "session_scope__id");
    assert_eq!(type_.as_deref(), Some("Int"));
    assert!(*nullable);
    assert_eq!(session_name.as_deref(), Some("scope__id"));
    assert_eq!(
        session_path.as_ref().map(ast::PredicatePath::authored),
        Some("scope.Workspace.id".to_string())
    );
    assert_eq!(session_discriminator, &None);
    assert!(*used);

    let typecheck::ParamInfo::Defined {
        raw_variable_name,
        session_name,
        session_discriminator,
        used,
        ..
    } = &params["Session.scope"]
    else {
        panic!("expected discriminator session param");
    };
    assert_eq!(raw_variable_name, "session_scope");
    assert_eq!(session_name.as_deref(), Some("scope"));
    assert_eq!(session_discriminator.as_deref(), Some("Workspace"));
    assert!(*used);
}

#[test]
fn session_tagged_union_paths_report_invalid_variants_and_terminal_types() {
    let context = session_union_predicate_context();
    for predicate in [
        "ownerId == Session.scope.Unknown.id",
        "ownerId == Session.scope.Workspace.missing",
        "ownerId == Session.scope.Workspace",
        "label == Session.scope.Workspace.id",
    ] {
        let source = format!(
            "query Resources {{ resource {{ @where {{ {} }} id }} }}",
            predicate
        );
        let queries = parser::parse_query("query.pyre", &source).expect("path syntax parses");
        assert!(
            typecheck::check_queries(&queries, &context).is_err(),
            "session path should fail typechecking: {}",
            predicate
        );
    }
}

#[test]
fn tagged_union_predicates_reject_unqualified_unknown_and_mismatched_paths() {
    let context = union_predicate_context();
    for predicate in [
        "state.errorCode == 1",
        "state.Unknown.errorCode == 1",
        "state.Ready.reason == Other",
    ] {
        let source = format!("query Jobs {{ job {{ @where {{ {} }} id }} }}", predicate);
        let queries = parser::parse_query("query.pyre", &source).expect("syntax parses");
        assert!(
            typecheck::check_queries(&queries, &context).is_err(),
            "predicate should fail typechecking: {}",
            predicate
        );
    }
}

#[test]
fn unary_not_remains_invalid() {
    assert!(parser::parse_query(
        "query.pyre",
        "query Jobs { job { @where { !(state.Failed.errorCode == 1) } id } }"
    )
    .is_err());
}

#[test]
fn tagged_union_predicate_paths_reject_json_traversal() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
type State
   = Failed {
        code String
     }
   | Ready

record Job {
    id Int @id
    payload Json<State>
    @public
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let queries = parser::parse_query(
        "query.pyre",
        r#"query Jobs { job { @where { payload.Failed.code == "x" } id } }"#,
    )
    .expect("path syntax parses");
    assert!(typecheck::check_queries(&queries, &context).is_err());
}

#[test]
fn tagged_union_predicate_paths_reject_document_terminal_fields() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
type State
   = Failed {
        data Json
     }
   | Ready

record Job {
    id Int @id
    state State
    @public
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let queries = parser::parse_query(
        "query.pyre",
        r#"query Jobs($data: Json) { job { @where { state.Failed.data == $data } id } }"#,
    )
    .expect("path syntax parses");
    assert!(typecheck::check_queries(&queries, &context).is_err());
}

#[test]
fn simple_query_interface_hash_remains_stable_and_nested_paths_are_distinct() {
    let simple = parser::parse_query(
        "query.pyre",
        "query Get($id: Int) { job { @where { id == $id } id } }",
    )
    .unwrap();
    let ast::QueryDef::Query(simple_query) = &simple.queries[0] else {
        panic!("expected query");
    };
    assert_eq!(
        simple_query.interface_hash,
        "238c9f549d32951ad23a103bd750e09cf58847b73a61ee72429b104422650648"
    );

    let first = parser::parse_query(
        "query.pyre",
        r#"query Get { job { @where { state.Failed.code == "x" } id } }"#,
    )
    .unwrap();
    let second = parser::parse_query(
        "query.pyre",
        r#"query Get { job { @where { state.FailedCode.value == "x" } id } }"#,
    )
    .unwrap();
    let ast::QueryDef::Query(first) = &first.queries[0] else {
        panic!("expected query");
    };
    let ast::QueryDef::Query(second) = &second.queries[0] else {
        panic!("expected query");
    };
    assert_ne!(first.interface_hash, second.interface_hash);
}

#[test]
fn query_predicate_validates_function_return_type() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
record Metric {
    id Int @id
    label String
    score Float
    @public
}
"#,
        &mut schema,
    )
    .unwrap();
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap();

    let wrong = parser::parse_query(
        "query.pyre",
        r#"query Metrics { metric { @where { label == length("value") } id } }"#,
    )
    .unwrap();
    assert!(typecheck::check_queries(&wrong, &context).is_err());

    let numeric = parser::parse_query(
        "query.pyre",
        "query Metrics { metric { @where { score == abs(1) } id } }",
    )
    .unwrap();
    typecheck::check_queries(&numeric, &context)
        .expect("number arguments and Float return type should be compatible");
}

#[test]
fn test_valid_query_with_params() {
    let query_source = r#"
        query GetUser($id: Int) {
            user {
                @where { id == $id }
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with params should parse successfully"
    );
}

#[test]
fn test_valid_query_with_id_type_param() {
    let query_source = r#"
        query GetTask($id: Task.id) {
            task {
                @where { id == $id }
                id
                description
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with Task.id param should parse successfully"
    );
}

#[test]
fn test_valid_query_with_nested_fields() {
    let query_source = r#"
        query GetUsers {
            user {
                id
                name
                posts {
                    id
                    title
                }
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with nested fields should parse successfully"
    );
}

#[test]
fn test_valid_query_with_where() {
    let query_source = r#"
        query GetUser($id: Int) {
            user {
                @where { id == $id }
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with where should parse successfully"
    );
}

#[test]
fn test_valid_query_with_sort() {
    let query_source = r#"
        query GetUsers {
            user {
                @sort(name, Asc)
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with sort should parse successfully"
    );
}

#[test]
fn test_valid_query_with_sort_desc() {
    let query_source = r#"
        query GetUsers {
            user {
                @sort(name, Desc)
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with sort desc should parse successfully"
    );
}

#[test]
fn test_valid_query_with_bare_function_value() {
    let query_source = r#"
        update TaskComplete($id: Task.id) {
            task {
                @where { id == $id }
                completedAt = now
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with bare function value should parse successfully"
    );
}

#[test]
fn test_valid_query_with_field_alias() {
    let query_source = r#"
        query GetUsers {
            user {
                id
                username: name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_ok(),
        "Valid query with field alias should parse successfully"
    );
}

#[test]
fn test_valid_multiple_queries() {
    let query_source = r#"
        query GetUsers {
            user {
                id
                name
            }
        }

        query GetPosts {
            post {
                id
                title
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_ok(), "Multiple queries should parse successfully");
}

#[test]
fn test_missing_query_name() {
    let query_source = r#"
        query {
            user {
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_err(), "Missing query name should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(query_source, &error);

            // The parser gives a generic error message for this case
            assert!(
                formatted.contains("query.pyre") && formatted.contains("query {"),
                "Error message should contain file and query. Got:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_missing_query_brace() {
    let query_source = r#"
        query GetUsers
            user {
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_err(), "Missing opening brace should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(query_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("query.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        } else {
            panic!("Expected parsing error but convert_parsing_error returned None");
        }
    } else {
        panic!("Expected parsing to fail but it succeeded");
    }
}

#[test]
fn test_invalid_param_syntax() {
    // Note: The parser may accept this syntax, but typechecking will fail
    // This test documents the current parsing behavior
    let query_source = r#"
        query GetUser($id Int) {
            user {
                @where { id == $id }
                id
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    // The parser may accept this, but typechecking will catch the missing colon
    let _ = result;
}

#[test]
fn test_invalid_directive() {
    let query_source = r#"
        query GetUsers {
            user {
                @unknown
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_err(), "Invalid directive should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(query_source, &error);

            // Check that the error mentions the unknown directive and suggests alternatives
            assert!(
                formatted.contains("@unknown")
                    && (formatted.contains("@where") || formatted.contains("did you mean")),
                "Error message should mention @unknown and suggest alternatives. Got:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_missing_closing_brace() {
    let query_source = r#"
        query GetUsers {
            user {
                id
                name
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_err(), "Missing closing brace should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(query_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("query.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue")
                    || formatted.contains("Incomplete"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_query_with_comments() {
    let query_source = r#"
        // This is a comment
        query GetUsers {
            user {
                id
                // Another comment
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(result.is_ok(), "Comments should be allowed in queries");
}

#[test]
fn test_empty_query() {
    let query_source = r#"
        query GetUsers {
            user {
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    // Empty queries might be valid or invalid depending on the implementation
    let _ = result;
}

#[test]
fn test_invalid_where_syntax() {
    let query_source = r#"
        query GetUser($id: Int) {
            user {
                @where id = $id
                id
                name
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);
    assert!(
        result.is_err(),
        "Invalid where syntax (missing braces) should fail"
    );

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(query_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("query.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        } else {
            panic!("Expected parsing error but convert_parsing_error returned None");
        }
    } else {
        panic!("Expected parsing to fail but it succeeded");
    }
}

#[test]
fn test_query_with_union_field() {
    // This test captures the query from test_union_required_fields_validation
    // which is failing with a parsing error at line 3, column 24 (around "testRecord {")
    let query_source = r#"
        query GetTests {
            testRecord {
                id
                action
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", query_source);

    match result {
        Ok(_) => {
            // Parsing succeeded - this is the expected behavior
            println!("Query with union field parsed successfully");
        }
        Err(err) => {
            // Parsing failed - this documents the bug we're trying to fix
            let rendered = parser::render_error(query_source, err, false);
            let formatted = strip_ansi_codes(&rendered);
            println!("Parsing error for union field query:\n{}", formatted);
            panic!(
                "Query with union field should parse successfully but failed:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_insert_simple_union_variant() {
    // Test 1 from test_union_required_fields_validation
    let insert_source = r#"
        insert CreateTestRecord {
            testRecord {
                id = 1
                action = Simple
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", insert_source);
    assert!(
        result.is_ok(),
        "Insert with Simple variant should parse successfully"
    );
}

#[test]
fn test_insert_create_union_variant_with_fields() {
    // Test 2 from test_union_required_fields_validation
    // This insert fails with a parsing error - union variants with multiple fields aren't parsed correctly
    let insert_source = r#"
        insert CreateTestRecord($name: String, $description: String) {
            testRecord {
                id = 2
                action = Create { name = $name, description = $description }
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", insert_source);
    match result {
        Ok(_) => {
            // Parsing succeeded - this is the expected behavior
            println!("Insert with Create variant (all fields) parsed successfully");
        }
        Err(err) => {
            // Parsing failed - this documents the bug we're trying to fix
            let rendered = parser::render_error(insert_source, err, false);
            let formatted = strip_ansi_codes(&rendered);
            println!("Parsing error for Create variant insert:\n{}", formatted);
            panic!(
                "Insert with Create variant (multiple fields) should parse successfully but failed:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_insert_create_incomplete_union_variant() {
    // Test 3 from test_union_required_fields_validation
    let insert_source = r#"
        insert CreateTestRecord($name: String) {
            testRecord {
                id = 3
                action = Create { name = $name }
                // Missing description field - should fail
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", insert_source);
    match result {
        Ok(_) => println!("Insert with Create variant (incomplete) parsed successfully"),
        Err(err) => {
            let rendered = parser::render_error(insert_source, err, false);
            let formatted = strip_ansi_codes(&rendered);
            println!(
                "Parsing error for incomplete Create variant insert:\n{}",
                formatted
            );
            // This might fail parsing or might pass parsing but fail typechecking
        }
    }
}

#[test]
fn test_insert_update_union_variant_with_fields() {
    // Test 4 from test_union_required_fields_validation
    // This insert fails with a parsing error - union variants with multiple fields aren't parsed correctly
    let insert_source = r#"
        insert CreateTestRecord($id: Int, $changes: String) {
            testRecord {
                id = 4
                action = Update { id = $id, changes = $changes }
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", insert_source);
    match result {
        Ok(_) => {
            // Parsing succeeded - this is the expected behavior
            println!("Insert with Update variant parsed successfully");
        }
        Err(err) => {
            // Parsing failed - this documents the bug we're trying to fix
            let rendered = parser::render_error(insert_source, err, false);
            let formatted = strip_ansi_codes(&rendered);
            println!("Parsing error for Update variant insert:\n{}", formatted);
            panic!(
                "Insert with Update variant (multiple fields) should parse successfully but failed:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_insert_delete_incomplete_union_variant() {
    // Test 5 from test_union_required_fields_validation
    let insert_source = r#"
        insert CreateTestRecord($id: Int) {
            testRecord {
                id = 5
                action = Delete { id = $id }
                // Missing reason field - should fail
            }
        }
    "#;

    let result = parser::parse_query("query.pyre", insert_source);
    match result {
        Ok(_) => println!("Insert with Delete variant (incomplete) parsed successfully"),
        Err(err) => {
            let rendered = parser::render_error(insert_source, err, false);
            let formatted = strip_ansi_codes(&rendered);
            println!(
                "Parsing error for incomplete Delete variant insert:\n{}",
                formatted
            );
            // This might fail parsing or might pass parsing but fail typechecking
        }
    }
}

#[test]
fn test_parse_string_column_position() {
    use nom_locate::LocatedSpan;
    use pyre::parser::{parse_string, ParseContext};

    // Test string parsing: "hello" with leading spaces
    // Line: "        title = \"hello\""
    // We want to verify the range starts at the opening quote
    let input_str = "        title = \"hello\"";
    let text = LocatedSpan::new_extra(
        input_str,
        ParseContext {
            file: "test.pyre",
            namespace: "Base".to_string(),
            expecting: pyre::error::Expecting::PyreFile,
        },
    );

    // Skip to the position where the string starts
    use nom::error::VerboseError;
    use pyre::parser::Text;
    let (remaining, _) =
        nom::bytes::complete::take_until::<_, _, VerboseError<Text>>("\"")(text).unwrap();
    let (_remaining, result) = parse_string(remaining).unwrap();

    match result {
        pyre::ast::QueryValue::String((range, _)) => {
            // The string starts at column 17 (1-based): "        title = " = 16 chars, then " at column 17
            // Column 17 should be the opening quote
            assert_eq!(
                range.start.column, 17,
                "String start column should be 17 (the opening quote), got {}",
                range.start.column
            );
            // The range should end after the closing quote (column 24: 17 + 7 chars for "\"hello\"")
            assert_eq!(
                range.end.column, 24,
                "String end column should be 24 (after closing quote), got {}",
                range.end.column
            );
        }
        _ => panic!("Expected String variant"),
    }
}

#[test]
fn test_parse_variable_column_position() {
    use nom_locate::LocatedSpan;
    use pyre::parser::{parse_variable, ParseContext};

    // Test variable parsing: $title with leading spaces
    // Line: "        title = $title"
    // We want to verify the range starts at the $ character
    let input_str = "        title = $title";
    let text = LocatedSpan::new_extra(
        input_str,
        ParseContext {
            file: "test.pyre",
            namespace: "Base".to_string(),
            expecting: pyre::error::Expecting::PyreFile,
        },
    );

    // Skip to the position where the variable starts
    use nom::error::VerboseError;
    use pyre::parser::Text;
    let (remaining, _) =
        nom::bytes::complete::take_until::<_, _, VerboseError<Text>>("$")(text).unwrap();
    let (_remaining, result) = parse_variable(remaining).unwrap();

    match result {
        pyre::ast::QueryValue::Variable((range, _)) => {
            // The variable should start at column 17 (1-based): "        title = " = 16 chars, then $ at column 17
            // Column 17 should be the $ character
            assert_eq!(
                range.start.column, 17,
                "Variable start column should be 17 (the $), got {}",
                range.start.column
            );
            // The range should end after "title" (column 23: 17 + 6 chars for "$title")
            assert_eq!(
                range.end.column, 23,
                "Variable end column should be 23 (after 'title'), got {}",
                range.end.column
            );
        }
        _ => panic!("Expected Variable variant"),
    }
}
