use pyre::ast;
use pyre::error;
use pyre::error::ErrorType;
use pyre::parser;
use pyre::typecheck;

fn check_schema_and_get_layers(schema_source: &str) -> std::collections::HashMap<String, usize> {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Extract sync layers for each table
    let mut layers = std::collections::HashMap::new();
    for (_record_name, table) in &context.tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        layers.insert(table_name, table.sync_layer);
    }

    layers
}

fn checked_context(schema_source: &str) -> typecheck::Context {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    typecheck::check_schema(&database).expect("Schema should typecheck")
}

#[test]
fn integer_foreign_key_parameters_remain_compatible_with_record_id_aliases() {
    let context = checked_context(
        r#"
record Game {
    @public
    id Id.Int @id
}

record MapScene {
    @public
    id Id.Int @id
    gameId Game.id
}
"#,
    );
    let query_list = parser::parse_query(
        "query.pyre",
        r#"
insert CreateMapScene($gameId: Game.id) {
    mapScene {
        gameId = $gameId
    }
}
"#,
    )
    .expect("query parses");

    typecheck::check_queries(&query_list, &context)
        .expect("integer foreign key should accept its record id alias");
}

#[test]
fn timestamps_add_managed_timestamp_fields() {
    let context = checked_context(
        r#"
record Note {
    @public
    id Int @id
    body String
    @timestamps
}
    "#,
    );

    let note = context.tables.get("note").expect("note table exists");
    let fields = ast::collect_columns(&note.record.fields);

    assert!(fields
        .iter()
        .any(|column| { column.name == "createdAt" && ast::is_created_at(column) }));
    assert!(fields
        .iter()
        .any(|column| { column.name == "updatedAt" && ast::is_updated_at(column) }));
}

#[test]
fn managed_fields_and_integer_ids_cannot_be_set_in_insert_mutations() {
    let context = checked_context(
        r#"
record Note {
    @public
    id Int @id
    body String
    createdAt DateTime @createdAt
    updatedAt DateTime @updatedAt
}
    "#,
    );

    let query_list = parser::parse_query(
        "query.pyre",
        r#"
insert CreateNote($id: Int, $body: String, $createdAt: DateTime, $updatedAt: DateTime) {
    note {
        id = $id
        body = $body
        createdAt = $createdAt
        updatedAt = $updatedAt
    }
}
    "#,
    )
    .expect("query parses");

    let errors = match typecheck::check_queries(&query_list, &context) {
        Ok(_) => panic!("managed writes should fail"),
        Err(errors) => errors,
    };
    let managed_fields = errors
        .iter()
        .filter_map(|error| match &error.error_type {
            ErrorType::ManagedColumnCannotBeSet { field, .. } => Some(field.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(managed_fields.contains(&"id"));
    assert!(managed_fields.contains(&"createdAt"));
    assert!(managed_fields.contains(&"updatedAt"));
}

#[test]
fn generated_create_excludes_integer_ids_and_managed_timestamps() {
    let context = checked_context(
        r#"
record Note {
    @public
    id Int @id
    body String
    createdAt DateTime @createdAt
    updatedAt DateTime @updatedAt
}
    "#,
    );
    let mut query_list = ast::QueryList { queries: vec![] };

    pyre::generated_queries::append_generated_crud_queries(&mut query_list, &context);

    let create = query_list
        .queries
        .iter()
        .find_map(|query| match query {
            ast::QueryDef::Query(query) if query.name == "NoteCreate" => Some(query),
            _ => None,
        })
        .expect("generated create exists");
    let arg_names = create
        .args
        .iter()
        .map(|arg| arg.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(arg_names, vec!["body"]);
}

#[test]
fn uuid_primary_ids_remain_settable_on_insert() {
    let context = checked_context(
        r#"
record Note {
    @public
    id Id.Uuid @id
    body String
}
    "#,
    );

    let query_list = parser::parse_query(
        "query.pyre",
        r#"
insert CreateNote($id: Note.id, $body: String) {
    note {
        id = $id
        body = $body
    }
}
    "#,
    )
    .expect("query parses");

    typecheck::check_queries(&query_list, &context).expect("UUID id should be caller-settable");
}

#[test]
fn test_tablename_rejects_unsafe_sql_identifier() {
    let schema_source = r#"
record User {
    @tablename("users-with-dash")
    @public
    id Int @id
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let errors = typecheck::check_schema(&database).expect_err("schema should fail typecheck");

    assert!(errors.iter().any(|error| matches!(
        &error.error_type,
        ErrorType::InvalidTypeUsage { message }
            if message.contains("Invalid @tablename value")
    )));
}

#[test]
fn test_list_type_must_be_wrapped_in_json() {
    let schema_source = r#"
record User {
    @public
    id Int @id
    tags List<String>
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_err(),
        "List<T> outside Json<T> should fail schema typechecking"
    );
}

#[test]
fn test_dict_type_must_be_wrapped_in_json() {
    let schema_source = r#"
record User {
    @public
    id Int @id
    metadata Dict<String>
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_err(),
        "Dict<T> outside Json<T> should fail schema typechecking"
    );
}

#[test]
fn test_json_typed_field_accepts_nested_document_only_custom_types() {
    let schema_source = r#"
type GameState
   = GameState {
        groups List<GroupState>
     }

type GroupState
   = GroupState {
        members List<String>
     }

record Game {
    @public
    id Int @id
    state Json<GameState>
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_ok(),
        "Json<T> fields should allow nested custom types containing List<T>"
    );
}

#[test]
fn test_json_typed_field_accepts_variant_fields_with_same_name_and_different_types() {
    let schema_source = r#"
type Attribute
   = AttributeInt {
        value Int
     }
   | AttributeBool {
        value Bool
     }

record Entity {
    @public
    id Int @id
    attrs Json<Dict<Attribute>>
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_ok(),
        "Json<T> fields should allow tagged JSON variants to reuse field names with different types"
    );
}

#[test]
fn test_json_typed_field_accepts_recursive_custom_type() {
    let schema_source = r#"
type Attribute
   = AttributeInt {
        value Int
     }
   | AttributeBool {
        value Bool
     }
   | AttributeCustom {
        variant String
        fields  Dict<Attribute>
     }

record Entity {
    @public
    id Int @id
    attrs Json<Dict<Attribute>>
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_ok(),
        "Json<T> fields should allow recursive tagged JSON types"
    );
}

#[test]
fn test_tagged_variant_accepts_nested_json_typed_field() {
    let schema_source = r#"
type DocumentVisibility
   = DocumentVisibleToEveryone
   | DocumentHidden
   | DocumentVisibleToSelectedUsers { userIds Json<List<String>> }

record Document {
    @public
    id Int @id
    visibility DocumentVisibility
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_ok(),
        "Tagged variants should allow fields typed as Json<T>"
    );
}

#[test]
fn test_json_typed_field_cannot_have_default() {
    let schema_source = r#"
type Preferences
   = Theme {
        theme String
     }

record User {
    @public
    id Int @id
    preferences Json<Preferences> @default(null)
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_err(),
        "Json<T> fields should reject @default(...)"
    );
}

#[test]
fn test_raw_json_field_cannot_have_default() {
    let schema_source = r#"
record User {
    @public
    id Int @id
    payload JSON @default(null)
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_err(),
        "Raw JSON fields should reject @default(...)"
    );
}

#[test]
fn test_json_typed_field_cannot_contain_relationships() {
    let schema_source = r#"
type Preferences
   = Owned {
        ownerId User.id
        owner @link(ownerId, User.id)
     }

record User {
    @public
    id Int @id
    preferences Json<Preferences>
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    assert!(
        schema_result.is_err(),
        "Json<T> payloads should reject relationship fields"
    );
}

#[test]
fn test_json_typed_field_accepts_union_literal_in_insert() {
    let schema_source = r#"
type Lifecycle
   = Running
   | Finished {
        reason String
     }

record Event {
    @public
    id Int @id
    payload Json<Lifecycle>
}
    "#;

    let query_source = r#"
insert SeedEvent {
    event {
        payload = Finished {
            reason = "done"
        }
    }
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let context = typecheck::check_schema(&database).expect("Schema should typecheck");
    let query_list = parser::parse_query("query.pyre", query_source).expect("Query should parse");

    let query_result = typecheck::check_queries(&query_list, &context);
    assert!(
        query_result.is_ok(),
        "Json<T> fields should accept inline tagged union values in writes"
    );
}

#[test]
fn test_variant_field_collision_highlight_positions() {
    let schema_source = r#"
type DevEventType
   = Mouse {
        kind DevMouseKind
     }
   | Keyboard {
        kind DevKeyKind
     }

type DevMouseKind
   = Click
   | Move

type DevKeyKind
   = Down
   | Up

record Event {
    @public
    id Int @id
    eventType DevEventType
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    match schema_result {
        Ok(_) => {
            panic!("Expected schema typechecking to fail due to variant field type collision");
        }
        Err(errors) => {
            let collision_error = errors
                .iter()
                .find(|e| matches!(&e.error_type, ErrorType::VariantFieldTypeCollision { .. }));

            assert!(
                collision_error.is_some(),
                "Expected VariantFieldTypeCollision error"
            );

            let error = collision_error.unwrap();
            let formatted = error::format_error(schema_source, error, false);
            let lines: Vec<&str> = formatted.lines().collect();

            let caret_lines: Vec<&str> = lines
                .iter()
                .filter(|line| line.contains('^'))
                .cloned()
                .collect();
            assert!(
                caret_lines.len() >= 2,
                "Expected at least two highlighted ranges. Got:\n{}",
                formatted
            );

            assert!(
                !formatted.contains("Mouse.kind") && !formatted.contains("Keyboard.kind"),
                "Expected no variant summary lines after the message. Got:\n{}",
                formatted
            );

            let last_caret_line = lines
                .iter()
                .rposition(|line| line.contains('^'))
                .expect("Expected at least one caret line");
            let trailing_context = lines
                .iter()
                .skip(last_caret_line + 1)
                .any(|line| line.trim_start().starts_with('|'));
            assert!(
                !trailing_context,
                "Expected no trailing context lines after variants. Got:\n{}",
                formatted
            );

            assert!(
                formatted.contains("Mouse {") && formatted.contains("Keyboard {"),
                "Expected both variant names to be visible. Got:\n{}",
                formatted
            );

            let mouse_line_index = lines
                .iter()
                .position(|line| line.contains("Mouse {"))
                .expect("Expected Mouse variant line");
            let mouse_field_line_index = lines
                .iter()
                .position(|line| line.contains("DevMouseKind"))
                .expect("Expected DevMouseKind line");
            assert!(
                mouse_line_index < mouse_field_line_index,
                "Mouse variant line should appear before its field type. Got:\n{}",
                formatted
            );
            if let Some(next_line) = lines.get(mouse_line_index + 1) {
                assert!(
                    !next_line.contains('^'),
                    "Variant name line should not be highlighted. Got:\n{}",
                    formatted
                );
            }

            let keyboard_line_index = lines
                .iter()
                .position(|line| line.contains("Keyboard {"))
                .expect("Expected Keyboard variant line");
            let keyboard_field_line_index = lines
                .iter()
                .position(|line| line.contains("DevKeyKind"))
                .expect("Expected DevKeyKind line");
            assert!(
                keyboard_line_index < keyboard_field_line_index,
                "Keyboard variant line should appear before its field type. Got:\n{}",
                formatted
            );
            if let Some(next_line) = lines.get(keyboard_line_index + 1) {
                assert!(
                    !next_line.contains('^'),
                    "Variant name line should not be highlighted. Got:\n{}",
                    formatted
                );
            }

            let check_alignment = |needle: &str| {
                let line_index = lines
                    .iter()
                    .position(|line| line.contains(needle))
                    .expect("Expected line containing field type");
                let code_line = lines[line_index];
                let caret_line = lines
                    .get(line_index + 1)
                    .expect("Expected caret line after field line");

                let code_index = code_line
                    .find(needle)
                    .expect("Expected to find needle in code line");
                let caret_index = caret_line
                    .find('^')
                    .expect("Expected to find caret in caret line");

                assert_eq!(
                    caret_index, code_index,
                    "Caret should align with '{}'. Got:\n{}",
                    needle, formatted
                );
            };

            check_alignment("DevMouseKind");
            check_alignment("DevKeyKind");
        }
    }
}

#[test]
fn test_duplicate_variant_error_renders_code() {
    let schema_source = r#"
type DevSource
   = Browser
   | Server
   | Worker
   | Root
   | Browser
    "#;

    let mut schema = ast::Schema::default();
    parser::run("pyre/schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    match schema_result {
        Ok(_) => {
            panic!("Expected schema typechecking to fail due to duplicate variant");
        }
        Err(errors) => {
            let duplicate_error = errors
                .iter()
                .find(|e| matches!(&e.error_type, ErrorType::DuplicateVariant { .. }));

            assert!(duplicate_error.is_some(), "Expected DuplicateVariant error");

            let error = duplicate_error.unwrap();
            assert_eq!(
                error.filepath, "pyre/schema.pyre",
                "Expected error filepath to match schema file"
            );

            let formatted = error::format_error(schema_source, error, false);
            assert!(
                formatted.contains("Browser") && formatted.contains("^^^^^^"),
                "Expected highlighted code in error output. Got:\n{}",
                formatted
            );
        }
    }
}

#[test]
fn test_invalid_uuid_default_is_caught_by_typechecker() {
    let schema_source = r#"
record Task {
    @public
    id Id.Uuid @id @default(uuid)
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("pyre/schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    match schema_result {
        Ok(_) => panic!("Expected schema typechecking to fail due to invalid default"),
        Err(errors) => {
            let default_error = errors
                .iter()
                .find(|e| matches!(&e.error_type, ErrorType::InvalidColumnDefault { .. }));

            assert!(
                default_error.is_some(),
                "Expected InvalidColumnDefault error, got: {:?}",
                errors
            );

            let formatted = error::format_error(schema_source, default_error.unwrap(), false);
            assert!(
                formatted.contains("Id.Uuid")
                    && formatted.contains("is not allowed to have an @default")
            );
            assert!(!formatted.contains("Allowed defaults here"));
        }
    }
}

#[test]
fn test_invalid_id_int_default_has_no_allowed_defaults() {
    let schema_source = r#"
record Counter {
    @public
    id Id.Int @id @default(1)
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("pyre/schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    match schema_result {
        Ok(_) => panic!("Expected schema typechecking to fail due to invalid default"),
        Err(errors) => {
            let default_error = errors
                .iter()
                .find(|e| matches!(&e.error_type, ErrorType::InvalidColumnDefault { .. }))
                .expect("Expected InvalidColumnDefault error");

            let formatted = error::format_error(schema_source, default_error, false);
            assert!(
                formatted.contains("Id.Int")
                    && formatted.contains("is not allowed to have an @default")
            );
            assert!(!formatted.contains("Allowed defaults here"));
        }
    }
}

#[test]
fn test_simple_linear_dependency() {
    // A -> B -> C
    // Expected: A=0, B=1, C=2
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    bId Int
    b @link(bId, B.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
    assert_eq!(layers.get("c"), Some(&2), "C should be layer 2");
}

#[test]
fn test_multiple_dependencies() {
    // A -> B, A -> C
    // Expected: A=0, B=1, C=1
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
    assert_eq!(layers.get("c"), Some(&1), "C should be layer 1");
}

#[test]
fn test_circular_dependency() {
    // A <-> B (circular)
    // Expected: A=0, B=0 (same layer due to cycle)
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    bId Int?
    b @link(bId, B.id)
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int?
    a @link(aId, A.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    let a_layer = layers.get("a").expect("Table 'a' should exist");
    let b_layer = layers.get("b").expect("Table 'b' should exist");

    assert_eq!(
        a_layer, b_layer,
        "A and B should have the same layer due to circular dependency"
    );
    assert_eq!(a_layer, &0, "Circular dependency should be in layer 0");
}

#[test]
fn test_independent_tables() {
    // A, B, C (no dependencies)
    // Expected: A=0, B=0, C=0
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    @public
}

record C {
    @tablename("c")
    id Int @id
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&0), "B should be layer 0");
    assert_eq!(layers.get("c"), Some(&0), "C should be layer 0");
}

#[test]
fn test_complex_graph() {
    // A -> B -> D
    // A -> C -> D
    // Expected: A=0, B=1, C=1, D=2
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record D {
    @tablename("d")
    id Int @id
    bId Int?
    b @link(bId, B.id)
    cId Int?
    c @link(cId, C.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
    assert_eq!(layers.get("c"), Some(&1), "C should be layer 1");
    assert_eq!(layers.get("d"), Some(&2), "D should be layer 2");
}

#[test]
fn test_three_way_cycle() {
    // A -> B -> C -> A (cycle)
    // Expected: A=0, B=0, C=0 (all same layer)
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    bId Int?
    b @link(bId, B.id)
    @public
}

record B {
    @tablename("b")
    id Int @id
    cId Int?
    c @link(cId, C.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    aId Int?
    a @link(aId, A.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    let a_layer = layers.get("a").expect("Table 'a' should exist");
    let b_layer = layers.get("b").expect("Table 'b' should exist");
    let c_layer = layers.get("c").expect("Table 'c' should exist");

    assert_eq!(a_layer, b_layer, "A and B should have the same layer");
    assert_eq!(b_layer, c_layer, "B and C should have the same layer");
    assert_eq!(a_layer, &0, "Cycle should be in layer 0");
}

#[test]
fn test_cycle_with_external_dependency() {
    // A -> B <-> C (B and C cycle, A depends on nothing)
    // D -> B (D depends on B)
    // Expected: A=0, B=0, C=0 (cycle), D=1
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    cId Int?
    c @link(cId, C.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    bId Int?
    b @link(bId, B.id)
    @public
}

record D {
    @tablename("d")
    id Int @id
    bId Int
    b @link(bId, B.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");

    let b_layer = layers.get("b").expect("Table 'b' should exist");
    let c_layer = layers.get("c").expect("Table 'c' should exist");
    assert_eq!(
        b_layer, c_layer,
        "B and C should have the same layer (cycle)"
    );
    assert_eq!(b_layer, &0, "Cycle should be in layer 0");

    assert_eq!(
        layers.get("d"),
        Some(&1),
        "D should be layer 1 (depends on B in cycle)"
    );
}

#[test]
fn test_deep_nested_dependencies() {
    // A -> B -> C -> D -> E
    // Expected: A=0, B=1, C=2, D=3, E=4
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    bId Int
    b @link(bId, B.id)
    @public
}

record D {
    @tablename("d")
    id Int @id
    cId Int
    c @link(cId, C.id)
    @public
}

record E {
    @tablename("e")
    id Int @id
    dId Int
    d @link(dId, D.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
    assert_eq!(layers.get("c"), Some(&2), "C should be layer 2");
    assert_eq!(layers.get("d"), Some(&3), "D should be layer 3");
    assert_eq!(layers.get("e"), Some(&4), "E should be layer 4");
}

#[test]
fn test_multiple_links_same_table() {
    // A -> B (via link1)
    // A -> B (via link2)
    // Expected: A=0, B=1
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId1 Int
    a1 @link(aId1, A.id)
    aId2 Int
    a2 @link(aId2, A.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
}

#[test]
fn test_table_with_no_links() {
    // A has links, B has no links
    // Expected: Both should have valid layers
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    name String
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
    assert_eq!(
        layers.get("c"),
        Some(&0),
        "C should be layer 0 (no dependencies)"
    );
}

#[test]
fn test_diamond_pattern() {
    //   A
    //  / \
    // B   C
    //  \ /
    //   D
    // Expected: A=0, B=1, C=1, D=2
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    @public
}

record B {
    @tablename("b")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record C {
    @tablename("c")
    id Int @id
    aId Int
    a @link(aId, A.id)
    @public
}

record D {
    @tablename("d")
    id Int @id
    bId Int?
    b @link(bId, B.id)
    cId Int?
    c @link(cId, C.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(layers.get("a"), Some(&0), "A should be layer 0");
    assert_eq!(layers.get("b"), Some(&1), "B should be layer 1");
    assert_eq!(layers.get("c"), Some(&1), "C should be layer 1");
    assert_eq!(layers.get("d"), Some(&2), "D should be layer 2");
}

#[test]
fn test_self_referential_table() {
    // A -> A (self-reference)
    // Expected: A=0 (self-cycle)
    let schema = r#"
record A {
    @tablename("a")
    id Int @id
    parentId Int?
    parent @link(parentId, A.id)
    @public
}
"#;

    let layers = check_schema_and_get_layers(schema);

    assert_eq!(
        layers.get("a"),
        Some(&0),
        "A should be layer 0 (self-cycle)"
    );
}

#[test]
fn test_nullable_query_parameter_typechecking() {
    // Test that nullable query parameters work correctly with nullable columns
    // This test verifies that a parameter defined as "String?" can be used
    // against a nullable String column without typechecking errors.
    let schema = r#"
record User {
    id   Int    @id
    name String?
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Query with nullable parameter used against nullable column
    let query_source = r#"
        query GetUserByName($name: String?) {
            user {
                @where { name == $name }
                id
                name
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            // This should succeed - nullable param should work with nullable column
            println!("Nullable parameter typechecking passed as expected");
        }
        Err(errors) => {
            // If this fails, it demonstrates the bug
            let mut error_messages = Vec::new();
            for error in &errors {
                error_messages.push(format!("{:?}", error));
            }
            panic!(
                "Nullable parameter typechecking failed (this may indicate a bug):\n{}",
                error_messages.join("\n")
            );
        }
    }
}

#[test]
fn test_nullable_query_parameter_with_non_nullable_column() {
    // Test that nullable query parameters cannot be used with non-nullable columns
    // Null is not a valid value for non-nullable types
    let schema = r#"
record User {
    id   Int    @id
    name String
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Query with nullable parameter used against non-nullable column
    // This should fail - nullable params cannot be used with non-nullable columns
    let query_source = r#"
        query GetUserByName($name: String?) {
            user {
                @where { name == $name }
                id
                name
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            panic!("Nullable parameter with non-nullable column should fail typechecking");
        }
        Err(errors) => {
            // Should fail with a type mismatch error
            let has_type_mismatch = errors
                .iter()
                .any(|e| matches!(&e.error_type, error::ErrorType::TypeMismatch { .. }));
            assert!(
                has_type_mismatch,
                "Should have a TypeMismatch error for nullable param with non-nullable column. Errors: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_json_param_only() {
    let schema = r#"
record Task {
    @public
    id Id.Int @id
    metadata Json
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    let valid_query = r#"
        update SetTaskMetadata($id: Task.id, $metadata: Json) {
            task {
                @where { id == $id }
                metadata = $metadata
            }
        }
    "#;

    let valid_query_list =
        parser::parse_query("query.pyre", valid_query).expect("Failed to parse query");
    let valid_result = typecheck::check_queries(&valid_query_list, &context);
    assert!(valid_result.is_ok(), "Valid Json literal should typecheck");

    let invalid_query = r#"
        update SetTaskMetadata($id: Task.id) {
            task {
                @where { id == $id }
                metadata = "{invalid}"
            }
        }
    "#;

    let invalid_query_list =
        parser::parse_query("query.pyre", invalid_query).expect("Failed to parse query");
    let invalid_result = typecheck::check_queries(&invalid_query_list, &context);

    match invalid_result {
        Ok(_) => panic!("Json literals should fail typechecking"),
        Err(errors) => {
            let has_type_mismatch = errors
                .iter()
                .any(|error| matches!(error.error_type, ErrorType::LiteralTypeMismatch { .. }));
            assert!(has_type_mismatch, "Expected LiteralTypeMismatch error");
        }
    }
}

#[test]
fn test_nullable_param_in_update_set_with_non_nullable_column() {
    // Test that nullable parameters cannot be used in SET operations with non-nullable columns
    let schema = r#"
record Post {
    id        Int    @id
    title     String
    content   String
    published Bool
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Update with nullable parameter used in SET against non-nullable column
    // This should fail - nullable params cannot be used with non-nullable columns
    let query_source = r#"
        update UpdatePost($id: Int, $title: String?) {
            post {
                @where { id == $id }
                title = $title
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            panic!("Nullable parameter in SET operation with non-nullable column should fail typechecking");
        }
        Err(errors) => {
            // Should fail with a type mismatch error
            let has_type_mismatch = errors
                .iter()
                .any(|e| matches!(&e.error_type, error::ErrorType::TypeMismatch { .. }));
            assert!(
                has_type_mismatch,
                "Should have a TypeMismatch error for nullable param in SET with non-nullable column. Errors: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_nullable_param_in_insert_set_with_non_nullable_column() {
    // Test that nullable parameters cannot be used in INSERT SET operations with non-nullable columns
    let schema = r#"
record Post {
    id        Int    @id
    title     String
    content   String
    published Bool
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Insert with nullable parameter used in SET against non-nullable column
    // This should fail - nullable params cannot be used with non-nullable columns
    let query_source = r#"
        insert CreatePost($title: String?) {
            post {
                title = $title
                content = "test"
                published = True
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            panic!("Nullable parameter in INSERT SET operation with non-nullable column should fail typechecking");
        }
        Err(errors) => {
            // Should fail with a type mismatch error
            let has_type_mismatch = errors
                .iter()
                .any(|e| matches!(&e.error_type, error::ErrorType::TypeMismatch { .. }));
            assert!(
                has_type_mismatch,
                "Should have a TypeMismatch error for nullable param in INSERT SET with non-nullable column. Errors: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_wildcard_disallowed_in_update() {
    let schema = r#"
record User {
    id   Int    @id
    name String
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    let query_source = r#"
        update UpdateUser($id: Int) {
            user {
                @where { id == $id }
                *
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => panic!("Wildcard should be disallowed in update queries"),
        Err(errors) => {
            let has_invalid_wildcard = errors.iter().any(|e| {
                matches!(
                    &e.error_type,
                    error::ErrorType::InvalidWildcardSelection {
                        reason: error::InvalidWildcardReason::UsedInInsertOrUpdate
                    }
                )
            });
            assert!(
                has_invalid_wildcard,
                "Expected InvalidWildcardSelection error. Errors: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_wildcard_disallowed_in_insert() {
    let schema = r#"
record User {
    id   Int    @id
    name String
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    let query_source = r#"
        insert CreateUser($name: String) {
            user {
                name = $name
                *
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => panic!("Wildcard should be disallowed in insert queries"),
        Err(errors) => {
            let has_invalid_wildcard = errors.iter().any(|e| {
                matches!(
                    &e.error_type,
                    error::ErrorType::InvalidWildcardSelection {
                        reason: error::InvalidWildcardReason::UsedInInsertOrUpdate
                    }
                )
            });
            assert!(
                has_invalid_wildcard,
                "Expected InvalidWildcardSelection error. Errors: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_non_nullable_param_with_nullable_column() {
    // Test that non-nullable parameters can be used with nullable columns
    // Non-null values are valid for nullable columns
    let schema = r#"
record User {
    id   Int    @id
    name String?
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Query with non-nullable parameter used against nullable column
    // This should succeed - non-null values are valid for nullable columns
    let query_source = r#"
        query GetUserByName($name: String) {
            user {
                @where { name == $name }
                id
                name
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            // This should succeed - non-nullable param should work with nullable column
            println!("Non-nullable parameter with nullable column passed as expected");
        }
        Err(errors) => {
            let mut error_messages = Vec::new();
            for error in &errors {
                error_messages.push(format!("{:?}", error));
            }
            panic!(
                "Non-nullable parameter with nullable column should succeed. Errors: {}",
                error_messages.join("\n")
            );
        }
    }
}

#[test]
fn test_non_nullable_param_in_update_set_with_nullable_column() {
    // Test that non-nullable parameters can be used in SET operations with nullable columns
    let schema = r#"
record Post {
    id        Int    @id
    title     String?
    content   String?
    published Bool?
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Update with non-nullable parameter used in SET against nullable column
    // This should succeed - non-null values are valid for nullable columns
    let query_source = r#"
        update UpdatePost($id: Int, $title: String) {
            post {
                @where { id == $id }
                title = $title
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            // This should succeed - non-nullable param should work with nullable column
            println!("Non-nullable parameter in SET with nullable column passed as expected");
        }
        Err(errors) => {
            let mut error_messages = Vec::new();
            for error in &errors {
                error_messages.push(format!("{:?}", error));
            }
            panic!(
                "Non-nullable parameter in SET with nullable column should succeed. Errors: {}",
                error_messages.join("\n")
            );
        }
    }
}

#[test]
fn test_non_nullable_param_in_insert_set_with_nullable_column() {
    // Test that non-nullable parameters can be used in INSERT SET operations with nullable columns
    let schema = r#"
record Post {
    id        Int    @id
    title     String?
    content   String?
    published Bool?
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Insert with non-nullable parameter used in SET against nullable column
    // This should succeed - non-null values are valid for nullable columns
    let query_source = r#"
        insert CreatePost($title: String) {
            post {
                title = $title
                content = "test"
                published = False
            }
        }
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            // This should succeed - non-nullable param should work with nullable column
            println!(
                "Non-nullable parameter in INSERT SET with nullable column passed as expected"
            );
        }
        Err(errors) => {
            let mut error_messages = Vec::new();
            for error in &errors {
                error_messages.push(format!("{:?}", error));
            }
            panic!(
                "Non-nullable parameter in INSERT SET with nullable column should succeed. Errors: {}",
                error_messages.join("\n")
            );
        }
    }
}

#[test]
fn test_type_mismatch_error_column_position() {
    // Test that error highlighting for type mismatch errors has correct column position
    let schema = r#"
record Post {
    id        Int    @id
    title     String
    content   String
    published Bool
    @public
}
"#;

    let mut schema_ast = ast::Schema::default();
    parser::run("schema.pyre", schema, &mut schema_ast).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema_ast],
    };

    let context = typecheck::check_schema(&database).expect("Failed to typecheck schema");

    // Update with nullable parameter used in SET against non-nullable column
    // This should fail with a type mismatch error
    // Using the same structure as the actual file to match real-world usage
    let query_source = r#"
update UpdatePost($id: Int, $title: String?) {
    post {
        @where { id == $id  }

        title = $title
    }
}
    "#;

    let query_list =
        parser::parse_query("query.pyre", query_source).expect("Failed to parse query");

    let result = typecheck::check_queries(&query_list, &context);

    match result {
        Ok(_) => {
            panic!("Expected typechecking to fail due to nullable param with non-nullable column");
        }
        Err(errors) => {
            // Check if the error is a TypeMismatch error
            let type_mismatch_error = errors
                .iter()
                .find(|e| matches!(&e.error_type, ErrorType::TypeMismatch { .. }));

            assert!(
                type_mismatch_error.is_some(),
                "Expected TypeMismatch error for nullable param with non-nullable column"
            );

            let error = type_mismatch_error.unwrap();
            // TypeMismatch errors have two locations: one for definition, one for usage
            assert!(
                error.locations.len() >= 2,
                "TypeMismatch error should have two locations (definition and usage), got {}",
                error.locations.len()
            );

            // Check the usage location (second location)
            let usage_location = &error.locations[1];

            assert!(
                !usage_location.primary.is_empty(),
                "Error location should have a primary range"
            );

            let primary_range = &usage_location.primary[0];
            // The line is: "        title = $title"
            // Counting: "        " = 8 spaces, "title" = 5 chars, " " = 1, "=" = 1, " " = 1
            // Total: 8 + 5 + 1 + 1 + 1 = 16 chars (0-based indices 0-15)
            // Column 17 (1-based) is where "$" starts
            // The parser correctly captures column 17 (the $), as verified by test_parse_variable_column_position
            let expected_start_column = 17; // Parser captures column 17 correctly
            assert_eq!(
                primary_range.start.column, expected_start_column,
                "Start column should be exactly {} (currently buggy, should be 17), got {}",
                expected_start_column, primary_range.start.column
            );
            // The range should cover "$title" (6 characters: $ + title)
            assert_eq!(
                primary_range.end.column - primary_range.start.column,
                6,
                "Range should cover '$title' (6 chars), got {}",
                primary_range.end.column - primary_range.start.column
            );
            assert_eq!(
                primary_range.end.column,
                expected_start_column + 6,
                "End column should be exactly {}, got {}",
                expected_start_column + 6,
                primary_range.end.column
            );
        }
    }
}

#[test]
fn test_permission_field_error_column_position() {
    // Test that error highlighting for unknown fields in permissions has correct column position
    let schema_source = r#"
session {
    userId Int
    role   String
}

record Post {
    @allow(query) { userId == Session.userId || published == True  }
    @watch

    id           Int     @id
    createdAt    DateTime @default(now)
    authorUserId Int
    title        String
    content      String
    published    Bool    @default(False)
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_result = typecheck::check_schema(&database);
    match schema_result {
        Ok(_) => {
            panic!("Expected schema typechecking to fail due to invalid permissions (userId doesn't exist on Post)");
        }
        Err(errors) => {
            // Check if the error is about userId not existing on Post in permissions
            let unknown_field_error = errors.iter().find(|e| {
                matches!(
                    &e.error_type,
                    ErrorType::UnknownField { found, record_name, .. }
                    if found == "userId" && record_name == "Post"
                )
            });

            assert!(
                unknown_field_error.is_some(),
                "Expected UnknownField error for userId on Post in permissions"
            );

            let error = unknown_field_error.unwrap();
            assert!(
                !error.locations.is_empty(),
                "Error should have at least one location"
            );

            // Verify the column position is correct
            // The field name "userId" should start at column 29 (1-based) in the line:
            // "    @allow(query, update) { userId == Session.userId  }"
            // Counting: "    @allow(query, update) { " = 28 chars, so userId starts at column 29
            let location = &error.locations[0];
            assert!(
                !location.primary.is_empty(),
                "Error location should have a primary range"
            );

            let primary_range = &location.primary[0];
            // The line is: "     @allow(query) { userId == Session.userId || published == True  }"
            // Counting: "     @allow(query) { " = 21 chars (0-based) = column 21 (0-based)
            // So userId starts at column 22 (0-based)
            let expected_start_column = 21;
            assert_eq!(
                primary_range.start.column, expected_start_column,
                "Start column should be exactly {}, got {}",
                expected_start_column, primary_range.start.column
            );
            // The range should cover the field name "userId" (6 characters)
            assert_eq!(
                primary_range.end.column - primary_range.start.column,
                6,
                "Range should cover 'userId' (6 chars), got {}",
                primary_range.end.column - primary_range.start.column
            );
            assert_eq!(
                primary_range.end.column,
                expected_start_column + 6,
                "End column should be exactly {}, got {}",
                expected_start_column + 6,
                primary_range.end.column
            );
        }
    }
}

#[test]
fn test_link_fails_when_table_exists_in_different_namespace() {
    let app_source = r#"
record User {
    id Int @id
    @public
}

record Post {
    id Int @id
    userId Int
    user @link(userId, Auth.User.id)
    @public
}
    "#;

    let auth_source = r#"
record Account {
    id Int @id
    @public
}
    "#;

    let mut app_schema = ast::Schema::default();
    app_schema.namespace = "App".to_string();
    parser::run("pyre/schema/App/schema.pyre", app_source, &mut app_schema)
        .expect("App schema should parse");

    let mut auth_schema = ast::Schema::default();
    auth_schema.namespace = "Auth".to_string();
    parser::run(
        "pyre/schema/Auth/schema.pyre",
        auth_source,
        &mut auth_schema,
    )
    .expect("Auth schema should parse");

    let database = ast::Database {
        schemas: vec![app_schema, auth_schema],
    };

    let result = typecheck::check_schema(&database);
    let errors = result.expect_err("Typecheck should fail for schema-qualified missing table");

    assert!(
        errors.iter().any(|error| {
            matches!(
                &error.error_type,
                ErrorType::LinkToUnknownTable {
                    link_name,
                    unknown_table
                } if link_name == "user" && unknown_table == "User"
            )
        }),
        "Expected LinkToUnknownTable for Auth.User, got: {:?}",
        errors
    );
}

#[test]
fn test_query_allows_cross_namespace_link_traversal() {
    let app_source = r#"
record Post {
    id Int @id
    userId Int
    user @link(userId, Auth.User.id)
    @public
}
    "#;

    let auth_source = r#"
record User {
    id Int @id
    email String
    @public
}
    "#;

    let mut app_schema = ast::Schema::default();
    app_schema.namespace = "App".to_string();
    parser::run("pyre/schema/App/schema.pyre", app_source, &mut app_schema)
        .expect("App schema should parse");

    let mut auth_schema = ast::Schema::default();
    auth_schema.namespace = "Auth".to_string();
    parser::run(
        "pyre/schema/Auth/schema.pyre",
        auth_source,
        &mut auth_schema,
    )
    .expect("Auth schema should parse");

    let database = ast::Database {
        schemas: vec![app_schema, auth_schema],
    };

    let context = typecheck::check_schema(&database).expect("Schema should typecheck");
    let query_source = r#"
query GetPosts {
    post {
        id
        user {
            id
            email
        }
    }
}
    "#;

    let query_list = parser::parse_query("query.pyre", query_source).expect("Query should parse");
    let result = typecheck::check_queries(&query_list, &context);

    assert!(
        result.is_ok(),
        "Cross-namespace link traversal should typecheck"
    );
}

#[test]
fn test_duplicate_record_name_across_namespaces_fails_today() {
    let app_source = r#"
record User {
    id Int @id
    @public
}
    "#;

    let auth_source = r#"
record User {
    id Int @id
    @public
}
    "#;

    let mut app_schema = ast::Schema::default();
    app_schema.namespace = "App".to_string();
    parser::run("pyre/schema/App/schema.pyre", app_source, &mut app_schema)
        .expect("App schema should parse");

    let mut auth_schema = ast::Schema::default();
    auth_schema.namespace = "Auth".to_string();
    parser::run(
        "pyre/schema/Auth/schema.pyre",
        auth_source,
        &mut auth_schema,
    )
    .expect("Auth schema should parse");

    let database = ast::Database {
        schemas: vec![app_schema, auth_schema],
    };

    let result = typecheck::check_schema(&database);
    let errors = result.expect_err("Duplicate record names across namespaces should fail");

    assert!(
        errors
            .iter()
            .any(|error| matches!(error.error_type, ErrorType::DuplicateDefinition(_))),
        "Expected DuplicateDefinition error, got: {:?}",
        errors
    );
}
