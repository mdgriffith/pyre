use libsql::Database;
use pyre::ast;
use pyre::db::diff;
use pyre::db::introspect;
use pyre::db::migrate;
use pyre::error;
use pyre::parser;
use pyre::typecheck;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use tempfile::TempDir;

use super::error::TestError;
use pyre::generate::sql::to_sql::SqlAndParams;

pub struct TestDatabase {
    pub db: Database,
    pub temp_dir: TempDir,
    pub context: typecheck::Context,
    pub schema: ast::Schema,
}

impl TestDatabase {
    /// Create a new test database with a schema
    pub async fn new(schema_source: &str) -> Result<Self, TestError> {
        let temp_dir = TempDir::new().map_err(TestError::Io)?;
        let db_path = temp_dir.path().join("test.db");
        let db_path_str = db_path.to_str().ok_or(TestError::InvalidPath)?;

        // Create database
        let db = libsql::Builder::new_local(db_path_str)
            .build()
            .await
            .map_err(TestError::Database)?;

        // Parse schema
        let mut schema = ast::Schema::default();
        parser::run("schema.pyre", schema_source, &mut schema)
            .map_err(|e| TestError::ParseError(parser::render_error(schema_source, e, false)))?;

        // Typecheck schema
        let database = ast::Database {
            schemas: vec![schema.clone()],
        };
        let context = typecheck::check_schema(&database)
            .map_err(|errors| TestError::TypecheckError(format_errors(schema_source, &errors)))?;

        // Create empty introspection for a fresh database
        let introspection = introspect::Introspection {
            tables: vec![],
            migration_state: introspect::MigrationState::NoMigrationTable,
            schema: introspect::SchemaResult::Success {
                schema: ast::Schema::default(),
                context: typecheck::empty_context(),
            },
        };

        // Generate migration SQL
        let db_diff = diff::diff(&context, &schema, &introspection);
        let mut migration_sql = diff::to_sql::to_sql(&db_diff);

        // Add migration table creation if needed
        match introspection.migration_state {
            introspect::MigrationState::NoMigrationTable => {
                migration_sql.splice(0..0, migrate::internal_setup_sql());
            }
            introspect::MigrationState::MigrationTable { .. } => {}
        }

        // Add schema insertion
        let schema_string = pyre::generate::to_string::schema_to_string("", &schema);
        migration_sql.push(SqlAndParams::SqlWithParams {
            sql: migrate::INSERT_SCHEMA.to_string(),
            args: vec![schema_string],
        });

        // Execute migration
        let conn = db.connect().map_err(TestError::Database)?;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .map_err(TestError::Database)?;

        // Collect SQL strings for error reporting
        let mut sql_strings: Vec<String> = Vec::new();
        for stmt in &migration_sql {
            match stmt {
                SqlAndParams::Sql(s) => {
                    sql_strings.push(s.clone());
                }
                SqlAndParams::SqlWithParams { sql: s, args } => {
                    sql_strings.push(format!("{} with args: {:?}", s, args));
                }
            }
        }

        for (idx, sql_stmt) in migration_sql.iter().enumerate() {
            match sql_stmt {
                SqlAndParams::Sql(sql) => {
                    if let Err(e) = tx.execute_batch(sql).await {
                        // On error, print all SQL statements for debugging
                        eprintln!(
                            "Migration failed at statement {} of {}. SQL statements:",
                            idx + 1,
                            sql_strings.len()
                        );
                        for (i, sql_str) in sql_strings.iter().enumerate() {
                            if i == idx {
                                eprintln!("  [{}] {} <-- FAILED HERE", i + 1, sql_str);
                            } else {
                                eprintln!("  [{}] {}", i + 1, sql_str);
                            }
                        }
                        return Err(TestError::Database(e));
                    }
                }
                SqlAndParams::SqlWithParams { sql, args } => {
                    // Convert args to libsql::Value
                    let values: Vec<libsql::Value> = args
                        .iter()
                        .map(|s| libsql::Value::Text(s.clone()))
                        .collect();
                    if let Err(e) = tx.execute(sql, libsql::params_from_iter(values)).await {
                        // On error, print all SQL statements for debugging
                        eprintln!(
                            "Migration failed at statement {} of {}. SQL statements:",
                            idx + 1,
                            sql_strings.len()
                        );
                        for (i, sql_str) in sql_strings.iter().enumerate() {
                            if i == idx {
                                eprintln!("  [{}] {} <-- FAILED HERE", i + 1, sql_str);
                            } else {
                                eprintln!("  [{}] {}", i + 1, sql_str);
                            }
                        }
                        return Err(TestError::Database(e));
                    }
                }
            }
        }

        tx.commit().await.map_err(TestError::Database)?;

        let db = TestDatabase {
            db,
            temp_dir,
            context,
            schema,
        };
        let _ = db.temp_dir.path();
        Ok(db)
    }

    /// Execute a query and return the SQL that would be generated
    /// Returns a vector of (include_flag, sql) tuples where include_flag indicates if the statement returns results
    pub fn generate_query_sql(
        &self,
        query_source: &str,
    ) -> Result<Vec<(bool, SqlAndParams)>, TestError> {
        let query_list = parser::parse_query("query.pyre", query_source)
            .map_err(|e| TestError::ParseError(parser::render_error(query_source, e, false)))?;

        // Use the existing context, but update the filepath
        // Note: We can't clone Context because Table and Type don't implement Clone
        // So we'll use a reference to the existing context
        let context = &self.context;

        let query_info = typecheck::check_queries(&query_list, &context)
            .map_err(|errors| TestError::TypecheckError(format_errors(query_source, &errors)))?;

        // Get the first query
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

        // Get the table for this query
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

        // Generate SQL using the to_string function
        let prepared_statements =
            pyre::generate::sql::to_string(context, query, info, table, table_field);

        // Convert Prepared statements to SqlAndParams
        // Note: For inserts and complex selects, we need to include ALL statements
        // (even those marked as ignore) because they create temp tables that are needed
        // We preserve the include flag to know which statements return results
        let mut sql_statements = Vec::new();
        for prepared in prepared_statements {
            sql_statements.push((prepared.include, SqlAndParams::Sql(prepared.sql)));
        }

        Ok(sql_statements)
    }

    /// Execute a query with parameters and return results
    pub async fn execute_query_with_params(
        &self,
        query_source: &str,
        params: HashMap<String, libsql::Value>,
    ) -> Result<Vec<libsql::Rows>, TestError> {
        // Parse query to get parameter names in order
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

        // Extract parameter names in order
        let param_names: Vec<String> = query.args.iter().map(|arg| arg.name.clone()).collect();

        let sql_statements = self.generate_query_sql(query_source)?;

        let conn = self.db.connect().map_err(TestError::Database)?;
        let mut results = Vec::new();

        // Execute statements sequentially, using include flag to determine if they return results
        for (include, sql_stmt) in sql_statements {
            match sql_stmt {
                SqlAndParams::Sql(sql) => {
                    // Build parameter values in the order they appear in THIS SQL statement
                    let mut param_values_for_stmt = Vec::new();
                    // Find parameters in the order they appear in SQL
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
                            if param_names.contains(&param_name) {
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
                        // This statement returns results - use query()
                        // Note: Rows objects may hold locks until consumed, but we need to return them
                        // The caller is responsible for consuming rows before executing more statements
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
                        // This statement doesn't return results - use execute()
                        // However, if the SQL contains RETURNING, we must use query() instead
                        // and consume the rows (they won't be added to results)
                        let has_returning = sql_with_params.to_uppercase().contains("RETURNING");
                        if has_returning {
                            // Statement has RETURNING, so it returns rows - use query() but don't add to results
                            if param_values_for_stmt.is_empty() {
                                let mut rows = conn
                                    .query(&sql_with_params, ())
                                    .await
                                    .map_err(TestError::Database)?;
                                // Consume all rows to avoid holding locks
                                while rows.next().await.map_err(TestError::Database)?.is_some() {}
                            } else {
                                let mut rows = conn
                                    .query(
                                        &sql_with_params,
                                        libsql::params_from_iter(param_values_for_stmt.clone()),
                                    )
                                    .await
                                    .map_err(TestError::Database)?;
                                // Consume all rows to avoid holding locks
                                while rows.next().await.map_err(TestError::Database)?.is_some() {}
                            }
                        } else {
                            // No RETURNING, safe to use execute()
                            if param_values_for_stmt.is_empty() {
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
                }
                SqlAndParams::SqlWithParams { sql, args } => {
                    // Build parameter values in the order they appear in THIS SQL statement
                    let mut param_values_for_stmt = Vec::new();
                    // Find parameters in the order they appear in SQL
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
                            if param_names.contains(&param_name) {
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
                        args.into_iter().map(|s| libsql::Value::Text(s)).collect();
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

    /// Execute a query and return results
    pub async fn execute_query(&self, query_source: &str) -> Result<Vec<libsql::Rows>, TestError> {
        self.execute_query_with_params(query_source, HashMap::new())
            .await
    }

    /// Execute an insert query with parameters
    pub async fn execute_insert_with_params(
        &self,
        insert_query: &str,
        params: HashMap<String, libsql::Value>,
    ) -> Result<Vec<libsql::Rows>, TestError> {
        self.execute_query_with_params(insert_query, params).await
    }

    /// Execute a query with parameters and session, returning results
    pub async fn execute_query_with_session(
        &self,
        query_source: &str,
        params: HashMap<String, libsql::Value>,
        session: HashMap<String, libsql::Value>,
        log_sql: bool,
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

        // Extract regular parameter names in order
        let param_names: Vec<String> = query.args.iter().map(|arg| arg.name.clone()).collect();

        // Parse and typecheck to get QueryInfo
        let context = &self.context;
        let query_info = typecheck::check_queries(&query_list, &context)
            .map_err(|errors| TestError::TypecheckError(format_errors(query_source, &errors)))?;

        let info = query_info
            .get(&query.name)
            .ok_or(TestError::NoQueryInfoFound)?;

        // Get SQL statements
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

        // Extract session parameters from QueryInfo
        let mut all_params = params.clone();
        for (_var_name, param_info) in &info.variables {
            if let typecheck::ParamInfo::Defined {
                from_session,
                used,
                session_name,
                ..
            } = param_info
            {
                if *from_session && *used {
                    if let Some(session_field) = session_name {
                        // Session variables are named session_fieldName in SQL
                        let sql_param_name = format!("session_{}", session_field);
                        if let Some(value) = session.get(session_field) {
                            all_params.insert(sql_param_name, value.clone());
                        }
                    }
                }
            }
        }

        // Collect all session param names that are used
        let mut session_param_names = Vec::new();
        for (_var_name, param_info) in &info.variables {
            if let typecheck::ParamInfo::Defined {
                from_session,
                used,
                session_name,
                ..
            } = param_info
            {
                if *from_session && *used {
                    if let Some(session_field) = session_name {
                        let sql_param_name = format!("session_{}", session_field);
                        session_param_names.push(sql_param_name);
                    }
                }
            }
        }

        // Combine all param names for replacement
        let mut all_param_names = param_names.clone();
        all_param_names.extend(session_param_names.clone());

        // Collect parameter values in the order they appear in SQL
        // We need to find the order parameters appear in each SQL statement
        let mut param_values: Vec<libsql::Value> = Vec::new();

        // For each SQL statement, collect parameters in the order they appear
        for (_, sql_stmt) in &sql_statements {
            if let SqlAndParams::Sql(sql) = sql_stmt {
                // Find parameters in the order they appear in this SQL
                let mut seen_in_this_sql = std::collections::HashSet::new();
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
                        if all_param_names.contains(&param_name)
                            && !seen_in_this_sql.contains(&param_name)
                        {
                            seen_in_this_sql.insert(param_name.clone());
                            if let Some(value) = all_params.get(&param_name) {
                                param_values.push(value.clone());
                            } else {
                                param_values.push(libsql::Value::Null);
                            }
                        }
                    }
                }
            }
        }

        let conn = self.db.connect().map_err(TestError::Database)?;
        let mut results = Vec::new();

        // Execute statements sequentially
        for (include, sql_stmt) in sql_statements {
            match sql_stmt {
                SqlAndParams::Sql(sql) => {
                    let sql_with_params = if all_param_names.is_empty() {
                        sql.clone()
                    } else {
                        replace_params_positional(&sql, &all_param_names)
                    };

                    if log_sql {
                        eprintln!("[SQL] {}", sql_with_params);
                        if !param_values.is_empty() {
                            eprintln!("[SQL Params] {:?}", param_values);
                        }
                    }

                    if include {
                        if param_values.is_empty() {
                            let rows = conn
                                .query(&sql_with_params, ())
                                .await
                                .map_err(TestError::Database)?;
                            results.push(rows);
                        } else {
                            let rows = conn
                                .query(
                                    &sql_with_params,
                                    libsql::params_from_iter(param_values.clone()),
                                )
                                .await
                                .map_err(TestError::Database)?;
                            results.push(rows);
                        }
                    } else {
                        // This statement doesn't return results - use execute()
                        // However, if the SQL contains RETURNING, we must use query() instead
                        // and consume the rows (they won't be added to results)
                        let has_returning = sql_with_params.to_uppercase().contains("RETURNING");
                        if has_returning {
                            // Statement has RETURNING, so it returns rows - use query() but don't add to results
                            if param_values.is_empty() {
                                let mut rows = conn
                                    .query(&sql_with_params, ())
                                    .await
                                    .map_err(TestError::Database)?;
                                // Consume all rows to avoid holding locks
                                while rows.next().await.map_err(TestError::Database)?.is_some() {}
                            } else {
                                let mut rows = conn
                                    .query(
                                        &sql_with_params,
                                        libsql::params_from_iter(param_values.clone()),
                                    )
                                    .await
                                    .map_err(TestError::Database)?;
                                // Consume all rows to avoid holding locks
                                while rows.next().await.map_err(TestError::Database)?.is_some() {}
                            }
                        } else {
                            // No RETURNING, safe to use execute()
                            if param_values.is_empty() {
                                conn.execute(&sql_with_params, ())
                                    .await
                                    .map_err(TestError::Database)?;
                            } else {
                                conn.execute(
                                    &sql_with_params,
                                    libsql::params_from_iter(param_values.clone()),
                                )
                                .await
                                .map_err(TestError::Database)?;
                            }
                        }
                    }
                }
                SqlAndParams::SqlWithParams { sql, args } => {
                    let mut values: Vec<libsql::Value> =
                        args.into_iter().map(|s| libsql::Value::Text(s)).collect();
                    values.extend(param_values.clone());
                    let sql_with_params = if all_param_names.is_empty() {
                        sql.clone()
                    } else {
                        replace_params_positional(&sql, &all_param_names)
                    };

                    if include {
                        let rows = conn
                            .query(&sql_with_params, libsql::params_from_iter(values))
                            .await
                            .map_err(TestError::Database)?;
                        results.push(rows);
                    } else {
                        // This statement doesn't return results - use execute()
                        // However, if the SQL contains RETURNING, we must use query() instead
                        // and consume the rows (they won't be added to results)
                        let has_returning = sql_with_params.to_uppercase().contains("RETURNING");
                        if has_returning {
                            // Statement has RETURNING, so it returns rows - use query() but don't add to results
                            let mut rows = conn
                                .query(&sql_with_params, libsql::params_from_iter(values))
                                .await
                                .map_err(TestError::Database)?;
                            // Consume all rows to avoid holding locks
                            while rows.next().await.map_err(TestError::Database)?.is_some() {}
                        } else {
                            // No RETURNING, safe to use execute()
                            conn.execute(&sql_with_params, libsql::params_from_iter(values))
                                .await
                                .map_err(TestError::Database)?;
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    /// Execute an insert query with parameters and session
    pub async fn execute_insert_with_session(
        &self,
        insert_query: &str,
        params: HashMap<String, libsql::Value>,
        session: HashMap<String, libsql::Value>,
    ) -> Result<Vec<libsql::Rows>, TestError> {
        self.execute_query_with_session(insert_query, params, session, false)
            .await
    }

    /// Parse JSON results from query execution
    /// Returns a map of field names to arrays of JSON objects
    /// Queries return JSON in a column named "result" which contains an object with query field names as keys
    pub async fn parse_query_results(
        &self,
        rows: Vec<libsql::Rows>,
    ) -> Result<HashMap<String, Vec<JsonValue>>, TestError> {
        let mut result = HashMap::new();

        for mut rows_set in rows {
            // Get column names
            let column_count = rows_set.column_count();
            if column_count == 0 {
                continue;
            }

            for i in 0..column_count {
                let col_name = rows_set.column_name(i).ok_or(TestError::NoQueryFound)?;
                if !col_name.starts_with('_') {
                    result.entry(col_name.to_string()).or_insert_with(Vec::new);
                }
            }

            // Process rows - queries return JSON in columns named after field names
            while let Some(row) = rows_set.next().await.map_err(TestError::Database)? {
                for i in 0..column_count {
                    let col_name = rows_set.column_name(i).ok_or(TestError::NoQueryFound)?;

                    // Get the JSON value from the column
                    if let Ok(json_str) = row.get::<String>(i as i32) {
                        if let Ok(json_value) = serde_json::from_str::<JsonValue>(&json_str) {
                            // Column name is the field name, value is already an array
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

    /// Execute raw SQL and return rows
    pub async fn execute_raw(&self, sql: &str) -> Result<libsql::Rows, TestError> {
        let conn = self.db.connect().map_err(TestError::Database)?;
        conn.query(sql, ()).await.map_err(TestError::Database)
    }

    /// Seed the database with standard test data
    /// This creates users, posts, and accounts based on the schema
    pub async fn seed_standard_data(&self) -> Result<(), TestError> {
        // Check if schema has User record (table names are lowercase in context)
        if self.context.tables.contains_key("user") {
            // Check if User has status field (for schemas with union types)
            let has_status = self
                .context
                .tables
                .get("user")
                .map(|t| {
                    t.record.fields.iter().any(|f| match f {
                        pyre::ast::Field::Column(col) => col.name == "status",
                        _ => false,
                    })
                })
                .unwrap_or(false);

            if has_status {
                // Insert users with status
                let insert_user = r#"
                    insert CreateUser($name: String, $status: Status) {
                        user {
                            name = $name
                            status = $status
                        }
                    }
                "#;

                let mut params = HashMap::new();
                params.insert("name".to_string(), libsql::Value::Text("Alice".to_string()));
                params.insert(
                    "status".to_string(),
                    libsql::Value::Text("Active".to_string()),
                );
                self.execute_insert_with_params(insert_user, params).await?;

                let mut params = HashMap::new();
                params.insert("name".to_string(), libsql::Value::Text("Bob".to_string()));
                params.insert(
                    "status".to_string(),
                    libsql::Value::Text("Inactive".to_string()),
                );
                self.execute_insert_with_params(insert_user, params).await?;

                let mut params = HashMap::new();
                params.insert(
                    "name".to_string(),
                    libsql::Value::Text("Charlie".to_string()),
                );
                params.insert(
                    "status".to_string(),
                    libsql::Value::Text("Special".to_string()),
                );
                self.execute_insert_with_params(insert_user, params).await?;
            } else {
                // Insert users without status
                let insert_user = r#"
                    insert CreateUser($name: String) {
                        user {
                            name = $name
                        }
                    }
                "#;

                let mut params = HashMap::new();
                params.insert("name".to_string(), libsql::Value::Text("Alice".to_string()));
                self.execute_insert_with_params(insert_user, params).await?;

                let mut params = HashMap::new();
                params.insert("name".to_string(), libsql::Value::Text("Bob".to_string()));
                self.execute_insert_with_params(insert_user, params).await?;
            }
        }

        // Check if schema has Post record (table names are lowercase in context)
        if self.context.tables.contains_key("post") {
            let insert_post = r#"
                insert CreatePost($title: String, $content: String, $authorId: Int) {
                    post {
                        title = $title
                        content = $content
                        authorId = $authorId
                    }
                }
            "#;

            let mut params = HashMap::new();
            params.insert(
                "title".to_string(),
                libsql::Value::Text("First Post".to_string()),
            );
            params.insert(
                "content".to_string(),
                libsql::Value::Text("Content here".to_string()),
            );
            params.insert("authorId".to_string(), libsql::Value::Integer(1));
            self.execute_insert_with_params(insert_post, params).await?;

            let mut params = HashMap::new();
            params.insert(
                "title".to_string(),
                libsql::Value::Text("Second Post".to_string()),
            );
            params.insert(
                "content".to_string(),
                libsql::Value::Text("More content".to_string()),
            );
            params.insert("authorId".to_string(), libsql::Value::Integer(1));
            self.execute_insert_with_params(insert_post, params).await?;
        }

        // Check if schema has Account record (table names are lowercase in context)
        if self.context.tables.contains_key("account") {
            let insert_account = r#"
                insert CreateAccount($userId: Int, $name: String, $status: String) {
                    account {
                        userId = $userId
                        name = $name
                        status = $status
                    }
                }
            "#;

            let mut params = HashMap::new();
            params.insert("userId".to_string(), libsql::Value::Integer(1));
            params.insert(
                "name".to_string(),
                libsql::Value::Text("Account 1".to_string()),
            );
            params.insert(
                "status".to_string(),
                libsql::Value::Text("active".to_string()),
            );
            self.execute_insert_with_params(insert_account, params)
                .await?;
        }

        Ok(())
    }
}

fn format_errors(schema_source: &str, errors: &[error::Error]) -> String {
    errors
        .iter()
        .map(|e| error::format_error(schema_source, e, false))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace $param_name placeholders with ? for positional parameters
/// Parameters are replaced in the order they appear in param_names
fn replace_params_positional(sql: &str, param_names: &[String]) -> String {
    let mut result = sql.to_string();
    // Replace parameters in the order they appear in the SQL, not in param_names order
    // This ensures the positional placeholders match the parameter values order
    let mut param_order = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Find all parameters in the order they appear in SQL
    let mut chars = result.chars().peekable();
    let mut i = 0;
    while let Some(ch) = chars.next() {
        if ch == '$' {
            let _start = i;
            let mut param_name = String::new();
            i += 1; // skip $
            while let Some(&next_ch) = chars.peek() {
                if next_ch.is_alphanumeric() || next_ch == '_' {
                    param_name.push(chars.next().unwrap());
                    i += 1;
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
        } else {
            i += 1;
        }
    }

    // Replace parameters in the order they appear in SQL
    for name in &param_order {
        result = result.replace(&format!("${}", name), "?");
    }

    // Replace any remaining parameters that weren't found in order
    for name in param_names {
        if !seen.contains(name) {
            result = result.replace(&format!("${}", name), "?");
        }
    }

    result
}
