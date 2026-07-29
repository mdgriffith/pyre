#[path = "helpers/error_basic.rs"]
mod error;
#[path = "helpers/schema_migrations.rs"]
mod schema;

use error::TestError;
use libsql::Database;
use pyre::ast;
use pyre::db::diff;
use pyre::db::introspect;
use pyre::parser;
use pyre::typecheck;
use tempfile::TempDir;

struct MigrationDatabase {
    db: Database,
    temp_dir: TempDir,
    schema: ast::Schema,
}

impl MigrationDatabase {
    async fn new(schema_source: &str) -> Result<Self, TestError> {
        let temp_dir = TempDir::new().map_err(TestError::Io)?;
        let db_path = temp_dir.path().join("test.db");
        let db_path_str = db_path.to_str().ok_or(TestError::InvalidPath)?;

        let db = libsql::Builder::new_local(db_path_str)
            .build()
            .await
            .map_err(TestError::Database)?;

        let mut schema = ast::Schema::default();
        parser::run("schema.pyre", schema_source, &mut schema)
            .map_err(|e| TestError::ParseError(parser::render_error(schema_source, e, false)))?;

        let database = ast::Database {
            schemas: vec![schema.clone()],
        };
        let context = typecheck::check_schema(&database)
            .map_err(|errors| TestError::TypecheckError(format!("{:?}", errors)))?;

        let introspection = introspect::Introspection {
            tables: vec![],
            migration_state: introspect::MigrationState::NoMigrationTable,
            schema: introspect::SchemaResult::Success {
                schema: ast::Schema::default(),
                context: typecheck::empty_context(),
            },
        };

        let db_diff = diff::diff(&context, &schema, &introspection);
        let mut migration_sql = diff::to_sql::to_sql(&db_diff);

        match introspection.migration_state {
            introspect::MigrationState::NoMigrationTable => {
                migration_sql.splice(0..0, pyre::db::migrate::internal_setup_sql());
            }
            introspect::MigrationState::MigrationTable { .. } => {}
        }

        let schema_string = pyre::generate::to_string::schema_to_string("", &schema);
        migration_sql.push(pyre::generate::sql::to_sql::SqlAndParams::SqlWithParams {
            sql: pyre::db::migrate::INSERT_MIGRATION_SUCCESS_WITH_SCHEMA.to_string(),
            args: vec!["test-init".to_string(), "".to_string(), schema_string],
        });

        let conn = db.connect().map_err(TestError::Database)?;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .map_err(TestError::Database)?;

        for stmt in migration_sql {
            match stmt {
                pyre::generate::sql::to_sql::SqlAndParams::Sql(sql) => {
                    tx.execute_batch(&sql).await.map_err(TestError::Database)?;
                }
                pyre::generate::sql::to_sql::SqlAndParams::SqlWithParams { sql, args } => {
                    let values: Vec<libsql::Value> =
                        args.into_iter().map(libsql::Value::Text).collect();
                    tx.execute(&sql, libsql::params_from_iter(values))
                        .await
                        .map_err(TestError::Database)?;
                }
            }
        }

        tx.commit().await.map_err(TestError::Database)?;

        let db = Self {
            db,
            temp_dir,
            schema,
        };
        let _ = db.temp_dir.path();
        Ok(db)
    }
}

async fn introspect_uninitialized_db(
    db: &Database,
) -> Result<introspect::IntrospectionRaw, TestError> {
    let conn = db.connect().map_err(TestError::Database)?;
    let mut rows = conn
        .query(introspect::INTROSPECT_UNINITIALIZED_SQL, ())
        .await
        .map_err(TestError::Database)?;

    let mut introspection_raw = None;
    while let Some(row) = rows.next().await.map_err(TestError::Database)? {
        let result: String = row.get(0).map_err(TestError::Database)?;
        introspection_raw = Some(
            serde_json::from_str::<introspect::IntrospectionRaw>(&result).map_err(|e| {
                TestError::TypecheckError(format!("Failed to parse introspection: {}", e))
            })?,
        );
    }

    introspection_raw.ok_or(TestError::TypecheckError(
        "Failed to get introspection result".to_string(),
    ))
}

/// Helper function to create a diff between two schemas
/// Takes the old schema (as a string) and new schema (as a string) and returns the diff
async fn create_migration_diff(
    old_schema_source: &str,
    new_schema_source: &str,
) -> Result<diff::Diff, TestError> {
    // Create database with old schema
    let db = MigrationDatabase::new(old_schema_source).await?;

    // Introspect the database to get actual tables
    let introspection_raw = introspect_uninitialized_db(&db.db).await?;

    // Recreate the context from the schema since Context doesn't implement Clone
    let database = pyre::ast::Database {
        schemas: vec![db.schema.clone()],
    };
    let current_context = typecheck::check_schema(&database)
        .map_err(|errors| TestError::TypecheckError(format!("Typecheck errors: {:?}", errors)))?;

    // Build introspection with actual tables from database
    let introspection = introspect::Introspection {
        tables: introspection_raw.tables,
        migration_state: introspect::MigrationState::NoMigrationTable,
        schema: introspect::SchemaResult::Success {
            schema: db.schema.clone(),
            context: current_context,
        },
    };

    // Parse new schema
    let mut new_schema = pyre::ast::Schema::default();
    parser::run("schema.pyre", new_schema_source, &mut new_schema).map_err(|e| {
        TestError::ParseError(pyre::parser::render_error(new_schema_source, e, false))
    })?;

    // Typecheck new schema
    let database = pyre::ast::Database {
        schemas: vec![new_schema.clone()],
    };
    let new_context = typecheck::check_schema(&database)
        .map_err(|errors| TestError::TypecheckError(format!("Typecheck errors: {:?}", errors)))?;

    // Calculate diff
    let db_diff = diff::diff(&new_context, &new_schema, &introspection);

    Ok(db_diff)
}

#[tokio::test]
async fn migration_creates_single_pyre_sync_revision_row() -> Result<(), TestError> {
    let db = MigrationDatabase::new(
        r#"record Note {
    id Int @id
    body String
    @public
}"#,
    )
    .await?;
    let conn = db.db.connect().map_err(TestError::Database)?;
    let mut rows = conn
        .query(
            "select id, database_epoch, server_revision, count(*) over () from _pyre_sync",
            (),
        )
        .await
        .map_err(TestError::Database)?;

    let row = rows
        .next()
        .await
        .map_err(TestError::Database)?
        .ok_or_else(|| TestError::TypecheckError("_pyre_sync should have one row".to_string()))?;

    let id: i64 = row.get(0).map_err(TestError::Database)?;
    let epoch: String = row.get(1).map_err(TestError::Database)?;
    let revision: i64 = row.get(2).map_err(TestError::Database)?;
    let count: i64 = row.get(3).map_err(TestError::Database)?;

    assert_eq!(id, 1);
    assert_eq!(epoch.len(), 32);
    assert_eq!(revision, 0);
    assert_eq!(count, 1);

    let mut internal_tables = conn
        .query(
            "select name from sqlite_master where type = 'table' and name like '_pyre_%' order by name",
            (),
        )
        .await
        .map_err(TestError::Database)?;
    let mut names = Vec::new();
    while let Some(row) = internal_tables.next().await.map_err(TestError::Database)? {
        names.push(row.get::<String>(0).map_err(TestError::Database)?);
    }
    assert_eq!(names, vec!["_pyre_migrations", "_pyre_sync"]);

    let schema: String = conn
        .query(
            "select schema from _pyre_migrations where schema is not null order by id desc limit 1",
            (),
        )
        .await
        .map_err(TestError::Database)?
        .next()
        .await
        .map_err(TestError::Database)?
        .ok_or_else(|| TestError::TypecheckError("missing migration schema".to_string()))?
        .get(0)
        .map_err(TestError::Database)?;
    assert!(schema.contains("record Note"));

    Ok(())
}

#[tokio::test]
async fn test_introspection_captures_index_metadata() -> Result<(), TestError> {
    let schema = r#"record Membership {
    id        Int @id
    orgId     Int
    userId    Int
    createdAt DateTime
    deletedAt DateTime?

    @unique(orgId, userId)
    @index(orgId asc, createdAt desc) where { deletedAt = null }
    @public
}"#;

    let db = MigrationDatabase::new(schema).await?;
    let introspection_raw = introspect_uninitialized_db(&db.db).await?;

    let memberships = introspection_raw
        .tables
        .iter()
        .find(|t| t.name == "memberships")
        .expect("memberships table should be present");

    let unique_index = memberships
        .indexes
        .iter()
        .find(|idx| idx.unique && idx.columns.len() == 2)
        .expect("expected composite unique index to be introspected");
    assert_eq!(unique_index.columns[0].name, "orgId");
    assert_eq!(unique_index.columns[0].desc, false);
    assert_eq!(unique_index.columns[1].name, "userId");
    assert_eq!(unique_index.columns[1].desc, false);

    let partial_index = memberships
        .indexes
        .iter()
        .find(|idx| !idx.unique && idx.where_clause.is_some())
        .expect("expected partial index to be introspected");
    assert!(partial_index
        .where_clause
        .as_ref()
        .unwrap()
        .to_lowercase()
        .contains("deletedat"));

    Ok(())
}

// ============================================================================
// Table Migration Tests
// ============================================================================

#[tokio::test]
async fn test_migration_add_table() -> Result<(), TestError> {
    let old_schema = r#"record User {
    id   Int    @id
    name String
    @public
}"#;

    let new_schema = r#"record User {
    id   Int    @id
    name String
    @public
}

record Post {
    id        Int    @id
    title     String
    content   String
    authorId  Int
    author    @link(authorId, User.id)
    @public
}"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Verify that the diff includes adding the Post table (table name is pluralized: "posts")
    let has_post_table = db_diff.added.iter().any(|t| t.name == "posts");
    assert!(
        has_post_table,
        "Migration should add Post table (as 'posts')"
    );

    // Verify that User table is not modified
    let user_table_diff = db_diff.modified_records.iter().find(|r| r.name == "users");
    assert!(
        user_table_diff.is_none(),
        "Migration should not modify User table"
    );

    Ok(())
}

#[tokio::test]
async fn test_migration_add_column() -> Result<(), TestError> {
    let old_schema = r#"record User {
    id   Int    @id
    name String
    @public
}"#;

    let new_schema = r#"record User {
    id    Int    @id
    name  String
    email String
    @public
}"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Verify that User table has modifications
    let user_table_diff = db_diff.modified_records.iter().find(|r| r.name == "users");
    assert!(
        user_table_diff.is_some(),
        "Migration should modify User table (as 'users')"
    );

    let user_table_diff = user_table_diff.unwrap();
    // Verify that email column is added
    let has_email_added = user_table_diff
        .changes
        .iter()
        .any(|change| matches!(change, diff::RecordChange::AddedField(col) if col.name == "email"));
    assert!(
        has_email_added,
        "Migration should add email column to User table"
    );

    Ok(())
}

#[tokio::test]
async fn test_migration_adds_composite_unique_and_partial_ordered_index() -> Result<(), TestError> {
    let old_schema = r#"record Membership {
    id        Int @id
    orgId     Int
    userId    Int
    createdAt DateTime
    deletedAt DateTime?
    @public
}"#;

    let new_schema = r#"record Membership {
    id        Int @id
    orgId     Int
    userId    Int
    createdAt DateTime
    deletedAt DateTime?

    @unique(orgId, userId)
    @index(orgId, createdAt desc) where { deletedAt = null }
    @public
}"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;
    let sql_statements = diff::to_sql::to_sql(&db_diff);
    let rendered_sql: Vec<String> = sql_statements
        .into_iter()
        .filter_map(|stmt| match stmt {
            pyre::generate::sql::to_sql::SqlAndParams::Sql(sql) => Some(sql),
            _ => None,
        })
        .collect();

    assert!(
        rendered_sql
            .iter()
            .any(|s| s.contains("create unique index")
                && s.contains("\"memberships\"")
                && s.contains("\"orgId\"")
                && s.contains("\"userId\"")
                && !s.contains("\"userId\" desc")),
        "Expected migration SQL to include composite unique index. SQL:\n{}",
        rendered_sql.join("\n")
    );

    assert!(
        rendered_sql.iter().any(|s| s.contains("create index")
            && s.contains("\"memberships\"")
            && s.contains("\"orgId\"")
            && s.contains("\"createdAt\" desc")
            && s.to_lowercase().contains(" where ")),
        "Expected migration SQL to include partial ordered composite index. SQL:\n{}",
        rendered_sql.join("\n")
    );

    Ok(())
}

#[tokio::test]
async fn test_migration_remove_column() -> Result<(), TestError> {
    let old_schema = r#"record User {
    id    Int    @id
    name  String
    email String
    @public
}"#;

    let new_schema = r#"record User {
    id   Int    @id
    name String
    @public
}"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Verify that User table has modifications
    let user_table_diff = db_diff.modified_records.iter().find(|r| r.name == "users");
    assert!(
        user_table_diff.is_some(),
        "Migration should modify User table (as 'users')"
    );

    let user_table_diff = user_table_diff.unwrap();
    // Verify that email column is removed
    let has_email_removed = user_table_diff.changes.iter().any(
        |change| matches!(change, diff::RecordChange::RemovedField(col) if col.name == "email"),
    );
    assert!(
        has_email_removed,
        "Migration should remove email column from User table"
    );

    Ok(())
}

#[tokio::test]
async fn test_migration_change_column_type() -> Result<(), TestError> {
    let old_schema = r#"record User {
    id   Int    @id
    age  Int
    @public
}"#;

    let new_schema = r#"record User {
    id   Int     @id
    age  String
    @public
}"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Verify that User table has modifications
    let user_table_diff = db_diff.modified_records.iter().find(|r| r.name == "users");
    assert!(
        user_table_diff.is_some(),
        "Migration should modify User table (as 'users')"
    );

    let user_table_diff = user_table_diff.unwrap();
    // Verify that age column type is changed
    let has_age_type_changed = user_table_diff.changes.iter().any(|change| {
        matches!(
            change,
            diff::RecordChange::ModifiedField { name, changes }
                if name == "age" && changes.type_changed.is_some()
        )
    });
    assert!(
        has_age_type_changed,
        "Migration should change age column type from Int to String"
    );

    Ok(())
}

// ============================================================================
// Union Type Migration Tests
// ============================================================================

#[tokio::test]
async fn test_migration_union_add_variant() -> Result<(), TestError> {
    // Format that worked for union type alone - use leading spaces, no leading newline
    // Note: These tests focus on union type changes, so we don't need the User record
    // The union type format is what matters for testing migration behavior
    let old_schema = r#"type Status
   = Active
   | Inactive
"#;

    let new_schema = r#"type Status
   = Active
   | Inactive
   | Pending
"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // For union types, we need to check if the diff handles the new variant
    // The User table should not be modified since the column structure doesn't change
    // (union types with simple variants don't add columns)
    // Note: Adding a simple variant (no fields) might not modify the table structure
    // This test documents the current behavior
    eprintln!("Diff for union variant addition: {:?}", db_diff);
    if let Some(_user_table_diff) = db_diff.modified_records.iter().find(|r| r.name == "users") {
        eprintln!("User table was modified (unexpected for simple variant addition)");
    }

    Ok(())
}

#[tokio::test]
async fn test_migration_union_add_variant_with_subfields() -> Result<(), TestError> {
    let old_schema = r#"type Status
   = Active
   | Inactive
"#;

    let new_schema = r#"type Status
   = Active
   | Inactive
   | Suspended {
        reason String
        until  Int
     }
"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Adding a variant with subfields should modify the User table
    // to add columns for the new variant's fields
    // The exact behavior depends on how union types with fields are handled
    // This test documents what happens when we add a variant with subfields
    eprintln!(
        "Diff for union variant with subfields addition: {:?}",
        db_diff
    );

    if let Some(user_table_diff) = db_diff.modified_records.iter().find(|r| r.name == "users") {
        eprintln!("User table changes: {:?}", user_table_diff.changes);
    }

    Ok(())
}

#[tokio::test]
async fn test_migration_union_add_variant_with_shared_subfields() -> Result<(), TestError> {
    let old_schema = r#"type Status
   = Active {
        since Int
     }
   | Inactive
"#;

    let new_schema = r#"type Status
   = Active {
        since Int
     }
   | Inactive
   | Suspended {
        since Int
        reason String
     }
"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Adding a variant with subfields that share some fields with existing variants
    // should modify the User table appropriately
    // The "since" field is shared between Active and Suspended variants
    eprintln!(
        "Diff for union variant with shared subfields: {:?}",
        db_diff
    );

    if let Some(user_table_diff) = db_diff.modified_records.iter().find(|r| r.name == "users") {
        eprintln!("User table changes: {:?}", user_table_diff.changes);
    }

    Ok(())
}

#[tokio::test]
async fn test_migration_union_remove_variant() -> Result<(), TestError> {
    let old_schema = r#"type Status
   = Active
   | Inactive
   | Pending
"#;

    let new_schema = r#"type Status
   = Active
   | Inactive
"#;

    let db_diff = create_migration_diff(old_schema, new_schema).await?;

    // Removing a simple variant (no fields) might not modify the table structure
    // This test documents the current behavior
    eprintln!("Diff for union variant removal: {:?}", db_diff);

    Ok(())
}

// ============================================================================
// Legacy Tests (kept for backwards compatibility)
// ============================================================================

#[tokio::test]
async fn test_migration_from_v1_to_v2() -> Result<(), TestError> {
    // Start with schema v1
    let schema_v1 = schema::schema_v1_complete();
    let schema_v2 = schema::schema_v2_complete();
    let db_diff = create_migration_diff(&schema_v1, &schema_v2).await?;

    // Verify that the diff includes adding the Post table (table name is pluralized: "posts")
    let has_post_table = db_diff.added.iter().any(|t| t.name == "posts");
    assert!(
        has_post_table,
        "Migration should add Post table (as 'posts')"
    );

    // Verify that User table has modifications (new posts relationship) (table name is pluralized: "users")
    let user_table_diff = db_diff.modified_records.iter().find(|r| r.name == "users");
    assert!(
        user_table_diff.is_some(),
        "Migration should modify User table (as 'users')"
    );

    Ok(())
}

#[tokio::test]
async fn test_migration_from_v2_to_v3() -> Result<(), TestError> {
    // Start with schema v2
    let schema_v2 = schema::schema_v2_complete();
    let schema_v3 = schema::schema_v3_complete();
    let db_diff = create_migration_diff(&schema_v2, &schema_v3).await?;

    // Verify that the diff includes adding the Account table (table name is pluralized: "accounts")
    let has_account_table = db_diff.added.iter().any(|t| t.name == "accounts");
    assert!(
        has_account_table,
        "Migration should add Account table (as 'accounts')"
    );

    // Verify that User table has modifications (new accounts relationship) (table name is pluralized: "users")
    let user_table_diff = db_diff.modified_records.iter().find(|r| r.name == "users");
    assert!(
        user_table_diff.is_some(),
        "Migration should modify User table (as 'users')"
    );

    Ok(())
}

#[tokio::test]
async fn test_migration_from_v1_to_v3() -> Result<(), TestError> {
    // Start with schema v1
    let schema_v1 = schema::schema_v1_complete();
    let schema_v3 = schema::schema_v3_complete();
    let db_diff = create_migration_diff(&schema_v1, &schema_v3).await?;

    // Verify that the diff includes adding both Post and Account tables (table names are pluralized)
    let has_post_table = db_diff.added.iter().any(|t| t.name == "posts");
    let has_account_table = db_diff.added.iter().any(|t| t.name == "accounts");

    assert!(
        has_post_table,
        "Migration should add Post table (as 'posts')"
    );
    assert!(
        has_account_table,
        "Migration should add Account table (as 'accounts')"
    );

    Ok(())
}
