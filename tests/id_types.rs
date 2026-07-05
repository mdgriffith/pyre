use pyre::ast;
use pyre::db::introspect;
use pyre::error::ErrorType;
use pyre::generate::to_string::schema_to_string;
use pyre::parser;
use pyre::sync;
use pyre::typecheck;
use std::collections::HashMap;
use tempfile::TempDir;

#[test]
fn test_id_int_parsing() {
    let schema_source = r#"
record User {
    id Id.Int @id
    name String
}
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Id.Int should parse successfully");

    // Check that the brand was set
    let record = schema.files[0]
        .definitions
        .iter()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "User" => Some(fields),
            _ => None,
        })
        .expect("Should find User record");

    let id_field = record
        .iter()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "id" => Some(col),
            _ => None,
        })
        .expect("Should find id field");

    assert_eq!(
        id_field.type_,
        ast::ColumnType::IdInt {
            table: String::new()
        }
    );
}

#[test]
fn test_id_uuid_parsing() {
    let schema_source = r#"
record Invite {
    id Id.Uuid @id
    code String
}
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Id.Uuid should parse successfully");

    let record = schema.files[0]
        .definitions
        .iter()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "Invite" => Some(fields),
            _ => None,
        })
        .expect("Should find Invite record");

    let id_field = record
        .iter()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "id" => Some(col),
            _ => None,
        })
        .expect("Should find id field");

    assert_eq!(
        id_field.type_,
        ast::ColumnType::IdUuid {
            table: String::new()
        }
    );
}

#[test]
fn test_id_int_generic_parsing_is_rejected() {
    let schema_source = r#"
record User {
    id Id.Int<User> @id
    name String
}
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Id.Int<User> should not parse");
}

#[test]
fn test_branded_id_int_to_string_preserves_brand() {
    let type_ = ast::ColumnType::IdInt {
        table: "User".to_string(),
    };
    assert_eq!(type_.to_string(), "Id.Int<User>");
}

#[test]
fn test_sync_status_sql_after_roundtrip_with_branded_ids() {
    let schema_source = r#"
record User {
    @public
    id Id.Int @id
    updatedAt DateTime
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let user_fields = schema.files[0]
        .definitions
        .iter_mut()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "User" => Some(fields),
            _ => None,
        })
        .expect("Should find User record");

    let id_field = user_fields
        .iter_mut()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "id" => Some(col),
            _ => None,
        })
        .expect("Should find id field");

    id_field.type_ = ast::ColumnType::IdInt {
        table: "User".to_string(),
    };

    let roundtripped_schema_source = schema_to_string("", &schema);
    assert!(!roundtripped_schema_source.contains("Id.Int<"));
    assert!(roundtripped_schema_source.contains("Id.Int"));

    let introspection = introspect::from_raw(introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: introspect::MigrationState::MigrationTable { migrations: vec![] },
        schema_source: roundtripped_schema_source,
        links: vec![],
    });

    let context = match &introspection.schema {
        introspect::SchemaResult::Success { context, .. } => context,
        introspect::SchemaResult::FailedToParse { errors, .. } => {
            panic!("Schema failed to parse: {:?}", errors)
        }
        introspect::SchemaResult::FailedToTypecheck { errors, .. } => {
            panic!("Schema failed to typecheck: {:?}", errors)
        }
    };

    let sync_cursor: sync::SyncCursor = HashMap::new();
    let session: HashMap<String, sync::SessionValue> = HashMap::new();
    let result = sync::get_sync_status_sql(&sync_cursor, context, &session);

    assert!(result.is_ok());
}

#[test]
fn test_foreign_key_field_reference_parsing() {
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
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Foreign key reference should parse successfully"
    );

    let post_record = schema.files[0]
        .definitions
        .iter()
        .find_map(|d| match d {
            ast::Definition::Record { name, fields, .. } if name == "Post" => Some(fields),
            _ => None,
        })
        .expect("Should find Post record");

    let author_id_field = post_record
        .iter()
        .find_map(|f| match f {
            ast::Field::Column(col) if col.name == "authorId" => Some(col),
            _ => None,
        })
        .expect("Should find authorId field");

    assert_eq!(
        author_id_field.type_,
        ast::ColumnType::ForeignKey {
            table: "User".to_string(),
            field: "id".to_string()
        }
    );
}

#[test]
fn test_foreign_key_to_unknown_table_error() {
    let schema_source = r#"
record Post {
    @public
    id Id.Int @id
    authorId NonExistent.id
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let result = typecheck::check_schema(&database);

    match result {
        Ok(_) => {
            panic!("Expected typechecking to fail due to foreign key referencing unknown table");
        }
        Err(errors) => {
            let fk_error = errors.iter().find(|e| {
                matches!(
                    &e.error_type,
                    ErrorType::ForeignKeyToUnknownTable {
                        field_name,
                        referenced_table,
                        ..
                    }
                    if field_name == "authorId" && referenced_table == "NonExistent"
                )
            });

            assert!(
                fk_error.is_some(),
                "Expected ForeignKeyToUnknownTable error for authorId referencing NonExistent.id, got: {:?}",
                errors
            );

            // Verify error has location info
            let error = fk_error.unwrap();
            assert!(
                !error.locations.is_empty(),
                "Error should have at least one location"
            );
        }
    }
}

#[test]
fn test_foreign_key_to_unknown_field_error() {
    let schema_source = r#"
record User {
    @public
    id Id.Int @id
    name String
}

record Post {
    @public
    id Id.Int @id
    authorId User.nonexistent
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let result = typecheck::check_schema(&database);

    match result {
        Ok(_) => {
            panic!("Expected typechecking to fail due to foreign key referencing unknown field");
        }
        Err(errors) => {
            let fk_error = errors.iter().find(|e| {
                matches!(
                    &e.error_type,
                    ErrorType::ForeignKeyToUnknownField {
                        field_name,
                        referenced_table,
                        referenced_field,
                        ..
                    }
                    if field_name == "authorId" && referenced_table == "User" && referenced_field == "nonexistent"
                )
            });

            assert!(
                fk_error.is_some(),
                "Expected ForeignKeyToUnknownField error for authorId referencing User.nonexistent, got: {:?}",
                errors
            );

            // Verify existing_fields are provided for helpful error messages
            if let Some(error) = fk_error {
                if let ErrorType::ForeignKeyToUnknownField {
                    existing_fields, ..
                } = &error.error_type
                {
                    assert!(
                        existing_fields.contains(&"id".to_string()),
                        "Error should list 'id' as an existing field"
                    );
                    assert!(
                        existing_fields.contains(&"name".to_string()),
                        "Error should list 'name' as an existing field"
                    );
                }
            }
        }
    }
}

#[test]
fn test_foreign_key_to_non_id_field_error() {
    let schema_source = r#"
record User {
    @public
    id Id.Int @id
    name String
}

record Post {
    @public
    id Id.Int @id
    authorId User.name
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let result = typecheck::check_schema(&database);

    match result {
        Ok(_) => {
            panic!("Expected typechecking to fail due to foreign key referencing non-ID field");
        }
        Err(errors) => {
            let fk_error = errors.iter().find(|e| {
                matches!(
                    &e.error_type,
                    ErrorType::ForeignKeyToNonIdField {
                        field_name,
                        referenced_table,
                        referenced_field,
                        referenced_field_type,
                    }
                    if field_name == "authorId"
                        && referenced_table == "User"
                        && referenced_field == "name"
                        && referenced_field_type == "String"
                )
            });

            assert!(
                fk_error.is_some(),
                "Expected ForeignKeyToNonIdField error for authorId referencing User.name (String), got: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_valid_foreign_key_passes_validation() {
    let schema_source = r#"
record User {
    @public
    id Id.Int @id
    name String
}

record Post {
    @public
    id Id.Int @id
    authorId User.id
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let result = typecheck::check_schema(&database);

    assert!(
        result.is_ok(),
        "Valid foreign key reference should pass validation, but got: {:?}",
        result.err()
    );
}

#[test]
fn test_foreign_key_with_id_uuid_passes_validation() {
    let schema_source = r#"
record User {
    @public
    id Id.Uuid @id
    name String
}

record Post {
    @public
    id Id.Int @id
    authorId User.id
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let result = typecheck::check_schema(&database);

    assert!(
        result.is_ok(),
        "Foreign key reference to Id.Uuid should pass validation, but got: {:?}",
        result.err()
    );
}

#[test]
fn test_sync_schema_roundtrip_with_plain_int_fk_field_succeeds() {
    let schema_source = r#"
session {
    userId Int
    role   String
}

record User {
    @allow(*) { id == Session.userId }

    id        Int     @id
    name      String?
    email     String?
    createdAt DateTime @default(now)

    posts @link(Post.authorUserId)
}

record Post {
    @allow(query) { authorUserId == Session.userId || published == True }
    @allow(update, insert, delete) { authorUserId == Session.userId }

    id           Int      @id
    createdAt    DateTime @default(now)
    authorUserId Int
    title        String
    content      String
    published    Bool     @default(False)

    users @link(authorUserId, User.id)
}
"#;

    let introspection = introspect::from_raw(introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: introspect::MigrationState::MigrationTable { migrations: vec![] },
        schema_source: schema_source.to_string(),
        links: vec![],
    });

    let context = match &introspection.schema {
        introspect::SchemaResult::Success { context, .. } => context,
        introspect::SchemaResult::FailedToParse { errors, .. } => {
            panic!("Schema should parse, got parse errors: {:?}", errors)
        }
        introspect::SchemaResult::FailedToTypecheck { errors, .. } => {
            panic!("Schema should typecheck, got type errors: {:?}", errors)
        }
    };

    let sync_cursor: sync::SyncCursor = HashMap::new();
    let mut session: HashMap<String, sync::SessionValue> = HashMap::new();
    session.insert("userId".to_string(), sync::SessionValue::Integer(1));
    session.insert(
        "role".to_string(),
        sync::SessionValue::Text("user".to_string()),
    );

    let result = sync::get_sync_status_statement(&sync_cursor, context, &session);
    assert!(
        result.is_ok(),
        "Sync status SQL statement should be generated"
    );
}

#[test]
fn test_sync_schema_roundtrip_with_user_id_field_reference_fails_typecheck() {
    let schema_source = r#"
session {
    userId Int
    role   String
}

record User {
    @allow(*) { id == Session.userId }

    id        Int     @id
    name      String?
    email     String?
    createdAt DateTime @default(now)

    posts @link(Post.authorUserId)
}

record Post {
    @allow(query) { authorUserId == Session.userId || published == True }
    @allow(update, insert, delete) { authorUserId == Session.userId }

    id           Int      @id
    createdAt    DateTime @default(now)
    authorUserId User.id
    title        String
    content      String
    published    Bool     @default(False)

    users @link(authorUserId, User.id)
}
"#;

    let introspection = introspect::from_raw(introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: introspect::MigrationState::MigrationTable { migrations: vec![] },
        schema_source: schema_source.to_string(),
        links: vec![],
    });

    match &introspection.schema {
        introspect::SchemaResult::Success { .. } => {
            panic!("Schema should fail typecheck when using User.id against Int @id")
        }
        introspect::SchemaResult::FailedToParse { errors, .. } => {
            panic!("Schema should parse, got parse errors: {:?}", errors)
        }
        introspect::SchemaResult::FailedToTypecheck { errors, .. } => {
            let has_expected_fk_error = errors.iter().any(|e| {
                matches!(
                    &e.error_type,
                    ErrorType::ForeignKeyToNonIdField {
                        field_name,
                        referenced_table,
                        referenced_field,
                        referenced_field_type,
                    }
                    if field_name == "authorUserId"
                        && referenced_table == "User"
                        && referenced_field == "id"
                        && referenced_field_type == "Int"
                )
            });

            assert!(
                has_expected_fk_error,
                "Expected ForeignKeyToNonIdField for authorUserId -> User.id(Int), got: {:?}",
                errors
            );
        }
    }
}

#[test]
fn test_schema_to_string_empty_namespace_omits_internal_default_schema_prefix() {
    let schema_source = r#"
record User {
    @public
    id    Id.Int @id
    posts @link(Post.authorUserId)
}

record Post {
    @public
    id           Id.Int @id
    authorUserId User.id
    users        @link(authorUserId, User.id)
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let formatted = schema_to_string("", &schema);

    assert!(
        !formatted.contains("_default."),
        "Formatted schema should not contain internal default namespace prefix: {}",
        formatted
    );

    let mut reparsed = ast::Schema::default();
    let reparsed_result = parser::run("schema.pyre", &formatted, &mut reparsed);
    assert!(
        reparsed_result.is_ok(),
        "Formatted schema should parse successfully, got: {:?}\nSchema:\n{}",
        reparsed_result.err(),
        formatted
    );
}

#[tokio::test]
async fn test_sync_status_sql_with_session_only_permission_executes_on_sqlite() {
    let schema_source = r#"
session {
    isAdmin Bool
}

record GameDocument {
    @allow(*) { Session.isAdmin == True }

    id        Id.Int   @id
    updatedAt DateTime @default(now)
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("Failed to parse schema");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let context = typecheck::check_schema(&database)
        .expect("Schema should typecheck for sync SQL generation");

    let sync_cursor: sync::SyncCursor = HashMap::new();
    let mut session: HashMap<String, sync::SessionValue> = HashMap::new();
    session.insert("isAdmin".to_string(), sync::SessionValue::Integer(1));

    let sync_status_statement =
        match sync::get_sync_status_statement(&sync_cursor, &context, &session) {
            Ok(statement) => statement,
            Err(_) => panic!("Sync status SQL generation should succeed"),
        };
    let sync_status_sql = sync_status_statement.sql;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("sync_status_test.db");
    let db = libsql::Builder::new_local(db_path.to_str().expect("invalid db path"))
        .build()
        .await
        .expect("Failed to create sqlite db");
    let conn = db.connect().expect("Failed to connect to sqlite db");

    conn.execute(
        "create table gameDocuments (id integer not null primary key, updatedAt integer not null)",
        (),
    )
    .await
    .expect("Failed creating test table");

    // The sync status SQL reads the server revision from _pyre_sync,
    // which migrations always create before sync runs.
    conn.execute(pyre::db::migrate::CREATE_SYNC_TABLE, ())
        .await
        .expect("Failed creating _pyre_sync table");
    conn.execute(pyre::db::migrate::INSERT_SYNC_REVISION_ROW, ())
        .await
        .expect("Failed seeding _pyre_sync revision row");

    assert!(
        !sync_status_sql.contains("gameDocuments.isAdmin"),
        "Sync status SQL should not reference session values as table columns: {}",
        sync_status_sql
    );

    let result = conn
        .query(
            &sync_status_sql,
            libsql::params_from_iter(vec![libsql::Value::Integer(1)]),
        )
        .await;
    assert!(
        result.is_ok(),
        "Expected sync status SQL to execute successfully, got error: {:?}\nSQL: {}",
        result.err(),
        sync_status_sql
    );
}
