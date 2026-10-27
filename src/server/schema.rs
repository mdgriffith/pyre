use crate::ast;
use crate::db::introspect;
use crate::db::migrate;
use crate::generate::sql::to_sql::SqlAndParams;
use crate::typecheck;

pub struct LoadedSchema {
    introspection: introspect::Introspection,
}

impl LoadedSchema {
    pub fn context(&self) -> Result<&typecheck::Context, Error> {
        context_from_introspection(&self.introspection)
    }

    pub fn schema(&self) -> Result<&ast::Schema, Error> {
        schema_from_introspection(&self.introspection)
    }

    pub fn introspection(&self) -> &introspect::Introspection {
        &self.introspection
    }
}

/// Load and typecheck the Pyre schema stored in a migrated database.
pub async fn load_schema_from_database(conn: &libsql::Connection) -> Result<LoadedSchema, Error> {
    if conn.is_autocommit() {
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Deferred)
            .await
            .map_err(Error::Database)?;
        let loaded = load_schema_in_transaction(&tx).await?;
        tx.commit().await.map_err(Error::Database)?;
        Ok(loaded)
    } else {
        load_schema_in_transaction(conn).await
    }
}

async fn load_schema_in_transaction(conn: &libsql::Connection) -> Result<LoadedSchema, Error> {
    let is_initialized = is_initialized(conn).await?;
    let sql = if is_initialized {
        introspect::INTROSPECT_SQL
    } else {
        introspect::INTROSPECT_UNINITIALIZED_SQL
    };
    let raw = query_introspection(conn, sql).await?;
    let introspection = introspect::from_raw(raw);
    context_from_introspection(&introspection)?;

    Ok(LoadedSchema { introspection })
}

pub async fn load_context_from_database(conn: &libsql::Connection) -> Result<LoadedSchema, Error> {
    load_schema_from_database(conn).await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnsureDatabaseOutcome {
    Created,
    Migrated,
    UpToDate,
}

/// Create or migrate a database to the supplied generated schema.
pub async fn ensure_database(
    conn: &libsql::Connection,
    schema_name: &str,
    schema_source: &str,
) -> Result<EnsureDatabaseOutcome, EnsureDatabaseError> {
    ensure_database_with_external_identities(conn, schema_name, schema_source, &[]).await
}

pub async fn ensure_database_with_external_identities(
    conn: &libsql::Connection,
    schema_name: &str,
    schema_source: &str,
    identities: &[ast::ExternalIdentity],
) -> Result<EnsureDatabaseOutcome, EnsureDatabaseError> {
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(EnsureDatabaseError::Database)?;
    let initialized = is_initialized(&tx)
        .await
        .map_err(EnsureDatabaseError::Introspection)?;
    let (introspection, stored_identities, same_namespace) =
        introspect_connection(&tx, initialized, schema_name, identities)
            .await
            .map_err(EnsureDatabaseError::Introspection)?;

    if !initialized && !introspection.tables.is_empty() {
        return Err(EnsureDatabaseError::UnmanagedDatabase);
    }

    let migration_name = format!("ensure:{schema_name}");
    let plan = migrate::migrate_dynamic_with_external_identities(
        migration_name,
        &introspection,
        schema_source,
        "generated-schema.pyre",
        schema_name,
        identities,
    )
    .map_err(EnsureDatabaseError::Migration)?;
    let same_metadata = same_namespace
        && stored_identities.len() == plan.external_identities.len()
        && plan
            .external_identities
            .iter()
            .all(|identity| stored_identities.contains(identity));

    let recorded_schema = if initialized {
        Some(query_recorded_schema(&tx).await?)
    } else {
        None
    };
    if initialized
        && plan.sql.is_empty()
        && recorded_schema.as_deref() == Some(schema_source)
        && same_metadata
    {
        tx.rollback().await.map_err(EnsureDatabaseError::Database)?;
        return Ok(EnsureDatabaseOutcome::UpToDate);
    }

    migrate::ensure_metadata_storage(&tx)
        .await
        .map_err(EnsureDatabaseError::Database)?;
    if !initialized && plan.sql.is_empty() {
        for statement in migrate::internal_setup_sql() {
            execute_statement(&tx, statement).await?;
        }
    }
    for statement in plan.sql {
        execute_statement(&tx, statement).await?;
    }
    execute_statement(&tx, plan.mark_success).await?;
    execute_statement(&tx, migrate::record_namespace_sql(schema_name)).await?;
    for statement in migrate::external_identity_sql(&plan.external_identities) {
        execute_statement(&tx, statement).await?;
    }
    tx.commit().await.map_err(EnsureDatabaseError::Database)?;

    Ok(if initialized {
        EnsureDatabaseOutcome::Migrated
    } else {
        EnsureDatabaseOutcome::Created
    })
}

async fn query_recorded_schema(conn: &libsql::Connection) -> Result<String, EnsureDatabaseError> {
    let mut rows = conn
        .query(introspect::GET_SCHEMA, ())
        .await
        .map_err(EnsureDatabaseError::Database)?;
    let row = rows
        .next()
        .await
        .map_err(EnsureDatabaseError::Database)?
        .ok_or_else(|| {
            EnsureDatabaseError::Introspection(Error::InvalidIntrospection(
                "initialized database has no recorded schema".to_string(),
            ))
        })?;
    row.get::<String>(0).map_err(EnsureDatabaseError::Database)
}

async fn introspect_connection(
    conn: &libsql::Connection,
    initialized: bool,
    namespace: &str,
    identities: &[ast::ExternalIdentity],
) -> Result<(introspect::Introspection, Vec<ast::ExternalIdentity>, bool), Error> {
    let sql = if initialized {
        introspect::INTROSPECT_SQL
    } else {
        introspect::INTROSPECT_UNINITIALIZED_SQL
    };
    let raw = query_introspection(conn, sql).await?;
    let same_namespace = raw.schema_namespace.as_deref() == Some(namespace);
    let stored_identities = raw.external_identities.clone();
    Ok((
        introspect::from_raw_with_project(raw, namespace, identities),
        stored_identities,
        same_namespace,
    ))
}

async fn execute_statement(
    conn: &libsql::Connection,
    statement: SqlAndParams,
) -> Result<(), EnsureDatabaseError> {
    match statement {
        SqlAndParams::Sql(sql) => conn.execute(&sql, ()).await,
        SqlAndParams::SqlWithParams { sql, args } => {
            conn.execute(&sql, libsql::params_from_iter(args)).await
        }
    }
    .map(|_| ())
    .map_err(EnsureDatabaseError::Database)
}

async fn is_initialized(conn: &libsql::Connection) -> Result<bool, Error> {
    let mut rows = conn
        .query(introspect::IS_INITIALIZED, ())
        .await
        .map_err(Error::Database)?;
    let row = rows.next().await.map_err(Error::Database)?.ok_or({
        Error::InvalidIntrospection("database initialization query returned no rows".to_string())
    })?;
    let value = row.get::<i64>(0).map_err(Error::Database)?;

    Ok(value == 1)
}

async fn query_introspection(
    conn: &libsql::Connection,
    sql: &str,
) -> Result<introspect::IntrospectionRaw, Error> {
    let mut rows = conn.query(sql, ()).await.map_err(Error::Database)?;
    let row = rows.next().await.map_err(Error::Database)?.ok_or({
        Error::InvalidIntrospection("introspection query returned no rows".to_string())
    })?;
    let raw = row.get::<String>(0).map_err(Error::Database)?;

    let mut raw = serde_json::from_str(&raw).map_err(Error::Json)?;
    introspect::load_metadata(conn, &mut raw)
        .await
        .map_err(Error::Database)?;
    Ok(raw)
}

fn context_from_introspection(
    introspection: &introspect::Introspection,
) -> Result<&typecheck::Context, Error> {
    match &introspection.schema {
        introspect::SchemaResult::Success { context, schema } => {
            if context.tables.is_empty() {
                return Err(Error::MissingSchema {
                    schema: schema.namespace.clone(),
                });
            }

            Ok(context)
        }
        introspect::SchemaResult::FailedToParse { source, errors } => Err(Error::SchemaParse {
            source: source.clone(),
            errors: errors.clone(),
        }),
        introspect::SchemaResult::FailedToTypecheck { source, errors, .. } => {
            Err(Error::SchemaTypecheck {
                source: source.clone(),
                errors: errors.clone(),
            })
        }
    }
}

fn schema_from_introspection(
    introspection: &introspect::Introspection,
) -> Result<&ast::Schema, Error> {
    match &introspection.schema {
        introspect::SchemaResult::Success { schema, .. } => Ok(schema),
        introspect::SchemaResult::FailedToParse { source, errors } => Err(Error::SchemaParse {
            source: source.clone(),
            errors: errors.clone(),
        }),
        introspect::SchemaResult::FailedToTypecheck { source, errors, .. } => {
            Err(Error::SchemaTypecheck {
                source: source.clone(),
                errors: errors.clone(),
            })
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Database(libsql::Error),
    InvalidIntrospection(String),
    Json(serde_json::Error),
    MissingSchema {
        schema: String,
    },
    SchemaParse {
        source: String,
        errors: Vec<crate::error::Error>,
    },
    SchemaTypecheck {
        source: String,
        errors: Vec<crate::error::Error>,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(error) => write!(f, "Could not read the recorded schema: {}", crate::db::diagnostic::database_error(error)),
            Error::InvalidIntrospection(message) => write!(f, "invalid introspection: {}", message),
            Error::Json(error) => write!(f, "json error: {}", error),
            Error::MissingSchema { schema } => write!(f, "No recorded schema found for {schema}. Check the database location and schema name; for a new database, apply its initial migration."),
            Error::SchemaParse { errors, source } => {
                writeln!(f, "The recorded schema could not be parsed:")?;
                for error in errors {
                    write!(f, "\n{}", crate::error::format_error(source, error, false))?;
                }
                Ok(())
            }
            Error::SchemaTypecheck { source, errors } => {
                writeln!(f, "The recorded schema failed validation:")?;
                for error in errors {
                    write!(f, "\n{}", crate::error::format_error(source, error, false))?;
                }
                write!(f, "\nCorrect these errors in the complete project schemas, then run `pyre migrate`.")
            }
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Attach the database location at the boundary that owns the connection.
    pub fn with_database(&self, database: &str) -> String {
        let location = crate::db::diagnostic::database_location(database);
        match self {
            Self::MissingSchema { schema } => {
                format!("Schema: {schema}\nDatabase: {location}\n\n{self}")
            }
            _ => format!("Database: {location}\n\n{self}"),
        }
    }
}

#[derive(Debug)]
pub enum EnsureDatabaseError {
    Database(libsql::Error),
    Introspection(Error),
    Migration(Vec<crate::error::Error>),
    UnmanagedDatabase,
}

impl std::fmt::Display for EnsureDatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(
                f,
                "Could not initialize or migrate the database: {}",
                crate::db::diagnostic::database_error(error)
            ),
            Self::Introspection(error) => write!(f, "{error}"),
            Self::Migration(errors) => {
                writeln!(f, "Cannot automatically apply these schema changes:")?;
                for error in errors {
                    writeln!(f, "\n{}", crate::error::to_error_description(error, false))?;
                }
                Ok(())
            }
            Self::UnmanagedDatabase => write!(
                f,
                "refusing to initialize a non-empty database that is not managed by Pyre"
            ),
        }
    }
}

impl std::error::Error for EnsureDatabaseError {}
