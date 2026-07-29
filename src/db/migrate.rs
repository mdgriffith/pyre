use crate::ast;
use crate::ast::diff;
use crate::db::diff as db_diff;
use crate::db::introspect;
use crate::error;
use crate::generate::sql::to_sql::SqlAndParams;
use crate::parser;
use crate::typecheck;

pub const MIGRATION_TABLE: &str = "_pyre_migrations";

pub const SYNC_TABLE: &str = "_pyre_sync";

pub const LIST_MIGRATIONS: &str = "select name from _pyre_migrations";

//
//
// Creating tables

pub const CREATE_MIGRATION_TABLE: &str = "create table if not exists _pyre_migrations (
    id integer not null primary key autoincrement,
    created_at integer not null default (unixepoch()),
    name text not null,
    finished_at integer,
    error text,
    sql text not null,
    schema text
)";

pub const CREATE_SYNC_TABLE: &str = "create table if not exists _pyre_sync (
    id integer not null primary key check (id = 1),
    database_epoch text not null,
    server_revision integer not null default 0 check (server_revision >= 0)
)";

pub const INSERT_SYNC_STATE_ROW: &str =
    "insert into _pyre_sync (id, database_epoch, server_revision) values (1, lower(hex(randomblob(16))), 0) on conflict(id) do nothing";

pub const INSERT_MIGRATION_ERROR: &str =
    "insert into _pyre_migrations (name, sql, error) values (?, ?, ?)";

pub const INSERT_MIGRATION_SUCCESS: &str =
    "insert into _pyre_migrations (name, sql, finished_at) values (?, ?, unixepoch())";

pub const INSERT_MIGRATION_SUCCESS_WITH_SCHEMA: &str =
    "insert into _pyre_migrations (name, sql, schema, finished_at) values (?, ?, ?, unixepoch())";

pub fn internal_setup_sql() -> Vec<SqlAndParams> {
    vec![
        SqlAndParams::Sql(CREATE_MIGRATION_TABLE.to_string()),
        SqlAndParams::Sql(CREATE_SYNC_TABLE.to_string()),
        SqlAndParams::Sql(INSERT_SYNC_STATE_ROW.to_string()),
    ]
}

pub fn quoted_internal_setup_sql() -> Vec<SqlAndParams> {
    internal_setup_sql()
        .into_iter()
        .map(|statement| match statement {
            SqlAndParams::Sql(sql) => SqlAndParams::Sql(quote_internal_table_names(&sql)),
            SqlAndParams::SqlWithParams { sql, args } => SqlAndParams::SqlWithParams {
                sql: quote_internal_table_names(&sql),
                args,
            },
        })
        .collect()
}

fn quote_internal_table_names(sql: &str) -> String {
    sql.replace(MIGRATION_TABLE, &crate::ext::string::quote(MIGRATION_TABLE))
        .replace(SYNC_TABLE, &crate::ext::string::quote(SYNC_TABLE))
}

/// Result type for dynamic migrations (used in WASM)
/// Contains SQL statements to execute and markers for success/failure
#[derive(serde::Serialize)]
pub struct MigrationSql {
    pub sql: Vec<SqlAndParams>,
    pub mark_success: SqlAndParams,
    pub mark_failure: SqlAndParams,
}

/// Dynamic migration approach - generates SQL from schema source without file operations.
/// This is used in WASM environments where file system access is not available.
///
/// Takes introspection results and a schema source string, parses and typechecks the schema,
/// diffs it against the recorded schema, and generates SQL migration statements.
pub fn migrate_dynamic(
    name: String,
    introspection: &introspect::Introspection,
    new_schema_source: &str,
    schema_filepath: &str,
) -> Result<MigrationSql, Vec<error::Error>> {
    // Parse the schema source into a Schema
    let mut new_schema = ast::Schema::default();
    let parse_result = parser::run(schema_filepath, new_schema_source, &mut new_schema);
    if let Err(e) = parse_result {
        return match parser::convert_parsing_error(e) {
            Some(error) => Err(vec![error]),
            None => Err(vec![error::Error {
                error_type: error::ErrorType::ParsingError(error::ParsingErrorDetails {
                    expecting: error::Expecting::PyreFile,
                }),
                filepath: schema_filepath.to_string(),
                locations: vec![],
            }]),
        };
    }
    let new_schema_clone = new_schema.clone();

    // Create a Database from the parsed Schema
    let new_database = ast::Database {
        schemas: vec![new_schema],
    };

    // Typecheck the new schema
    let new_context = typecheck::check_schema(&new_database)?;

    // Get the recorded schema from introspection
    let (db_recorded_schema, _db_recorded_context) = match &introspection.schema {
        introspect::SchemaResult::FailedToParse { errors, .. } => {
            return Err(errors.clone());
        }
        introspect::SchemaResult::FailedToTypecheck { errors, .. } => {
            return Err(errors.clone());
        }
        introspect::SchemaResult::Success { schema, context } => (schema, context),
    };

    // Diff the schemas and check for errors
    let schema_diff = diff::diff_schema(&db_recorded_schema, &new_schema_clone);

    let errors = diff::to_errors(schema_diff);
    if !errors.is_empty() {
        return Err(errors);
    }

    // Generate the SQL from the diff
    let db_diff = db_diff::diff(&new_context, &new_schema_clone, &introspection);

    if db_diff::is_empty(&db_diff) {
        return Ok(MigrationSql {
            sql: vec![],
            mark_success: SqlAndParams::SqlWithParams {
                sql: INSERT_MIGRATION_SUCCESS_WITH_SCHEMA.to_string(),
                args: vec![
                    name.to_string(),
                    "".to_string(),
                    new_schema_source.to_string(),
                ],
            },
            mark_failure: SqlAndParams::SqlWithParams {
                sql: INSERT_MIGRATION_ERROR.to_string(),
                args: vec![name.to_string(), "".to_string()],
            },
        });
    }

    // Generate SQL statements from the diff
    let mut sql = db_diff::to_sql::to_sql(&db_diff);

    let sql_executed = String::new();

    sql.splice(0..0, internal_setup_sql());

    Ok(MigrationSql {
        sql,
        mark_success: SqlAndParams::SqlWithParams {
            sql: INSERT_MIGRATION_SUCCESS_WITH_SCHEMA.to_string(),
            args: vec![
                name.to_string(),
                sql_executed.clone(),
                new_schema_source.to_string(),
            ],
        },
        mark_failure: SqlAndParams::SqlWithParams {
            sql: INSERT_MIGRATION_ERROR.to_string(),
            args: vec![name.to_string(), sql_executed.clone()],
        },
    })
}

/// File-based migration approach - executes pre-written SQL migration files.
/// This is used in CLI environments where migration files are stored on disk.
///
/// Takes a list of migration files (name and SQL content) and executes them,
/// skipping migrations that have already been run.
///
/// The caller is responsible for:
/// - Reading migration files from disk
/// - Providing database connection
/// - Executing the SQL statements
///
/// This function provides the core logic for determining which migrations to run
/// and generating the SQL for recording migrations and schema.
pub struct FileBasedMigrationPlan {
    /// Migrations that should be executed (name, sql_content)
    pub migrations_to_run: Vec<(String, String)>,
    /// Schema string to insert
    pub schema_string: String,
}

/// Plan file-based migrations by determining which migration files need to be executed.
/// Returns a plan that can be executed by the caller using their database connection.
pub fn plan_file_based_migrations(
    migration_files: &[(String, String)],
    migration_state: &introspect::MigrationState,
    schema: &ast::Schema,
) -> FileBasedMigrationPlan {
    // Determine which migrations need to be run
    let migrations_to_run: Vec<(String, String)> = match migration_state {
        introspect::MigrationState::NoMigrationTable => {
            // Run all migrations if no migration table exists
            migration_files.to_vec()
        }
        introspect::MigrationState::MigrationTable { migrations } => {
            // Filter out migrations that have already been run
            migration_files
                .iter()
                .filter(|(name, _)| !migrations.iter().any(|m| m.name == *name))
                .cloned()
                .collect()
        }
    };

    // Generate schema string
    let schema_string = crate::generate::to_string::schema_to_string("", schema);

    FileBasedMigrationPlan {
        migrations_to_run,
        schema_string,
    }
}
