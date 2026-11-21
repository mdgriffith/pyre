use crate::fixture::{self, Insert, ReturnedRow, SqlValue};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug)]
pub struct SeedResult {
    pub response: Value,
}

/// Insert schema-shaped fixture or import data without applying query permissions or sync metadata.
pub async fn seed<T: Serialize>(conn: &libsql::Connection, input: T) -> Result<SeedResult, Error> {
    let input = serde_json::to_value(input).map_err(Error::Json)?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(Error::Database)?;
    let result = async {
        let loaded = crate::server::schema::load_schema_from_database(&tx)
            .await
            .map_err(Error::Schema)?;
        let namespace = &loaded.schema().map_err(Error::Schema)?.namespace;
        let context = loaded.context().map_err(Error::Schema)?;
        let conn: &libsql::Connection = &tx;
        fixture::ingest(context, namespace, input, |insert| execute(conn, insert)).await
    }
    .await;

    match result {
        Ok(response) => {
            tx.commit().await.map_err(Error::CommitUnknown)?;
            Ok(SeedResult { response })
        }
        Err(cause) => match tx.rollback().await {
            Ok(()) => Err(cause),
            Err(rollback) => Err(Error::RollbackUnknown {
                cause: Box::new(cause),
                rollback,
            }),
        },
    }
}

async fn execute(conn: &libsql::Connection, insert: Insert) -> Result<ReturnedRow, Error> {
    let Insert { sql, args, path } = insert;
    let at_path = |error| Error::DatabaseAt {
        path: path.clone(),
        error,
    };
    let args = args.into_iter().map(|value| match value {
        SqlValue::Null => libsql::Value::Null,
        SqlValue::Integer(value) => libsql::Value::Integer(value),
        SqlValue::Real(value) => libsql::Value::Real(value),
        SqlValue::Text(value) => libsql::Value::Text(value),
        SqlValue::Blob(value) => libsql::Value::Blob(value),
    });
    let mut rows = conn
        .query(&sql, libsql::params_from_iter(args))
        .await
        .map_err(at_path)?;
    let row = rows
        .next()
        .await
        .map_err(at_path)?
        .ok_or_else(|| Error::InvalidInput(format!("{path}: insert returned no row")))?;
    let mut result = ReturnedRow::new();
    for index in 0..rows.column_count() {
        let column = rows.column_name(index).unwrap_or_default().to_owned();
        let value = match row.get::<libsql::Value>(index).map_err(at_path)? {
            libsql::Value::Null => Value::Null,
            libsql::Value::Integer(value) => Value::from(value),
            libsql::Value::Real(value) => Value::from(value),
            libsql::Value::Text(value) => Value::String(value),
            libsql::Value::Blob(bytes) => Value::from(bytes),
        };
        result.insert(column, value);
    }
    Ok(result)
}

#[derive(Debug)]
pub enum Error {
    Database(libsql::Error),
    DatabaseAt {
        path: String,
        error: libsql::Error,
    },
    InvalidInput(String),
    Json(serde_json::Error),
    Schema(crate::server::schema::Error),
    Unsupported(String),
    CommitUnknown(libsql::Error),
    RollbackUnknown {
        cause: Box<Error>,
        rollback: libsql::Error,
    },
}

impl From<fixture::Error> for Error {
    fn from(error: fixture::Error) -> Self {
        match error {
            fixture::Error::InvalidInput(message) => Self::InvalidInput(message),
            fixture::Error::Json(error) => Self::Json(error),
            fixture::Error::Unsupported(message) => Self::Unsupported(message),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::db::diagnostic::database_error;
        match self {
            Self::Database(error) => {
                write!(f, "Could not seed the database: {}", database_error(error))
            }
            Self::DatabaseAt { path, error } => {
                write!(f, "Could not seed {path}: {}", database_error(error))
            }
            Self::InvalidInput(message) => write!(f, "invalid seed input: {message}"),
            Self::Json(error) => write!(f, "json error: {error}"),
            Self::Schema(error) => write!(f, "schema error: {error}"),
            Self::Unsupported(message) => write!(f, "unsupported seed input: {message}"),
            Self::CommitUnknown(error) => {
                write!(f, "seed commit outcome unknown: {}", database_error(error))
            }
            Self::RollbackUnknown { cause, rollback } => write!(
                f,
                "{cause}; seed rollback outcome unknown: {}",
                database_error(rollback)
            ),
        }
    }
}

impl std::error::Error for Error {}
