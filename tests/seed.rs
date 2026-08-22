#[path = "helpers/error.rs"]
mod error;
#[path = "helpers/schema_full.rs"]
mod schema;

use error::TestError;
use libsql::Database;
use pyre::ast;
use pyre::db::diff;
use pyre::db::introspect;
use pyre::db::migrate;
use pyre::error as pyre_error;
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::parser;
use pyre::typecheck;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use tempfile::TempDir;

struct SeedDatabase {
    db: Database,
    temp_dir: TempDir,
    context: typecheck::Context,
    schema: ast::Schema,
}

impl SeedDatabase {
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
            .map_err(|errors| TestError::TypecheckError(format_errors(schema_source, &errors)))?;

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
                migration_sql.splice(0..0, migrate::internal_setup_sql());
            }
            introspect::MigrationState::MigrationTable { .. } => {}
        }

        let schema_string = pyre::generate::to_string::schema_to_string("", &schema);
        migration_sql.push(SqlAndParams::SqlWithParams {
            sql: migrate::INSERT_MIGRATION_SUCCESS_WITH_SCHEMA.to_string(),
            args: vec!["test-init".to_string(), "".to_string(), schema_string],
        });

        let conn = db.connect().map_err(TestError::Database)?;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .map_err(TestError::Database)?;

        for stmt in migration_sql {
            match stmt {
                SqlAndParams::Sql(sql) => {
                    tx.execute_batch(&sql).await.map_err(TestError::Database)?;
                }
                SqlAndParams::SqlWithParams { sql, args } => {
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
            context,
            schema,
        };
        let _ = db.temp_dir.path();
        Ok(db)
    }

    fn generate_query_sql(
        &self,
        query_source: &str,
    ) -> Result<Vec<(bool, SqlAndParams)>, TestError> {
        let query_list = parser::parse_query("query.pyre", query_source)
            .map_err(|e| TestError::ParseError(parser::render_error(query_source, e, false)))?;

        let context = &self.context;
        let query_info = typecheck::check_queries(&query_list, context)
            .map_err(|errors| TestError::TypecheckError(format_errors(query_source, &errors)))?;

        let query = query_list
            .queries
            .iter()
            .find_map(|q| match q {
                ast::QueryDef::Query(q) => Some(q),
                _ => None,
            })
            .ok_or(TestError::NoQueryFound)?;

        let info = query_info
            .get(&query.name)
            .ok_or(TestError::NoQueryInfoFound)?;

        let table_field = query
            .fields
            .iter()
            .find_map(|f| match f {
                ast::TopLevelQueryField::Field(f) => Some(f),
                _ => None,
            })
            .ok_or(TestError::NoQueryFound)?;

        let table = context
            .tables
            .get(&table_field.name)
            .ok_or(TestError::NoQueryFound)?;

        let prepared_statements =
            pyre::generate::sql::to_string(context, query, info, table, table_field);

        let mut sql_statements = Vec::new();
        for prepared in prepared_statements {
            sql_statements.push((prepared.include, SqlAndParams::Sql(prepared.sql)));
        }

        Ok(sql_statements)
    }

    async fn execute_query_with_params(
        &self,
        query_source: &str,
        params: HashMap<String, libsql::Value>,
    ) -> Result<Vec<libsql::Rows>, TestError> {
        let query_list = parser::parse_query("query.pyre", query_source)
            .map_err(|e| TestError::ParseError(parser::render_error(query_source, e, false)))?;

        let query = query_list
            .queries
            .iter()
            .find_map(|q| match q {
                ast::QueryDef::Query(q) => Some(q),
                _ => None,
            })
            .ok_or(TestError::NoQueryFound)?;

        let param_names: Vec<String> = query.args.iter().map(|arg| arg.name.clone()).collect();
        let sql_statements = self.generate_query_sql(query_source)?;

        let conn = self.db.connect().map_err(TestError::Database)?;
        let mut results = Vec::new();

        for (include, sql_stmt) in sql_statements {
            match sql_stmt {
                SqlAndParams::Sql(sql) => {
                    let mut param_values_for_stmt = Vec::new();
                    let mut seen_params = std::collections::HashSet::new();

                    let mut chars = sql.chars().peekable();
                    while let Some(ch) = chars.next() {
                        if ch == '$' {
                            let mut param_name = String::new();
                            while let Some(&next_ch) = chars.peek() {
                                if next_ch.is_alphanumeric() || next_ch == '_' {
                                    param_name.push(chars.next().unwrap());
                                } else {
                                    break;
                                }
                            }
                            if param_names.contains(&param_name)
                                && !seen_params.contains(&param_name)
                            {
                                seen_params.insert(param_name.clone());
                                param_values_for_stmt.push(
                                    params
                                        .get(&param_name)
                                        .cloned()
                                        .unwrap_or(libsql::Value::Null),
                                );
                            }
                        }
                    }

                    let sql_with_params = if param_names.is_empty() {
                        sql.clone()
                    } else {
                        replace_params_positional(&sql, &param_names)
                    };

                    if include {
                        if param_values_for_stmt.is_empty() {
                            let rows = conn
                                .query(&sql_with_params, ())
                                .await
                                .map_err(TestError::Database)?;
                            results.push(rows);
                        } else {
                            let rows = conn
                                .query(
                                    &sql_with_params,
                                    libsql::params_from_iter(param_values_for_stmt.clone()),
                                )
                                .await
                                .map_err(TestError::Database)?;
                            results.push(rows);
                        }
                    } else {
                        let has_returning = sql_with_params.to_uppercase().contains("RETURNING");
                        if has_returning {
                            if param_values_for_stmt.is_empty() {
                                let mut rows = conn
                                    .query(&sql_with_params, ())
                                    .await
                                    .map_err(TestError::Database)?;
                                while rows.next().await.map_err(TestError::Database)?.is_some() {}
                            } else {
                                let mut rows = conn
                                    .query(
                                        &sql_with_params,
                                        libsql::params_from_iter(param_values_for_stmt.clone()),
                                    )
                                    .await
                                    .map_err(TestError::Database)?;
                                while rows.next().await.map_err(TestError::Database)?.is_some() {}
                            }
                        } else if param_values_for_stmt.is_empty() {
                            conn.execute(&sql_with_params, ())
                                .await
                                .map_err(TestError::Database)?;
                        } else {
                            conn.execute(
                                &sql_with_params,
                                libsql::params_from_iter(param_values_for_stmt.clone()),
                            )
                            .await
                            .map_err(TestError::Database)?;
                        }
                    }
                }
                SqlAndParams::SqlWithParams { sql, args } => {
                    let mut param_values_for_stmt = Vec::new();
                    let mut seen_params = std::collections::HashSet::new();

                    let mut chars = sql.chars().peekable();
                    while let Some(ch) = chars.next() {
                        if ch == '$' {
                            let mut param_name = String::new();
                            while let Some(&next_ch) = chars.peek() {
                                if next_ch.is_alphanumeric() || next_ch == '_' {
                                    param_name.push(chars.next().unwrap());
                                } else {
                                    break;
                                }
                            }
                            if param_names.contains(&param_name)
                                && !seen_params.contains(&param_name)
                            {
                                seen_params.insert(param_name.clone());
                                param_values_for_stmt.push(
                                    params
                                        .get(&param_name)
                                        .cloned()
                                        .unwrap_or(libsql::Value::Null),
                                );
                            }
                        }
                    }

                    let mut values: Vec<libsql::Value> =
                        args.into_iter().map(libsql::Value::Text).collect();
                    values.extend(param_values_for_stmt);
                    let sql_with_params = if param_names.is_empty() {
                        sql.clone()
                    } else {
                        replace_params_positional(&sql, &param_names)
                    };

                    if include {
                        let rows = conn
                            .query(&sql_with_params, libsql::params_from_iter(values))
                            .await
                            .map_err(TestError::Database)?;
                        results.push(rows);
                    } else {
                        conn.execute(&sql_with_params, libsql::params_from_iter(values))
                            .await
                            .map_err(TestError::Database)?;
                    }
                }
            }
        }

        Ok(results)
    }

    async fn execute_query(&self, query_source: &str) -> Result<Vec<libsql::Rows>, TestError> {
        self.execute_query_with_params(query_source, HashMap::new())
            .await
    }

    async fn parse_query_results(
        &self,
        rows: Vec<libsql::Rows>,
    ) -> Result<HashMap<String, Vec<JsonValue>>, TestError> {
        let mut result = HashMap::new();

        for mut rows_set in rows {
            let column_count = rows_set.column_count();
            if column_count == 0 {
                continue;
            }

            while let Some(row) = rows_set.next().await.map_err(TestError::Database)? {
                for i in 0..column_count {
                    let col_name = rows_set.column_name(i).ok_or(TestError::NoQueryFound)?;

                    if let Ok(json_str) = row.get::<String>(i as i32) {
                        if let Ok(json_value) = serde_json::from_str::<JsonValue>(&json_str) {
                            match json_value {
                                JsonValue::Array(arr) => {
                                    result
                                        .entry(col_name.to_string())
                                        .or_insert_with(Vec::new)
                                        .extend(arr);
                                }
                                JsonValue::Object(_) => {
                                    result
                                        .entry(col_name.to_string())
                                        .or_insert_with(Vec::new)
                                        .push(json_value);
                                }
                                _ => {
                                    result
                                        .entry(col_name.to_string())
                                        .or_insert_with(Vec::new)
                                        .push(json_value);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    async fn execute_raw(&self, sql: &str) -> Result<libsql::Rows, TestError> {
        let conn = self.db.connect().map_err(TestError::Database)?;
        conn.query(sql, ()).await.map_err(TestError::Database)
    }
}

fn format_errors(schema_source: &str, errors: &[pyre_error::Error]) -> String {
    errors
        .iter()
        .map(|e| pyre_error::format_error(schema_source, e, false))
        .collect::<Vec<_>>()
        .join("\n")
}

fn replace_params_positional(sql: &str, param_names: &[String]) -> String {
    let mut result = sql.to_string();
    let mut param_order = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut chars = result.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' {
            let mut param_name = String::new();
            while let Some(&next_ch) = chars.peek() {
                if next_ch.is_alphanumeric() || next_ch == '_' {
                    param_name.push(chars.next().unwrap());
                } else {
                    break;
                }
            }
            if param_names.contains(&param_name) {
                if !seen.contains(&param_name) {
                    param_order.push(param_name.clone());
                    seen.insert(param_name);
                }
            }
        }
    }

    for name in &param_order {
        result = result.replace(&format!("${}", name), "?");
    }

    result
}
use pyre::seed;

#[tokio::test]
async fn test_seed_populates_immutable_fields() -> Result<(), TestError> {
    let schema_source = r#"
record Document {
    id      Int @id
    ownerId Int @immutable
    title   String
    @public
}
"#;
    let db = SeedDatabase::new(schema_source).await?;
    let operations = seed::seed_database(&db.schema, &db.context, None);
    let conn = db.db.connect().map_err(TestError::Database)?;

    assert!(operations
        .iter()
        .any(|operation| operation.sql.contains("ownerId")));
    for operation in operations {
        conn.execute(&operation.sql, ())
            .await
            .map_err(TestError::Database)?;
    }

    let mut rows = conn
        .query("select ownerId from documents limit 1", ())
        .await
        .map_err(TestError::Database)?;
    assert!(rows.next().await.map_err(TestError::Database)?.is_some());

    Ok(())
}

#[tokio::test]
async fn test_seed_generates_valid_sql() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Generate seed data
    let operations = seed::seed_database(&db.schema, &db.context, None);

    // Verify we got some operations
    assert!(
        !operations.is_empty(),
        "Should generate at least some SQL operations"
    );

    // Execute all INSERT statements
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        // Verify SQL is valid by attempting to execute it
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    Ok(())
}

#[tokio::test]
async fn test_seed_data_can_be_queried() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Generate and execute seed data
    let operations = seed::seed_database(&db.schema, &db.context, None);
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Query all tables to verify data exists
    let user_query = r#"
        query GetUsers {
            user {
                id
                name
                status
            }
        }
    "#;

    let rows = db.execute_query(user_query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("user"),
        "Results should contain 'user' field"
    );
    let users = results.get("user").unwrap();
    assert!(
        !users.is_empty(),
        "Should have at least some users from seed data"
    );

    // Verify user fields match schema
    for user in users {
        assert!(user.get("id").is_some(), "User should have id field");
        assert!(user.get("name").is_some(), "User should have name field");
        assert!(
            user.get("status").is_some(),
            "User should have status field"
        );
    }

    // Query posts
    let post_query = r#"
        query GetPosts {
            post {
                id
                title
                content
                authorId
            }
        }
    "#;

    let rows = db.execute_query(post_query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("post"),
        "Results should contain 'post' field"
    );
    let posts = results.get("post").unwrap();
    assert!(
        !posts.is_empty(),
        "Should have at least some posts from seed data"
    );

    // Verify post fields match schema
    for post in posts {
        assert!(post.get("id").is_some(), "Post should have id field");
        assert!(post.get("title").is_some(), "Post should have title field");
        assert!(
            post.get("content").is_some(),
            "Post should have content field"
        );
        assert!(
            post.get("authorId").is_some(),
            "Post should have authorId field"
        );
    }

    // Query accounts
    let account_query = r#"
        query GetAccounts {
            account {
                id
                userId
                name
                status
            }
        }
    "#;

    let rows = db.execute_query(account_query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("account"),
        "Results should contain 'account' field"
    );
    let accounts = results.get("account").unwrap();
    assert!(
        !accounts.is_empty(),
        "Should have at least some accounts from seed data"
    );

    // Verify account fields match schema
    for account in accounts {
        assert!(account.get("id").is_some(), "Account should have id field");
        assert!(
            account.get("userId").is_some(),
            "Account should have userId field"
        );
        assert!(
            account.get("name").is_some(),
            "Account should have name field"
        );
        assert!(
            account.get("status").is_some(),
            "Account should have status field"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_seed_with_custom_options() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Create custom options with specific row counts
    let mut options = seed::Options::default();
    options.default_rows_per_table = 10;
    options.table_rows.insert("users".to_string(), 5);
    options.table_rows.insert("posts".to_string(), 20);

    // Generate seed data with custom options
    let operations = seed::seed_database(&db.schema, &db.context, Some(options));

    // Execute seed data
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Verify row counts match expectations (approximately)
    let mut user_rows = db.execute_raw("SELECT COUNT(*) FROM users").await?;
    let mut user_count = 0;
    while let Some(row) = user_rows.next().await.map_err(TestError::Database)? {
        user_count = row.get::<i64>(0).map_err(TestError::Database)?;
    }
    assert_eq!(
        user_count, 5,
        "Should have exactly 5 users as specified in options"
    );

    let mut post_rows = db.execute_raw("SELECT COUNT(*) FROM posts").await?;
    let mut post_count = 0;
    while let Some(row) = post_rows.next().await.map_err(TestError::Database)? {
        post_count = row.get::<i64>(0).map_err(TestError::Database)?;
    }
    assert_eq!(
        post_count, 20,
        "Should have exactly 20 posts as specified in options"
    );

    Ok(())
}

#[tokio::test]
async fn test_seed_foreign_key_relationships() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Generate seed data
    let operations = seed::seed_database(&db.schema, &db.context, None);

    // Execute seed data
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Query posts with author relationship
    let query = r#"
        query GetPostsWithAuthors {
            post {
                id
                title
                authorId
                author {
                    id
                    name
                }
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("post"),
        "Results should contain 'post' field"
    );
    let posts = results.get("post").unwrap();
    assert!(!posts.is_empty(), "Should have posts");

    // Verify foreign key relationships are valid
    for post in posts {
        let author_id = post
            .get("authorId")
            .and_then(|v| v.as_i64())
            .expect("Post should have authorId");

        // Check if author relationship exists (it might be null if the user was deleted or doesn't exist)
        if let Some(author) = post.get("author").and_then(|v| v.as_object()) {
            let author_id_from_rel = author
                .get("id")
                .and_then(|v| v.as_i64())
                .expect("Author should have id");

            assert_eq!(
                author_id, author_id_from_rel,
                "Post authorId should match author.id from relationship"
            );
        } else {
            // If author relationship is missing, at least verify the authorId exists in users table
            let mut user_check = db
                .execute_raw(&format!(
                    "SELECT COUNT(*) FROM users WHERE id = {}",
                    author_id
                ))
                .await?;
            let mut user_exists = 0;
            while let Some(row) = user_check.next().await.map_err(TestError::Database)? {
                user_exists = row.get::<i64>(0).map_err(TestError::Database)?;
            }
            assert!(
                user_exists > 0,
                "Post authorId {} should reference an existing user",
                author_id
            );
        }
    }

    // Query accounts with user relationship
    let account_query = r#"
        query GetAccountsWithUsers {
            account {
                id
                userId
                user {
                    id
                    name
                }
            }
        }
    "#;

    let rows = db.execute_query(account_query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("account"),
        "Results should contain 'account' field"
    );
    let accounts = results.get("account").unwrap();

    // Verify foreign key relationships are valid
    for account in accounts {
        let user_id = account
            .get("userId")
            .and_then(|v| v.as_i64())
            .expect("Account should have userId");
        let user = account
            .get("user")
            .and_then(|v| v.as_object())
            .expect("Account should have user relationship");
        let user_id_from_rel = user
            .get("id")
            .and_then(|v| v.as_i64())
            .expect("User should have id");

        assert_eq!(
            user_id, user_id_from_rel,
            "Account userId should match user.id from relationship"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_seed_with_foreign_key_ratios() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Create options with custom foreign key ratio
    let mut options = seed::Options::default();
    options.default_rows_per_table = 10;
    // Set ratio: 10 posts per user
    options
        .foreign_key_ratios
        .insert(("User".to_string(), "Post".to_string()), 10.0);

    // Generate seed data
    let operations = seed::seed_database(&db.schema, &db.context, Some(options));

    // Execute seed data
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Verify approximate ratio (allowing for ±20% variation)
    let mut user_rows = db.execute_raw("SELECT COUNT(*) FROM users").await?;
    let mut user_count = 0;
    while let Some(row) = user_rows.next().await.map_err(TestError::Database)? {
        user_count = row.get::<i64>(0).map_err(TestError::Database)?;
    }

    let mut post_rows = db.execute_raw("SELECT COUNT(*) FROM posts").await?;
    let mut post_count = 0;
    while let Some(row) = post_rows.next().await.map_err(TestError::Database)? {
        post_count = row.get::<i64>(0).map_err(TestError::Database)?;
    }

    if user_count > 0 {
        let actual_ratio = post_count as f64 / user_count as f64;
        // Should be approximately 10, but allow for variation
        assert!(
            actual_ratio >= 8.0 && actual_ratio <= 12.0,
            "Post-to-user ratio should be approximately 10:1, but got {}:1 ({} posts / {} users)",
            actual_ratio,
            post_count,
            user_count
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_seed_union_types() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Generate seed data
    let operations = seed::seed_database(&db.schema, &db.context, None);

    // Execute seed data
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Query users with status (union type)
    let query = r#"
        query GetUsersWithStatus {
            user {
                id
                name
                status
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("user"),
        "Results should contain 'user' field"
    );
    let users = results.get("user").unwrap();
    assert!(!users.is_empty(), "Should have users");

    // Verify status field exists and is valid
    for user in users {
        let status = user.get("status").expect("User should have status field");
        // Status should be a string (variant name) or an object (variant with fields)
        assert!(
            status.is_string() || status.is_object(),
            "Status should be a string or object, got: {:?}",
            status
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_seed_all_tables_have_data() -> Result<(), TestError> {
    let db = SeedDatabase::new(&schema::full_schema()).await?;

    // Generate seed data
    let operations = seed::seed_database(&db.schema, &db.context, None);

    // Execute seed data
    let conn = db.db.connect().map_err(TestError::Database)?;
    for op in &operations {
        conn.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Verify all tables in the schema have data
    for (table_name, _table) in &db.context.tables {
        let sql_table_name = pyre::ast::get_tablename(&_table.record.name, &_table.record.fields);
        let count_query = format!(
            "SELECT COUNT(*) FROM {}",
            pyre::ext::string::quote(&sql_table_name)
        );

        let mut rows = db.execute_raw(&count_query).await?;
        let mut count = 0;
        while let Some(row) = rows.next().await.map_err(TestError::Database)? {
            count = row.get::<i64>(0).map_err(TestError::Database)?;
        }

        assert!(
            count > 0,
            "Table {} (record {}) should have at least one row, but got {}",
            sql_table_name,
            table_name,
            count
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_seed_deterministic() -> Result<(), TestError> {
    let db1 = SeedDatabase::new(&schema::full_schema()).await?;
    let db2 = SeedDatabase::new(&schema::full_schema()).await?;

    // Create options with the same seed
    let mut options = seed::Options::default();
    options.seed = Some(42);
    options.default_rows_per_table = 10;

    // Generate seed data for both databases with the same seed
    let operations1 = seed::seed_database(&db1.schema, &db1.context, Some(options.clone()));
    let operations2 = seed::seed_database(&db2.schema, &db2.context, Some(options));

    // Verify that the SQL operations are identical (deterministic)
    assert_eq!(
        operations1.len(),
        operations2.len(),
        "Should generate the same number of operations"
    );

    for (i, (op1, op2)) in operations1.iter().zip(operations2.iter()).enumerate() {
        assert_eq!(
            op1.sql, op2.sql,
            "Operation {} should be identical with the same seed",
            i
        );
    }

    // Execute on both databases
    let conn1 = db1.db.connect().map_err(TestError::Database)?;
    for op in &operations1 {
        conn1.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    let conn2 = db2.db.connect().map_err(TestError::Database)?;
    for op in &operations2 {
        conn2.execute(&op.sql, ()).await.map_err(|e| {
            TestError::TypecheckError(format!(
                "Failed to execute seed SQL: {}\nSQL: {}",
                e, op.sql
            ))
        })?;
    }

    // Verify data is the same in both databases
    let mut rows1 = db1.execute_raw("SELECT COUNT(*) FROM users").await?;
    let mut count1 = 0;
    while let Some(row) = rows1.next().await.map_err(TestError::Database)? {
        count1 = row.get::<i64>(0).map_err(TestError::Database)?;
    }

    let mut rows2 = db2.execute_raw("SELECT COUNT(*) FROM users").await?;
    let mut count2 = 0;
    while let Some(row) = rows2.next().await.map_err(TestError::Database)? {
        count2 = row.get::<i64>(0).map_err(TestError::Database)?;
    }

    assert_eq!(
        count1, count2,
        "Both databases should have the same number of users"
    );

    Ok(())
}
