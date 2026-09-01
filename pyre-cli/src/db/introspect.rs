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

                                        let introspection_raw: Result<
                                            pyre::db::introspect::IntrospectionRaw,
                                            serde_json::Error,
                                        > = serde_json::from_str(&introspection.result);

                                        if let Ok(introspection_raw) = introspection_raw {
                                            return Ok(pyre::db::introspect::from_raw(
                                                introspection_raw,
                                            ));
                                        } else {
                                            // This is likely not correct
                                            return Ok(Introspection {
                                                tables: vec![],
                                                migration_state: MigrationState::NoMigrationTable,
                                                schema:
                                                    pyre::db::introspect::SchemaResult::Success {
                                                        schema: pyre::ast::Schema::default(),
                                                        context: pyre::typecheck::empty_context(),
                                                    },
                                            });
                                        }
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
                let introspection_raw: Result<pyre::db::introspect::IntrospectionRaw, _> =
                    serde_json::from_str(&introspection.result);

                if let Ok(introspection_raw) = introspection_raw {
                    return Ok(pyre::db::introspect::from_raw(introspection_raw));
                }
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
