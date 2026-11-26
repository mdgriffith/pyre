use pyre::db::diff;
use std::env;
use std::fs;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::path::{Path, PathBuf};

pub mod error;
pub mod introspect;

#[derive(Debug)]
pub enum DbError {
    AuthTokenRequired(String),
    EnvVarNotFound(String),
    DatabaseError { database: String, cause: String },
    LocalFilesystemError(std::io::Error, PathBuf),
}

impl DbError {
    pub fn format_error(&self) -> String {
        match self {
            DbError::AuthTokenRequired(database) => pyre::error::format_custom_error(
                "Authentication Error",
                &format!("Cannot connect to remote database {} without an authentication token.\nPass --auth <token> or --auth '$VARIABLE'.", error::database_location(database)),
            ),
            DbError::EnvVarNotFound(var) => pyre::error::format_custom_error(
                "Unknown Environment Variable",
                &format!("Environment variable {} is not set. Set it in the environment running this command, then retry.", var),
            ),
            DbError::DatabaseError { database, cause } => pyre::error::format_custom_error(
                "Cannot Connect To Database",
                &format!("Database: {}\n\n{}", error::database_location(database), cause),
            ),
            DbError::LocalFilesystemError(e, path) => pyre::error::format_custom_error(
                "Local Database Path Error",
                &format!("Failed to create local database directory {}: {}", path.display(), e),
            ),
        }
    }
}

fn ensure_local_db_parent_exists(db_path: &str) -> Result<(), DbError> {
    if db_path == ":memory:" {
        return Ok(());
    }
    if let Some(parent) = Path::new(db_path).parent() {
        if parent.as_os_str().is_empty() || parent.exists() {
            return Ok(());
        }
        fs::create_dir_all(parent)
            .map_err(|e| DbError::LocalFilesystemError(e, parent.to_path_buf()))?;
    }
    Ok(())
}

fn parse_arg_or_env(arg: &str) -> Result<String, DbError> {
    if let Some(variable) = arg.strip_prefix('$') {
        env::var(variable).map_err(|_| DbError::EnvVarNotFound(variable.to_string()))
    } else {
        Ok(arg.to_string())
    }
}

pub fn is_remote(db: &str) -> Result<bool, DbError> {
    let value = parse_arg_or_env(db)?;
    Ok(value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("libsql://"))
}

pub async fn connect(
    db: &str,
    maybe_auth_token: &Option<String>,
) -> Result<libsql::Database, DbError> {
    connect_with_creation(db, maybe_auth_token, true).await
}

pub async fn connect_existing(
    db: &str,
    maybe_auth_token: &Option<String>,
) -> Result<libsql::Database, DbError> {
    connect_with_creation(db, maybe_auth_token, false).await
}

async fn connect_with_creation(
    db: &str,
    maybe_auth_token: &Option<String>,
    create: bool,
) -> Result<libsql::Database, DbError> {
    let db_value = parse_arg_or_env(db)?;
    let database_location = error::database_location(&db_value);
    if is_remote(&db_value)? {
        let Some(token) = maybe_auth_token else {
            return Err(DbError::AuthTokenRequired(database_location));
        };
        let token = parse_arg_or_env(token)?;
        libsql::Builder::new_remote(db_value, token.clone())
            .build()
            .await
            .map_err(|e| DbError::DatabaseError {
                database: database_location,
                cause: if token.is_empty() {
                    error::format_libsql_error(&e)
                } else {
                    error::format_libsql_error(&e).replace(&token, "<redacted>")
                },
            })
    } else {
        let mut builder = libsql::Builder::new_local(&db_value);
        if create {
            ensure_local_db_parent_exists(&db_value)?;
        } else {
            builder = builder.flags(libsql::OpenFlags::SQLITE_OPEN_READ_WRITE);
        }
        builder.build().await.map_err(|e| DbError::DatabaseError {
            database: database_location,
            cause: error::format_libsql_error(&e),
        })
    }
}

/// A connection kept alive until this short-lived CLI process exits.
///
/// libSQL currently closes local SQLite handles twice when the final connection is dropped,
/// which can segfault on platforms such as Linux musl. Avoid that destructor in CLI commands;
/// the operating system reclaims the handle at process exit.
/// https://github.com/tursodatabase/libsql/issues/2251
pub struct ProcessConnection(ManuallyDrop<libsql::Connection>);

impl ProcessConnection {
    pub fn connect(db: &libsql::Database) -> Result<Self, libsql::Error> {
        db.connect().map(|conn| Self(ManuallyDrop::new(conn)))
    }
}

impl Deref for ProcessConnection {
    type Target = libsql::Connection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub fn migration_validation_changes(db_diff: &diff::Diff) -> Vec<String> {
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
                diff::RecordChange::ModifiedField {
                    name,
                    changes: column,
                } => {
                    changes.push(column_change_description(&record_diff.name, name, column));
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

pub fn column_change_description(table: &str, name: &str, column: &diff::ColumnDiff) -> String {
    let mut details = Vec::new();
    if let Some((old, new)) = &column.type_changed {
        details.push(format!("type {old} -> {new}"));
    }
    if let Some((old, new)) = &column.nullable_changed {
        // The physical diff stores SQLite's notnull flag.
        details.push(format!("allows null {} -> {}", !old, !new));
    }
    if let Some((old, new)) = &column.primary_key_changed {
        details.push(format!("primary key {old} -> {new}"));
    }
    format!("column {table}.{name}: {}", details.join(", "))
}
