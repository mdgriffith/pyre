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
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(EnsureDatabaseError::Database)?;
    let initialized = is_initialized(&tx)
        .await
        .map_err(EnsureDatabaseError::Introspection)?;
    let introspection = introspect_connection(&tx, initialized)
        .await
        .map_err(EnsureDatabaseError::Introspection)?;

    if !initialized && !introspection.tables.is_empty() {
        return Err(EnsureDatabaseError::UnmanagedDatabase);
    }

    let migration_name = format!("ensure:{schema_name}");
    let plan = migrate::migrate_dynamic_for_schema(
        migration_name,
        &introspection,
        schema_source,
        "generated-schema.pyre",
        schema_name,
    )
    .map_err(EnsureDatabaseError::Migration)?;

    let recorded_schema = if initialized {
        Some(query_recorded_schema(&tx).await?)
    } else {
        None
    };
    if initialized && plan.sql.is_empty() && recorded_schema.as_deref() == Some(schema_source) {
        tx.rollback().await.map_err(EnsureDatabaseError::Database)?;
        return Ok(EnsureDatabaseOutcome::UpToDate);
    }

    if !initialized && plan.sql.is_empty() {
        for statement in migrate::internal_setup_sql() {
            execute_statement(&tx, statement).await?;
        }
    }
    for statement in plan.sql {
        execute_statement(&tx, statement).await?;
    }
    execute_statement(&tx, plan.mark_success).await?;
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
) -> Result<introspect::Introspection, Error> {
    let sql = if initialized {
        introspect::INTROSPECT_SQL
    } else {
        introspect::INTROSPECT_UNINITIALIZED_SQL
    };
    let raw = query_introspection(conn, sql).await?;
    Ok(introspect::from_raw(raw))
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

    serde_json::from_str(&raw).map_err(Error::Json)
}

fn context_from_introspection(
    introspection: &introspect::Introspection,
) -> Result<&typecheck::Context, Error> {
    match &introspection.schema {
        introspect::SchemaResult::Success { context, .. } => {
            if context.tables.is_empty() {
                return Err(Error::MissingSchema);
            }

            Ok(context)
        }
        introspect::SchemaResult::FailedToParse { source, errors } => Err(Error::SchemaParse {
            source: source.clone(),
            errors: errors.clone(),
        }),
        introspect::SchemaResult::FailedToTypecheck { schema: _, errors } => {
            Err(Error::SchemaTypecheck {
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
        introspect::SchemaResult::FailedToTypecheck { schema: _, errors } => {
            Err(Error::SchemaTypecheck {
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
    MissingSchema,
    SchemaParse {
        source: String,
        errors: Vec<crate::error::Error>,
    },
    SchemaTypecheck {
        errors: Vec<crate::error::Error>,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(error) => write!(f, "database error: {}", error),
            Error::InvalidIntrospection(message) => write!(f, "invalid introspection: {}", message),
            Error::Json(error) => write!(f, "json error: {}", error),
            Error::MissingSchema => write!(f, "database does not contain a Pyre schema"),
            Error::SchemaParse { errors, .. } => {
                write!(f, "schema failed to parse with {} error(s)", errors.len())
            }
            Error::SchemaTypecheck { errors } => {
                write!(
                    f,
                    "schema failed to typecheck with {} error(s)",
                    errors.len()
                )
            }
        }
    }
}

impl std::error::Error for Error {}

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
            Self::Database(error) => write!(f, "database error: {error}"),
            Self::Introspection(error) => write!(f, "{error}"),
            Self::Migration(errors) => {
                write!(f, "schema migration failed with {} error(s)", errors.len())
            }
            Self::UnmanagedDatabase => write!(
                f,
                "refusing to initialize a non-empty database that is not managed by Pyre"
            ),
        }
    }
}

impl std::error::Error for EnsureDatabaseError {}
