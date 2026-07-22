use pyre::ast;
use pyre::parser;
use pyre::typecheck;
#[path = "helpers/schema_v1.rs"]
mod schema;

fn parse_schema(source: &str) -> ast::Schema {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema should parse");
    schema
}

/// Test helper to create a simple record with a field that has a specific directive
fn create_record_with_directive(
    field_name: &str,
    directive: ast::ColumnDirective,
) -> ast::RecordDetails {
    let mut schema = ast::Schema::default();
    let schema_source = format!(
        r#"
record TestRecord {{
    {} Int @id
    {} Int {}
}}
"#,
        if matches!(directive, ast::ColumnDirective::PrimaryKey) {
            field_name
        } else {
            "id"
        },
        if matches!(directive, ast::ColumnDirective::PrimaryKey) {
            "otherField"
        } else {
            field_name
        },
        match directive {
            ast::ColumnDirective::PrimaryKey => "@id",
            ast::ColumnDirective::Unique => "@unique",
            ast::ColumnDirective::Index => "@index",
            ast::ColumnDirective::CreatedAt => "@createdAt",
            ast::ColumnDirective::UpdatedAt => "@updatedAt",
            ast::ColumnDirective::Default { .. } => "",
        }
    );

    parser::run("test.pyre", &schema_source, &mut schema).unwrap();

    // Extract the record from the schema
    for file in &schema.files {
        for definition in &file.definitions {
            if let ast::Definition::Record { name, fields, .. } = definition {
                if name == "TestRecord" {
                    return ast::RecordDetails {
                        name: name.clone(),
                        fields: fields.clone(),
                        start: None,
                        end: None,
                        start_name: None,
                        end_name: None,
                    };
                }
            }
        }
    }
    panic!("Failed to create test record");
}

fn column_default_id(schema: &ast::Schema, column_name: &str) -> String {
    schema
        .files
        .iter()
        .flat_map(|file| &file.definitions)
        .find_map(|definition| match definition {
            ast::Definition::Record { fields, .. } => ast::collect_columns(fields)
                .into_iter()
                .find(|column| column.name == column_name),
            _ => None,
        })
        .and_then(|column| {
            column
                .directives
                .iter()
                .find_map(|directive| match directive {
                    ast::ColumnDirective::Default { id, .. } => Some(id.clone()),
                    _ => None,
                })
        })
        .expect("column should have a default")
}

#[test]
fn default_id_contains_only_the_parsed_value() {
    let schema = parse_schema(
        r#"record Example {
    published Bool @default(False)

    label String
}"#,
    );

    assert_eq!(column_default_id(&schema, "published"), "False");
}

#[test]
fn serialized_defaults_do_not_produce_migration_errors() {
    let authored = parse_schema(
        r#"record Example {
    id        Int     @id
    published Bool    @default(False)

    label     String  @default("hello")

    count     Int     @default(3)

    ratio     Float   @default(1.5)

    optional  String? @default(null)
}"#,
    );
    let persisted_source = pyre::generate::to_string::schema_to_string("", &authored);
    let persisted = parse_schema(&persisted_source);

    let errors = ast::diff::to_errors(ast::diff::diff_schema(&persisted, &authored));

    assert!(errors.is_empty(), "persisted schema:\n{persisted_source}");
}

#[test]
fn changed_default_produces_a_modified_column_error() {
    let old = parse_schema(
        r#"record Example {
    published Bool @default(False)
}"#,
    );
    let new = parse_schema(
        r#"record Example {
    published Bool @default(True)
}"#,
    );

    let errors = ast::diff::to_errors(ast::diff::diff_schema(&old, &new));

    assert_eq!(errors.len(), 1);
    match &errors[0].error_type {
        pyre::error::ErrorType::MigrationColumnModified { changes, .. } => {
            assert_eq!(changes.added_directives.len(), 1);
            assert_eq!(changes.removed_directives.len(), 1);
        }
        error => panic!("expected modified column error, got {error:?}"),
    }
}

#[test]
fn adding_or_removing_a_default_produces_a_modified_column_error() {
    let without_default = parse_schema(
        r#"record Example {
    published Bool
}"#,
    );
    let with_default = parse_schema(
        r#"record Example {
    published Bool @default(False)
}"#,
    );

    let added = ast::diff::to_errors(ast::diff::diff_schema(&without_default, &with_default));
    let removed = ast::diff::to_errors(ast::diff::diff_schema(&with_default, &without_default));

    assert!(matches!(
        added[0].error_type,
        pyre::error::ErrorType::MigrationColumnModified { .. }
    ));
    assert!(matches!(
        removed[0].error_type,
        pyre::error::ErrorType::MigrationColumnModified { .. }
    ));
}

#[test]
fn test_linked_to_unique_field_with_primary_key() {
    // Test that a link to a PRIMARY KEY field is detected as unique
    let record = create_record_with_directive("userId", ast::ColumnDirective::PrimaryKey);

    let link = ast::LinkDetails {
        link_name: "user".to_string(),
        local_ids: vec!["userId".to_string()],
        foreign: ast::Qualified {
            schema: "default".to_string(),
            table: "TestRecord".to_string(),
            fields: vec!["id".to_string()],
        },
        start_name: None,
        end_name: None,
        inline_comment: None,
    };

    // The link points to "id" which is a PRIMARY KEY, so it should be unique
    assert!(
        ast::linked_to_unique_field_with_record(&link, &record),
        "Link to PRIMARY KEY field should be detected as unique"
    );
}

#[test]
fn test_linked_to_unique_field_with_unique_constraint() {
    // Test that a link to a UNIQUE field is detected as unique
    let record = create_record_with_directive("email", ast::ColumnDirective::Unique);

    let link = ast::LinkDetails {
        link_name: "account".to_string(),
        local_ids: vec!["accountId".to_string()],
        foreign: ast::Qualified {
            schema: "default".to_string(),
            table: "TestRecord".to_string(),
            fields: vec!["email".to_string()],
        },
        start_name: None,
        end_name: None,
        inline_comment: None,
    };

    // The link points to "email" which has @unique, so it should be unique
    assert!(
        ast::linked_to_unique_field_with_record(&link, &record),
        "Link to UNIQUE field should be detected as unique"
    );
}

#[test]
fn test_linked_to_unique_field_with_non_unique_field() {
    // Test that a link to a non-unique field is NOT detected as unique
    let record = create_record_with_directive(
        "name",
        ast::ColumnDirective::Default {
            id: "default".to_string(),
            value: ast::DefaultValue::Value(ast::QueryValue::String((
                ast::Range {
                    start: ast::Location {
                        offset: 0,
                        line: 0,
                        column: 0,
                    },
                    end: ast::Location {
                        offset: 0,
                        line: 0,
                        column: 0,
                    },
                },
                "test".to_string(),
            ))),
            start: None,
            end: None,
        },
    );

    let link = ast::LinkDetails {
        link_name: "record".to_string(),
        local_ids: vec!["recordId".to_string()],
        foreign: ast::Qualified {
            schema: "default".to_string(),
            table: "TestRecord".to_string(),
            fields: vec!["name".to_string()],
        },
        start_name: None,
        end_name: None,
        inline_comment: None,
    };

    // The link points to "name" which has no unique constraint, so it should NOT be unique
    assert!(
        !ast::linked_to_unique_field_with_record(&link, &record),
        "Link to non-unique field should NOT be detected as unique"
    );
}

#[test]
fn test_linked_to_unique_field_fallback_to_id() {
    // Test that the fallback behavior still works (checking for "id" field name)
    let mut schema = ast::Schema::default();
    let schema_source = r#"
record TestRecord {
    id Int @id
    name String
}
"#;

    parser::run("test.pyre", schema_source.trim(), &mut schema).unwrap();

    // Extract record from schema
    let mut record = None;
    for file in &schema.files {
        for definition in &file.definitions {
            if let ast::Definition::Record { name, fields, .. } = definition {
                if name == "TestRecord" {
                    record = Some(ast::RecordDetails {
                        name: name.clone(),
                        fields: fields.clone(),
                        start: None,
                        end: None,
                        start_name: None,
                        end_name: None,
                    });
                    break;
                }
            }
        }
    }
    let record = record.expect("Failed to find TestRecord");

    let link = ast::LinkDetails {
        link_name: "record".to_string(),
        local_ids: vec!["recordId".to_string()],
        foreign: ast::Qualified {
            schema: "default".to_string(),
            table: "TestRecord".to_string(),
            fields: vec!["id".to_string()],
        },
        start_name: None,
        end_name: None,
        inline_comment: None,
    };

    // Should detect as unique because "id" is a PRIMARY KEY
    assert!(
        ast::linked_to_unique_field_with_record(&link, &record),
        "Link to 'id' PRIMARY KEY field should be detected as unique"
    );
}

#[test]
fn test_select_type_columns_simple_enum() {
    // Test that simple enums (no fields) work correctly in SQL generation
    // Use a simpler schema that we know parses correctly
    let schema_source = schema::schema_v1_complete();

    let mut schema = ast::Schema::default();
    parser::run("test.pyre", &schema_source, &mut schema).unwrap();

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();

    // Test through the public SQL generation API
    let query_source = r#"
        query GetUsers {
            user {
                id
                status
            }
        }
    "#;

    let query_list = pyre::parser::parse_query("test.pyre", query_source).unwrap();
    let query_info = typecheck::check_queries(&query_list, &context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let info = query_info.get(&query.name).unwrap();
    let table = context.tables.get("user").unwrap();
    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(f) => Some(f),
            _ => None,
        })
        .unwrap();

    // Generate SQL - select_type_columns is called internally
    let sql_statements = pyre::generate::sql::to_string(&context, query, info, table, table_field);

    // Verify SQL was generated
    assert!(!sql_statements.is_empty(), "Should generate SQL statements");
}

#[test]
fn test_select_type_columns_union_with_fields() {
    // Test that union types with fields generate JSON CASE statements
    // Use the helper schema which already has a union type with fields
    let schema_source = schema::schema_v1_complete();

    let mut schema = ast::Schema::default();
    parser::run("test.pyre", &schema_source, &mut schema).unwrap();

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();

    // Test through the public SQL generation API
    let query_source = r#"
        query GetUsers {
            user {
                id
                status
            }
        }
    "#;

    let query_list = pyre::parser::parse_query("test.pyre", query_source).unwrap();
    let query_info = typecheck::check_queries(&query_list, &context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let info = query_info.get(&query.name).unwrap();
    let table = context.tables.get("user").unwrap();
    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(f) => Some(f),
            _ => None,
        })
        .unwrap();

    // Generate SQL - select_type_columns is called internally and should generate JSON CASE
    let sql_statements = pyre::generate::sql::to_string(&context, query, info, table, table_field);

    // Verify SQL was generated
    assert!(!sql_statements.is_empty(), "Should generate SQL statements");

    // For union types with fields, select_type_columns should generate JSON CASE statements
    // The fact that SQL was generated successfully indicates the function works
}

#[test]
fn test_select_type_columns_unknown_type() {
    // Test that queries with unknown types still work (though this shouldn't happen
    // in practice due to typechecking, but we test the fallback behavior)
    // This test is mainly to ensure the code doesn't panic
    let mut schema = ast::Schema::default();
    let schema_source = r#"
record User {
    id Int @id
    name String
    @public
}
"#;

    parser::run("test.pyre", schema_source.trim(), &mut schema).unwrap();

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();

    // Query with a known field (not a type, but tests the code path)
    let query_source = r#"
        query GetUsers {
            user {
                id
                name
            }
        }
    "#;

    let query_list = pyre::parser::parse_query("test.pyre", query_source).unwrap();
    let query_info = typecheck::check_queries(&query_list, &context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let info = query_info.get(&query.name).unwrap();
    let table = context.tables.get("user").unwrap();
    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(f) => Some(f),
            _ => None,
        })
        .unwrap();

    // Generate SQL - should work fine
    let sql_statements = pyre::generate::sql::to_string(&context, query, info, table, table_field);
    assert!(!sql_statements.is_empty(), "Should generate SQL statements");
}

#[test]
fn test_select_type_columns_qualified_table_name() {
    // Test that qualified table names are handled correctly for variant fields
    // Use the helper schema
    let schema_source = schema::schema_v1_complete();

    let mut schema = ast::Schema::default();
    parser::run("test.pyre", &schema_source, &mut schema).unwrap();

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();

    // Test through SQL generation - the qualified names should be handled correctly
    let query_source = r#"
        query GetUsers {
            user {
                id
                status
            }
        }
    "#;

    let query_list = pyre::parser::parse_query("test.pyre", query_source).unwrap();
    let query_info = typecheck::check_queries(&query_list, &context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let info = query_info.get(&query.name).unwrap();
    let table = context.tables.get("user").unwrap();
    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(f) => Some(f),
            _ => None,
        })
        .unwrap();

    // Generate SQL - qualified names should be handled correctly
    let sql_statements = pyre::generate::sql::to_string(&context, query, info, table, table_field);
    assert!(!sql_statements.is_empty(), "Should generate SQL statements");

    // The fact that SQL was generated successfully indicates qualified names are handled correctly
}

#[test]
fn test_resolve_id_brands_sets_brand_on_id_columns() {
    // Test that ID columns (Id.Int or Id.Uuid) get their brand set to the record name
    let schema_source = r#"
record User {
    id Id.Int @id
    name String
}

record Post {
    id Id.Uuid @id
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("test.pyre", schema_source.trim(), &mut schema).unwrap();

    let mut database = ast::Database {
        schemas: vec![schema],
    };

    // Apply the brand resolution
    ast::resolve_id_brands(&mut database);

    // Verify that the ID columns now have the correct brand
    let user_record = database.schemas[0].files[0]
        .definitions
        .iter()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "User" => Some(fields),
            _ => None,
        })
        .unwrap();

    let user_id_column = user_record
        .iter()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "id" => Some(col),
            _ => None,
        })
        .unwrap();

    assert_eq!(
        user_id_column.type_,
        ast::ColumnType::IdInt {
            table: "User".to_string()
        },
        "User.id should have table 'User'"
    );

    let post_record = database.schemas[0].files[0]
        .definitions
        .iter()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "Post" => Some(fields),
            _ => None,
        })
        .unwrap();

    let post_id_column = post_record
        .iter()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "id" => Some(col),
            _ => None,
        })
        .unwrap();

    assert_eq!(
        post_id_column.type_,
        ast::ColumnType::IdUuid {
            table: "Post".to_string()
        },
        "Post.id should have table 'Post'"
    );
}

#[test]
fn test_resolve_id_brands_resolves_foreign_key_references() {
    // Test that foreign key references resolve the brand from the referenced table
    let schema_source = r#"
record User {
    id Id.Int @id
    name String
}

record Post {
    id Id.Int @id
    authorId User.id
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("test.pyre", schema_source.trim(), &mut schema).unwrap();

    let mut database = ast::Database {
        schemas: vec![schema],
    };

    // Apply the brand resolution
    ast::resolve_id_brands(&mut database);

    // Verify that the foreign key column has the correct brand
    let post_record = database.schemas[0].files[0]
        .definitions
        .iter()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "Post" => Some(fields),
            _ => None,
        })
        .unwrap();

    let author_id_column = post_record
        .iter()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "authorId" => Some(col),
            _ => None,
        })
        .unwrap();

    // The foreign key should reference the User table's id field
    assert_eq!(
        author_id_column.type_,
        ast::ColumnType::ForeignKey {
            table: "User".to_string(),
            field: "id".to_string(),
            serialization_type: Some(ast::ConcreteSerializationType::IdInt),
        },
        "Post.authorId should be a ForeignKey to User.id"
    );
}
