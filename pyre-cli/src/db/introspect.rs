use libsql;
use pyre::db::introspect::{
    DbTable, Introspection, Migration, MigrationRun, MigrationState, LIST_MIGRATIONS, LIST_TABLES,
    MIGRATION_TABLE,
};

pub async fn get_migration_state(
    conn: &libsql::Connection,
) -> Result<MigrationState, libsql::Error> {
    let args: Vec<String> = vec![];
    let table_list_result = conn.query(LIST_TABLES, args).await?;
    let mut has_migrations_table = false;

    let mut table_rows = table_list_result;
    while let Some(row) = table_rows.next().await? {
        let table = libsql::de::from_row::<DbTable>(&row).unwrap();
        if table.name == MIGRATION_TABLE {
            has_migrations_table = true;
            break;
        }
    }

    if !has_migrations_table {
        return Ok(MigrationState::NoMigrationTable);
    }

    let args: Vec<String> = vec![];
    let migration_list_result = conn.query(LIST_MIGRATIONS, args).await?;
    let mut migrations = Vec::new();

    let mut migration_rows = migration_list_result;
    while let Some(row) = migration_rows.next().await? {
        let migration_run = libsql::de::from_row::<MigrationRun>(&row).unwrap();
        migrations.push(Migration {
            name: migration_run.name,
        });
    }

    Ok(MigrationState::MigrationTable { migrations })
}

#[derive(serde::Deserialize)]
struct IntrospectionRow {
    result: String,
}

#[derive(Debug)]
struct IntrospectionDecodeError(serde_json::Error);

impl std::fmt::Display for IntrospectionDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Failed to decode database introspection JSON: {}",
            self.0
        )
    }
}

impl std::error::Error for IntrospectionDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn decode_introspection(source: &str) -> Result<Introspection, libsql::Error> {
    let raw = serde_json::from_str(source).map_err(|error| {
        libsql::Error::ToSqlConversionFailure(Box::new(IntrospectionDecodeError(error)))
    })?;
    Ok(pyre::db::introspect::from_raw(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn malformed_json_retains_typed_decode_error() {
        for source in ["not JSON", "{}"] {
            let error = decode_introspection(source).unwrap_err();
            assert!(error.to_string().contains("database introspection JSON"));
            let libsql::Error::ToSqlConversionFailure(source) = error else {
                panic!("expected conversion error");
            };
            let decode = source.downcast_ref::<IntrospectionDecodeError>().unwrap();
            assert!(decode.source().unwrap().is::<serde_json::Error>());
        }
    }

    #[tokio::test]
    async fn both_entrypoints_propagate_invalid_introspection_json() {
        let temp = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(temp.path().join("test.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE _pyre_migrations (id INTEGER, name, schema, finished_at, error);
             INSERT INTO _pyre_migrations (name) VALUES (NULL);",
        )
        .await
        .unwrap();

        for result in [introspect(&db).await, introspect_connection(&conn).await] {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("database introspection JSON"));
            assert!(error.to_string().contains("invalid type: null"));
            assert!(matches!(error, libsql::Error::ToSqlConversionFailure(_)));
        }

        conn.execute("UPDATE _pyre_migrations SET name = 'initial'", ())
            .await
            .unwrap();
        for result in [introspect(&db).await, introspect_connection(&conn).await] {
            let introspection = result.unwrap();
            let MigrationState::MigrationTable { migrations } = introspection.migration_state
            else {
                panic!("expected migration table");
            };
            assert_eq!(migrations[0].name, "initial");
        }
    }

    #[tokio::test]
    async fn database_query_errors_keep_their_libsql_variant() {
        let temp = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(temp.path().join("test.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE _pyre_migrations (name TEXT)", ())
            .await
            .unwrap();
        for result in [introspect(&db).await, introspect_connection(&conn).await] {
            assert!(matches!(result, Err(libsql::Error::SqliteFailure(_, _))));
        }
    }

    #[tokio::test]
    async fn uninitialized_database_remains_successful() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        for result in [introspect(&db).await, introspect_connection(&conn).await] {
            let introspection = result.unwrap();
            assert!(introspection.tables.is_empty());
            assert!(matches!(
                introspection.migration_state,
                MigrationState::NoMigrationTable
            ));
        }
    }
}

#[derive(serde::Deserialize)]
struct IsInitialized {
    #[serde(deserialize_with = "deserialize_bool_from_int")]
    is_initialized: bool,
}

fn deserialize_bool_from_int<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let i: i32 = serde::Deserialize::deserialize(deserializer)?;
    Ok(i == 1)
}

pub async fn introspect(db: &libsql::Database) -> Result<Introspection, libsql::Error> {
    match super::ProcessConnection::connect(db) {
        Err(e) => {
            println!("Error: {}", e);
            Err(e)
        }
        Ok(conn) => {
            let args: Vec<String> = vec![];
            let is_initialized_result =
                conn.query(pyre::db::introspect::IS_INITIALIZED, args).await;

            match is_initialized_result {
                Ok(mut is_initialized_rows) => {
                    if let Some(row) = is_initialized_rows.next().await? {
                        let is_initialized = libsql::de::from_row::<IsInitialized>(&row).unwrap();
                        if is_initialized.is_initialized {
                            let args: Vec<String> = vec![];
                            let introspection_result =
                                conn.query(pyre::db::introspect::INTROSPECT_SQL, args).await;

                            match introspection_result {
                                Ok(mut introspection_rows) => {
                                    if let Some(row) = introspection_rows.next().await? {
                                        let introspection =
                                            libsql::de::from_row::<IntrospectionRow>(&row).unwrap();

                                        return decode_introspection(&introspection.result);
                                    }
                                }
                                Err(e) => {
                                    println!("Error: {}", e);
                                    return Err(e);
                                }
                            }
                        }
                    }
                    // This is likely not correct
                    Ok(Introspection {
                        tables: vec![],
                        migration_state: MigrationState::NoMigrationTable,
                        schema: pyre::db::introspect::SchemaResult::Success {
                            schema: pyre::ast::Schema::default(),
                            context: pyre::typecheck::empty_context(),
                        },
                    })
                }
                Err(e) => {
                    println!("Error: {}", e);
                    Err(e)
                }
            }
        }
    }
}

pub async fn introspect_connection(
    conn: &libsql::Connection,
) -> Result<Introspection, libsql::Error> {
    let args: Vec<String> = vec![];
    let mut is_initialized_rows = conn
        .query(pyre::db::introspect::IS_INITIALIZED, args)
        .await?;

    if let Some(row) = is_initialized_rows.next().await? {
        let is_initialized = libsql::de::from_row::<IsInitialized>(&row).unwrap();
        if is_initialized.is_initialized {
            let args: Vec<String> = vec![];
            let mut introspection_rows = conn
                .query(pyre::db::introspect::INTROSPECT_SQL, args)
                .await?;

            if let Some(row) = introspection_rows.next().await? {
                let introspection = libsql::de::from_row::<IntrospectionRow>(&row).unwrap();
                return decode_introspection(&introspection.result);
            }
        }
    }

    Ok(Introspection {
        tables: vec![],
        migration_state: MigrationState::NoMigrationTable,
        schema: pyre::db::introspect::SchemaResult::Success {
            schema: pyre::ast::Schema::default(),
            context: pyre::typecheck::empty_context(),
        },
    })
}
