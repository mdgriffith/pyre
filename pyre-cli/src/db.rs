use pyre::ast;
use pyre::db::diff;
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::typecheck;

use libsql;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub mod error;
pub mod introspect;

#[derive(Debug)]
pub enum DbError {
    AuthTokenRequired,
    EnvVarNotFound(String),
    DatabaseError(libsql::Error),
    LocalFilesystemError(std::io::Error, PathBuf),
}

impl DbError {
    pub fn format_error(&self) -> String {
        match self {
            DbError::AuthTokenRequired => pyre::error::format_custom_error(
                "Authentication Error",
                "Authentication token is required",
            ),
            DbError::EnvVarNotFound(var) => pyre::error::format_custom_error(
                "Unknown Environment Variable",
                &format!("Environment variable {} not found", var),
            ),
            DbError::DatabaseError(e) => pyre::error::format_custom_error(
                "Database Error",
                &format!("Database error: {:?}", e),
            ),
            DbError::LocalFilesystemError(e, path) => pyre::error::format_custom_error(
                "Local Database Path Error",
                &format!(
                    "Failed to create local database directory {}: {}",
                    path.display(),
                    e
                ),
            ),
        }
    }
}

fn ensure_local_db_parent_exists(db_path: &str) -> Result<(), DbError> {
    if db_path == ":memory:" {
        return Ok(());
    }

    let path = Path::new(db_path);
    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() || parent.exists() {
            return Ok(());
        }

        fs::create_dir_all(parent)
            .map_err(|e| DbError::LocalFilesystemError(e, parent.to_path_buf()))?;
    }

    Ok(())
}

fn parse_arg_or_env(arg: &str) -> Result<String, DbError> {
    if arg.starts_with('$') {
        let env_var_name = &arg[1..];
        env::var(env_var_name).map_err(|_| DbError::EnvVarNotFound(env_var_name.to_string()))
    } else {
        Ok(arg.to_string())
    }
}

pub async fn connect(
    db: &String,
    maybe_auth_token: &Option<String>,
) -> Result<libsql::Database, DbError> {
    let db_value = parse_arg_or_env(&db)?;

    if db_value.starts_with("http://")
        || db_value.starts_with("https://")
        || db_value.starts_with("libsql://")
    {
        // Remote database
        match maybe_auth_token {
            None => return Err(DbError::AuthTokenRequired),
            Some(token) => {
                let token_value = parse_arg_or_env(&token)?;
                let connected_result = libsql::Builder::new_remote(db_value, token_value)
                    .build()
                    .await;
                match connected_result {
                    Ok(connected) => return Ok(connected),
                    Err(e) => return Err(DbError::DatabaseError(e)),
                }
            }
        }
    } else {
        ensure_local_db_parent_exists(&db_value)?;

        let connected_result = libsql::Builder::new_local(db_value).build().await;
        match connected_result {
            Ok(connected) => return Ok(connected),
            Err(e) => return Err(DbError::DatabaseError(e)),
        }
    }
}

// Migrations

#[derive(Debug)]
pub enum MigrationError {
    SqlError(libsql::Error),
    MigrationReadIoError(std::io::Error, PathBuf),
    NoMigrationsFound(PathBuf),
    IncompatibleDatabase {
        db_path: String,
        tables: Vec<String>,
    },
    NamespaceNotFound {
        requested_namespace: String,
        found_namespaces: Vec<String>,
        db_path: String,
    },
    AppliedMigrationChanged {
        name: String,
    },
    MigrationValidationFailed {
        changes: Vec<String>,
    },
}

impl MigrationError {
    pub fn format_error(&self) -> String {
        match self {
            MigrationError::SqlError(sql_error) => error::format_libsql_error(sql_error),
            MigrationError::MigrationReadIoError(io_error, path) => {
                pyre::error::format_custom_error(
                    "Migration Read Error",
                    &format!(
                    "I was looking for migrations in {},\nbut ran into the following issue:\n\n{}",
                    pyre::error::yellow_if(true, &path.display().to_string()),
                    io_error
                ),
                )
            }
            MigrationError::NoMigrationsFound(path) => pyre::error::format_custom_error(
                "No Migrations Found",
                &format!(
                    "No migrations were found in {}.\n\nRun `pyre migration --db <database> init` to generate your first migration.",
                    pyre::error::yellow_if(true, &path.display().to_string())
                ),
            ),
            MigrationError::IncompatibleDatabase { db_path, tables } => pyre::error::format_custom_error(
                "Non-Pyre or Incompatible Database",
                &format!(
                    "The database at {} has existing tables but no compatible Pyre metadata.\n\nTables found:\n{}\n\nGuidance:\n- If this should be managed by Pyre, migrate from a fresh database or reseed.\n- If this database is already in use by another tool, use a separate database for Pyre.\n- You can run `pyre init` and then `pyre migration --db <database> init` for a new Pyre-managed setup.",
                    db_path,
                    if tables.is_empty() {
                        "  (none)".to_string()
                    } else {
                        tables
                            .iter()
                            .map(|t| format!("  - {}", t))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                ),
            ),
            MigrationError::NamespaceNotFound {
                requested_namespace,
                found_namespaces,
                db_path,
            } => pyre::error::format_custom_error(
                "Namespace Missing In Database Metadata",
                &format!(
                    "Requested namespace: {}\nNamespaces found: {}\nDatabase: {}\n\nThis database already has Pyre metadata for a different namespace.",
                    requested_namespace,
                    if found_namespaces.is_empty() {
                        "(none)".to_string()
                    } else {
                        found_namespaces.join(", ")
                    },
                    db_path
                ),
            ),
            MigrationError::AppliedMigrationChanged { name } => pyre::error::format_custom_error(
                "Applied Migration Changed",
                &format!(
                    "Migration {} has already been applied, but its migration.sql no longer matches the SQL recorded in the database.\n\nCreate a new migration instead of editing an applied migration.",
                    pyre::error::yellow_if(true, name)
                ),
            ),
            MigrationError::MigrationValidationFailed { changes } => pyre::error::format_custom_error(
                "Migration Validation Failed",
                &format!(
                    "After applying pending migration files, the database still does not match the current schema:\n\n{}\n\nGenerate or edit a migration.sql file so the migration explicitly performs these changes.",
                    changes
                        .iter()
                        .map(|change| format!("  - {}", change))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ),
        }
    }
}

async fn get_applied_migration_sql(
    conn: &libsql::Connection,
) -> Result<HashMap<String, String>, MigrationError> {
    let mut rows = conn
        .query(
            &format!(
                "select name, sql from {}",
                pyre::ext::string::quote(pyre::db::migrate::MIGRATION_TABLE)
            ),
            (),
        )
        .await
        .map_err(MigrationError::SqlError)?;

    let mut applied = HashMap::new();
    while let Some(row) = rows.next().await.map_err(MigrationError::SqlError)? {
        let name: String = row.get(0).map_err(MigrationError::SqlError)?;
        let sql: String = row.get(1).map_err(MigrationError::SqlError)?;
        applied.insert(name, sql);
    }

    Ok(applied)
}

fn verify_applied_migration_sql_unchanged(
    migration_files: &[(String, String)],
    applied_sql: &HashMap<String, String>,
) -> Result<(), MigrationError> {
    for (name, sql) in migration_files {
        if let Some(recorded_sql) = applied_sql.get(name) {
            if recorded_sql != sql {
                return Err(MigrationError::AppliedMigrationChanged { name: name.clone() });
            }
        }
    }

    Ok(())
}

fn migration_validation_changes(db_diff: &diff::Diff) -> Vec<String> {
    let mut changes = Vec::new();

    for table in &db_diff.added {
        changes.push(format!("missing table {}", table.name));
    }

    for table in &db_diff.removed {
        changes.push(format!("unexpected table {}", table.name));
    }

    for record_diff in &db_diff.modified_records {
        for change in &record_diff.changes {
            match change {
                diff::RecordChange::AddedField(column) => {
                    changes.push(format!(
                        "missing column {}.{}",
                        record_diff.name, column.name
                    ));
                }
                diff::RecordChange::RemovedField(column) => {
                    changes.push(format!(
                        "unexpected column {}.{}",
                        record_diff.name, column.name
                    ));
                }
                diff::RecordChange::ModifiedField { name, .. } => {
                    changes.push(format!("modified column {}.{}", record_diff.name, name));
                }
                diff::RecordChange::AddedIndex(index) => {
                    changes.push(format!("missing index {}", index.name));
                }
                diff::RecordChange::RemovedIndex(index) => {
                    changes.push(format!("unexpected index {}", index.name));
                }
            }
        }
    }

    changes
}

#[derive(Debug)]
pub struct MigrateOutcome {
    pub migrations_applied: usize,
    pub push_applied: bool,
}

impl MigrateOutcome {
    pub fn status_line(&self) -> String {
        match (self.migrations_applied, self.push_applied) {
            (0, false) => "Up to date, nothing applied.".to_string(),
            (1, false) => "1 migration applied.".to_string(),
            (count, false) => format!("{} migrations applied.", count),
            (0, true) => "No migrations applied + push.".to_string(),
            (1, true) => "1 migration applied + push.".to_string(),
            (count, true) => format!("{} migrations applied + push.", count),
        }
    }
}

pub struct MigrateOptions<'a> {
    pub schema: &'a ast::Schema,
    pub context: &'a typecheck::Context,
    pub migration_folder: &'a Path,
    pub migration_root: &'a Path,
    pub namespace: Option<&'a str>,
    pub db_path: &'a str,
}

fn detect_namespaces_from_migrations(
    migration_root: &Path,
    applied_migrations: &HashSet<String>,
) -> Vec<String> {
    let mut namespaces = Vec::new();
    let entries = match fs::read_dir(migration_root) {
        Ok(entries) => entries,
        Err(_) => return namespaces,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(namespace) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        let Ok(migration_names) = read_migration_items(&path) else {
            continue;
        };

        if migration_names
            .iter()
            .any(|name| applied_migrations.contains(name))
        {
            namespaces.push(namespace.to_string());
        }
    }

    namespaces.sort();
    namespaces.dedup();
    namespaces
}

async fn preflight_database_compatibility(
    conn: &libsql::Connection,
    options: &MigrateOptions<'_>,
) -> Result<(), MigrationError> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            (),
        )
        .await
        .map_err(MigrationError::SqlError)?;

    let mut table_names: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await.map_err(MigrationError::SqlError)? {
        let name: String = row.get(0).map_err(MigrationError::SqlError)?;
        table_names.push(name);
    }

    let has_migration_table = table_names
        .iter()
        .any(|name| name == pyre::db::migrate::MIGRATION_TABLE);
    let non_pyre_tables: Vec<String> = table_names
        .iter()
        .filter(|name| {
            name.as_str() != pyre::db::migrate::MIGRATION_TABLE
                && name.as_str() != pyre::db::migrate::SYNC_TABLE
        })
        .cloned()
        .collect();

    if !has_migration_table {
        if !non_pyre_tables.is_empty() {
            return Err(MigrationError::IncompatibleDatabase {
                db_path: options.db_path.to_string(),
                tables: non_pyre_tables,
            });
        }
        return Ok(());
    }

    if let Some(namespace) = options.namespace {
        let migration_state = introspect::get_migration_state(conn)
            .await
            .map_err(MigrationError::SqlError)?;

        let applied_names: HashSet<String> = match migration_state {
            pyre::db::introspect::MigrationState::NoMigrationTable => HashSet::new(),
            pyre::db::introspect::MigrationState::MigrationTable { migrations } => migrations
                .into_iter()
                .map(|migration| migration.name)
                .collect(),
        };

        let found_namespaces =
            detect_namespaces_from_migrations(options.migration_root, &applied_names);
        if !found_namespaces.is_empty() && !found_namespaces.iter().any(|name| name == namespace) {
            return Err(MigrationError::NamespaceNotFound {
                requested_namespace: namespace.to_string(),
                found_namespaces,
                db_path: options.db_path.to_string(),
            });
        }
    }

    Ok(())
}

/*

This doesn't do any checking on the schema or the migrations, it just runs them.


*/
pub async fn migrate(
    db: &libsql::Database,
    options: MigrateOptions<'_>,
) -> Result<MigrateOutcome, MigrationError> {
    let schema = options.schema;
    let migration_folder = options.migration_folder;
    let conn = db.connect().map_err(MigrationError::SqlError)?;
    preflight_database_compatibility(&conn, &options).await?;

    let migration_files = match read_migration_folder(migration_folder) {
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return Err(MigrationError::NoMigrationsFound(
                    migration_folder.to_path_buf(),
                ));
            }

            return Err(MigrationError::MigrationReadIoError(
                err,
                migration_folder.to_path_buf(),
            ));
        }
        Ok(files) => files,
    };

    if migration_files.file_contents.is_empty() {
        return Err(MigrationError::NoMigrationsFound(
            migration_folder.to_path_buf(),
        ));
    }

    for statement in pyre::db::migrate::quoted_internal_setup_sql() {
        match statement {
            SqlAndParams::Sql(sql) => {
                conn.execute_batch(&sql)
                    .await
                    .map_err(MigrationError::SqlError)?;
            }
            SqlAndParams::SqlWithParams { sql, args } => {
                conn.execute(&sql, libsql::params_from_iter(args))
                    .await
                    .map_err(MigrationError::SqlError)?;
            }
        }
    }

    let migration_state = introspect::get_migration_state(&conn)
        .await
        .map_err(MigrationError::SqlError)?;

    let applied_sql = get_applied_migration_sql(&conn).await?;
    verify_applied_migration_sql_unchanged(&migration_files.file_contents, &applied_sql)?;

    // Use centralized migration planning logic
    let migration_plan = pyre::db::migrate::plan_file_based_migrations(
        &migration_files.file_contents,
        &migration_state,
        schema,
    );

    let migrations_applied = migration_plan.migrations_to_run.len();

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(MigrationError::SqlError)?;

    // Execute migrations that need to be run
    for (index, (migration_filename, migration_contents)) in
        migration_plan.migrations_to_run.iter().enumerate()
    {
        tx.execute_batch(migration_contents)
            .await
            .map_err(MigrationError::SqlError)?;

        // Record migration using centralized constant
        // INSERT_MIGRATION_SUCCESS requires (name, sql, finished_at)
        // where finished_at is set to unixepoch() automatically
        let is_last = index + 1 == migration_plan.migrations_to_run.len();
        let insert_sql = (if is_last {
            pyre::db::migrate::INSERT_MIGRATION_SUCCESS_WITH_SCHEMA
        } else {
            pyre::db::migrate::INSERT_MIGRATION_SUCCESS
        })
        .replace(
            pyre::db::migrate::MIGRATION_TABLE,
            &pyre::ext::string::quote(pyre::db::migrate::MIGRATION_TABLE),
        );
        let params = if is_last {
            vec![
                libsql::Value::Text(migration_filename.clone()),
                libsql::Value::Text(migration_contents.clone()),
                libsql::Value::Text(migration_plan.schema_string.clone()),
            ]
        } else {
            vec![
                libsql::Value::Text(migration_filename.clone()),
                libsql::Value::Text(migration_contents.clone()),
            ]
        };
        tx.execute(&insert_sql, libsql::params_from_iter(params))
            .await
            .map_err(MigrationError::SqlError)?;
    }

    let introspection = introspect::introspect_connection(&tx)
        .await
        .map_err(MigrationError::SqlError)?;
    let validation_diff = diff::diff(options.context, schema, &introspection);
    if !diff::is_empty(&validation_diff) {
        let changes = migration_validation_changes(&validation_diff);
        tx.rollback().await.map_err(MigrationError::SqlError)?;
        return Err(MigrationError::MigrationValidationFailed { changes });
    }

    tx.commit().await.map_err(MigrationError::SqlError)?;

    Ok(MigrateOutcome {
        migrations_applied,
        push_applied: false,
    })
}

pub fn read_migration_items(migration_folder: &Path) -> Result<Vec<String>, std::io::Error> {
    let mut migration_items: Vec<String> = Vec::new();

    for entry in fs::read_dir(migration_folder)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            if let Some(folder_name) = path.file_name().and_then(|name| name.to_str()) {
                migration_items.push(folder_name.to_string());
            }
        }
    }

    migration_items.sort();

    Ok(migration_items)
}

pub struct Migrations {
    pub file_contents: Vec<(String, String)>,
}

pub fn read_migration_folder(migration_folder: &Path) -> Result<Migrations, std::io::Error> {
    let mut migration_dirs: Vec<(String, PathBuf)> = Vec::new();

    for entry in fs::read_dir(migration_folder)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let migrate_file_path = path.join("migration.sql");
            if migrate_file_path.is_file() {
                if let Some(folder_name) = path.file_name().and_then(|name| name.to_str()) {
                    migration_dirs.push((folder_name.to_string(), migrate_file_path));
                }
            }
        }
    }

    migration_dirs.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut file_contents: Vec<(String, String)> = Vec::new();
    for (folder_name, migrate_file_path) in migration_dirs {
        let mut file = fs::File::open(&migrate_file_path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        file_contents.push((folder_name, contents));
    }

    Ok(Migrations { file_contents })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_migration_folder_returns_migrations_in_name_order() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let root = temp_dir.path();

        for (name, sql) in [
            ("202601020000_second", "select 2;"),
            ("202601010000_first", "select 1;"),
        ] {
            let migration_dir = root.join(name);
            std::fs::create_dir(&migration_dir).unwrap();
            std::fs::write(migration_dir.join("migration.sql"), sql).unwrap();
        }

        let migrations = read_migration_folder(root).unwrap();

        assert_eq!(migrations.file_contents[0].0, "202601010000_first");
        assert_eq!(migrations.file_contents[1].0, "202601020000_second");
    }

    #[test]
    fn read_migration_items_returns_names_in_order() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let root = temp_dir.path();

        std::fs::create_dir(root.join("202601020000_second")).unwrap();
        std::fs::create_dir(root.join("202601010000_first")).unwrap();

        let names = read_migration_items(root).unwrap();

        assert_eq!(names, vec!["202601010000_first", "202601020000_second"]);
    }
}
