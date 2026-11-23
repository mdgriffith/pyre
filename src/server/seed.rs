use crate::fixture::{self, Insert, ReturnedRow, SqlValue};
use serde::de::{MapAccess, Visitor};
use serde::Serialize;
use serde_json::Value;

// Keep top-level insert order without changing how the rest of Pyre orders JSON maps.
struct FixtureEntries(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for FixtureEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;

        impl<'de> Visitor<'de> for EntriesVisitor {
            type Value = FixtureEntries;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a fixture object keyed by table name")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = Vec::new();
                let mut tables = std::collections::HashSet::new();
                while let Some((table, rows)) = map.next_entry::<String, Value>()? {
                    if !tables.insert(table.clone()) {
                        return Err(serde::de::Error::custom(format_args!(
                            "duplicate fixture table '{table}'"
                        )));
                    }
                    entries.push((table, rows));
                }
                Ok(FixtureEntries(entries))
            }
        }

        deserializer.deserialize_map(EntriesVisitor)
    }
}

#[derive(Debug)]
pub struct SeedResult {
    pub response: Value,
}

/// Insert schema-shaped fixture or import data without applying query permissions or sync metadata.
pub async fn seed<T: Serialize>(conn: &libsql::Connection, input: T) -> Result<SeedResult, Error> {
    seed_for_namespace(conn, None, input).await
}

/// Seed an initialized target, optionally requiring its installed namespace to match.
pub async fn seed_for_namespace<T: Serialize>(
    conn: &libsql::Connection,
    expected_namespace: Option<&str>,
    input: T,
) -> Result<SeedResult, Error> {
    let json = serde_json::to_vec(&input).map_err(Error::Json)?;
    let FixtureEntries(entries) =
        serde_json::from_slice(&json).map_err(|error| Error::InvalidInput(error.to_string()))?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(Error::Database)?;
    let result = async {
        let loaded = crate::server::schema::load_schema_from_database(&tx)
            .await
            .map_err(Error::Schema)?;
        let namespace = &loaded.schema().map_err(Error::Schema)?.namespace;
        if let Some(expected) = expected_namespace {
            if expected != namespace {
                return Err(Error::InvalidInput(format!(
                    "seed expected database namespace '{expected}', found '{namespace}'"
                )));
            }
        }
        let context = loaded.context().map_err(Error::Schema)?;
        let conn: &libsql::Connection = &tx;
        let trigger_tables = {
            let mut rows = conn
                .query(fixture::TRIGGER_TABLES_SQL, ())
                .await
                .map_err(Error::Database)?;
            rows.next()
                .await
                .map_err(Error::Database)?
                .ok_or_else(|| Error::Unsupported("could not read fixture trigger tables".into()))?
                .get::<String>(0)
                .map_err(Error::Database)?
        };
        let policies =
            fixture::InsertPolicies::from_trigger_tables(&trigger_tables).map_err(Error::Json)?;
        fixture::ingest_entries(context, namespace, entries, &policies, |insert| {
            execute(conn, insert)
        })
        .await
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
        SqlValue::Text(value) | SqlValue::Json(value) => libsql::Value::Text(value),
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
