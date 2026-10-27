use libsql;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use super::shared::{check_namespace_requirements, parse_database_schemas, Options};
use crate::db;
use pyre::ast;
use pyre::ast::diff;
use pyre::error;
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::typecheck;

fn confirm_table_removals(
    database: &str,
    namespace: &str,
    tables: &[String],
    force: bool,
) -> io::Result<()> {
    if tables.is_empty() || force {
        return Ok(());
    }
    let summary = format!(
        "{} · {}\n\nThe following tables will be dropped:\n{}\n\nThis is a destructive change!",
        namespace,
        db::error::database_location(database),
        tables
            .iter()
            .map(|name| format!("  - {name}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(io::Error::other(format!(
            "{summary}\n\nRerun with --force to apply this change without confirmation.\nNo migration SQL was executed."
        )));
    }
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{summary}\n")?;
    if !read_confirmation(&mut io::stdin().lock(), &mut stderr)? {
        return Err(io::Error::other(
            "Push cancelled. No migration SQL was executed.",
        ));
    }
    Ok(())
}

fn read_confirmation(reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<bool> {
    write!(writer, "Continue? [y/N] ")?;
    writer.flush()?;
    let mut response = String::new();
    reader.read_line(&mut response)?;
    Ok(matches!(
        response.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn report_stored_schema_errors(
    database: &str,
    namespace: &str,
    failure: &str,
    source: &str,
    errors: &[error::Error],
    enable_color: bool,
) -> io::Error {
    eprintln!(
        "Stored schema in database '{}' for namespace '{}' failed to {}:",
        db::error::database_location(database),
        namespace,
        failure
    );
    for schema_error in errors {
        eprintln!(
            "{}",
            error::format_error(source, &schema_error, enable_color)
        );
    }
    eprintln!("Correct the errors above in the complete project schemas, then retry.");
    io::Error::new(io::ErrorKind::InvalidData, "Stored schema is invalid")
}

pub async fn migrate<'a>(
    options: &'a Options<'a>,
    database: &str,
    auth: &Option<String>,
    migration_dir: &str,
    namespace: &Option<String>,
) -> io::Result<()> {
    check_namespace_requirements(&namespace, &options);
    let namespace_migration_dir = match namespace {
        Some(ns) => Path::new(migration_dir).join(ns),
        None => Path::new(migration_dir).to_path_buf(),
    };

    // Get schema
    let paths = crate::filesystem::collect_filepaths(&options.in_dir)?;
    let all_schemas = parse_database_schemas(&paths, options.enable_color)?;

    let real_namespace = match namespace {
        Some(ns) => ns,
        None => &ast::DEFAULT_SCHEMANAME.to_string(),
    };

    // Get exactly one schema based on namespace or default
    let schema = match all_schemas
        .schemas
        .iter()
        .find(|schema| schema.namespace == *real_namespace)
    {
        Some(s) => s,
        None => {
            eprintln!("Error: No schema found for namespace '{}'", real_namespace);
            std::process::exit(1);
        }
    };

    // Typecheck schemas

    match typecheck::check_schema(&all_schemas) {
        Err(error_list) => {
            error::report_and_exit(error_list, &paths, options.enable_color);
        }
        Ok(context) => {
            let connection_result = db::connect(&database.to_string(), auth).await;
            match connection_result {
                Ok(conn) => {
                    let migration_result = db::migrate(
                        &conn,
                        db::MigrateOptions {
                            schema,
                            context: &context,
                            migration_folder: &namespace_migration_dir,
                            migration_root: Path::new(migration_dir),
                            namespace: namespace.as_deref(),
                            db_path: database,
                        },
                    )
                    .await;
                    match migration_result {
                        Ok(outcome) => {
                            println!("{}", outcome.status_line());
                        }
                        Err(migration_error) => {
                            return Err(io::Error::other(format!(
                                "{}\n\n{}",
                                db::error::context(database, real_namespace),
                                migration_error.format_error()
                            )));
                        }
                    }
                }
                Err(err) => {
                    return Err(io::Error::other(err.format_error()));
                }
            }
        }
    }
    Ok(())
}

/**
 * This is the new "dynamic" migration approach
 *
 *
 *
 */
pub async fn push<'a>(
    options: &'a Options<'a>,
    database: &str,
    auth: &Option<String>,

    namespace: &Option<String>,
    force: bool,
) -> io::Result<()> {
    check_namespace_requirements(&namespace, &options);

    // Get schema
    let paths = crate::filesystem::collect_filepaths(&options.in_dir)?;
    let all_schemas = parse_database_schemas(&paths, options.enable_color)?;

    let real_namespace = match namespace {
        Some(ns) => ns,
        None => &ast::DEFAULT_SCHEMANAME.to_string(),
    };

    // Get exactly one schema based on namespace or default
    let current_schema = match all_schemas
        .schemas
        .iter()
        .find(|schema| schema.namespace == *real_namespace)
    {
        Some(s) => s,
        None => {
            eprintln!("Error: No schema found for namespace '{}'", real_namespace);
            std::process::exit(1);
        }
    };

    // Typecheck schemas

    match typecheck::check_schema(&all_schemas) {
        Err(error_list) => {
            error::report_and_exit(error_list, &paths, options.enable_color);
        }
        Ok(current_context) => {
            let connection_result = db::connect(&database.to_string(), auth).await;
            match connection_result {
                Err(err) => {
                    return Err(io::Error::other(err.format_error()));
                }
                Ok(conn) => {
                    let failure = |operation: &str, error: &libsql::Error| {
                        io::Error::other(db::error::operation_error(
                            database,
                            real_namespace,
                            operation,
                            error,
                        ))
                    };
                    let connected_conn =
                        db::ProcessConnection::connect(&conn).map_err(|error| {
                            failure("Could not open the database connection.", &error)
                        })?;
                    let tx = connected_conn
                        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                        .await
                        .map_err(|error| failure("Could not start the migration transaction. No migration SQL was executed.", &error))?;
                    let introspection_result = crate::db::introspect::introspect_for_project(
                        &tx,
                        &current_context,
                        real_namespace,
                    )
                    .await;
                    match introspection_result {
                        Ok(introspection) => {
                            match &introspection.schema {
                                pyre::db::introspect::SchemaResult::Success {
                                    schema: db_recorded_schema,
                                    ..
                                } => {
                                    let schema_diff = pyre::db::migrate::diff_stored_schema(
                                        &current_context,
                                        db_recorded_schema,
                                        current_schema,
                                    );

                                    // We diff the two schemas and report errors.

                                    // Table removals are supported by the SQL generator. Confirm
                                    // the physical table names below; retain unsupported changes.
                                    let errors = diff::to_errors(schema_diff)
                                        .into_iter()
                                        .filter(|error| {
                                            !matches!(
                                                error.error_type,
                                                error::ErrorType::MigrationTableDropped { .. }
                                            )
                                        })
                                        .collect::<Vec<_>>();
                                    if !errors.is_empty() {
                                        let messages = errors
                                            .iter()
                                            .map(|error| {
                                                error::format_error("", error, options.enable_color)
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n\n");
                                        return Err(io::Error::other(format!(
                                            "{}\n\n{messages}\n\nNo migration SQL was executed.",
                                            db::error::context(database, real_namespace)
                                        )));
                                    }

                                    // If there are no errors, we can now generate sql.

                                    let db_diff = pyre::db::diff::diff(
                                        &current_context,
                                        &current_schema,
                                        &introspection,
                                    );

                                    // Physical drift can require a rebuild even when the
                                    // recorded and authored schemas agree.
                                    let unsupported = db_diff.modified_records.iter().flat_map(|record| {
                                        record.changes.iter().filter_map(|change| match change {
                                            pyre::db::diff::RecordChange::ModifiedField { name, changes } => Some(db::column_change_description(&record.name, name, changes)),
                                            _ => None,
                                        })
                                    }).collect::<Vec<_>>();
                                    if !unsupported.is_empty() {
                                        return Err(io::Error::other(format!("{}\n\nThese column changes require a table-rebuild SQL migration:\n  - {}\n\nNo migration SQL was executed.", db::error::context(database, real_namespace), unsupported.join("\n  - "))));
                                    }

                                    let mut removed_tables = db_diff
                                        .removed
                                        .iter()
                                        .map(|table| table.name.clone())
                                        .collect::<Vec<_>>();
                                    removed_tables.sort();
                                    confirm_table_removals(
                                        database,
                                        real_namespace,
                                        &removed_tables,
                                        force,
                                    )?;

                                    // Generate sql
                                    let mut sql = pyre::db::diff::to_sql::to_sql(&db_diff);

                                    sql.splice(0..0, pyre::db::migrate::internal_setup_sql());

                                    let schema_source = pyre::db::migrate::schema_to_storage_string(
                                        &current_context,
                                        current_schema,
                                    );
                                    sql.push(SqlAndParams::SqlWithParams {
                                        sql:
                                            pyre::db::migrate::INSERT_MIGRATION_SUCCESS_WITH_SCHEMA
                                                .to_string(),
                                        args: vec![
                                            "push".to_string(),
                                            "".to_string(),
                                            schema_source,
                                        ],
                                    });
                                    let (_, identities) =
                                        pyre::generate::to_string::standalone_schema(
                                            &current_context,
                                            current_schema,
                                        );
                                    sql.extend(pyre::db::migrate::external_identity_sql(
                                        &identities,
                                    ));
                                    sql.push(pyre::db::migrate::record_namespace_sql(
                                        real_namespace,
                                    ));

                                    if let Err(error) =
                                        pyre::db::migrate::ensure_metadata_storage(&tx).await
                                    {
                                        return Err(rollback_failure(
                                            tx,
                                            db::error::operation_error(
                                                database,
                                                real_namespace,
                                                "Could not prepare Pyre's migration metadata.",
                                                &error,
                                            ),
                                        )
                                        .await);
                                    }
                                    for (index, statement) in sql.into_iter().enumerate() {
                                        let sql_text = match &statement {
                                            SqlAndParams::Sql(sql)
                                            | SqlAndParams::SqlWithParams { sql, .. } => {
                                                sql.clone()
                                            }
                                        };
                                        let result = match statement {
                                            SqlAndParams::Sql(sql) => tx.execute(&sql, ()).await,
                                            SqlAndParams::SqlWithParams { sql, args } => {
                                                tx.execute(&sql, libsql::params_from_iter(args))
                                                    .await
                                            }
                                        };
                                        if let Err(error) = result {
                                            let message = db::error::operation_error(database, real_namespace, &format!("Could not apply schema change (SQL statement {}).\nSQL: {sql_text}", index + 1), &error);
                                            return Err(rollback_failure(tx, message).await);
                                        }
                                    }
                                    let updated = match crate::db::introspect::introspect_for_project(
                                        &tx,
                                        &current_context,
                                        real_namespace,
                                    )
                                    .await {
                                        Ok(updated) => updated,
                                        Err(error) => return Err(rollback_failure(tx, db::error::operation_error(database, real_namespace, "Could not verify the schema after applying changes.", &error)).await),
                                    };
                                    let remaining = pyre::db::diff::diff(
                                        &current_context,
                                        current_schema,
                                        &updated,
                                    );
                                    if !pyre::db::diff::is_empty(&remaining) {
                                        let changes = db::migration_validation_changes(&remaining)
                                            .join("\n  - ");
                                        let message = format!("{}\n\nThe database still differs from your schema:\n  - {changes}\n\nApply an explicit SQL migration for these changes.", db::error::context(database, real_namespace));
                                        return Err(rollback_failure(tx, message).await);
                                    }
                                    tx.commit().await.map_err(|error| failure("Could not confirm that the migration committed. The transaction outcome is unknown; check the database's migration state before retrying.", &error))?;
                                    println!("Schema pushed successfully.");
                                }
                                pyre::db::introspect::SchemaResult::FailedToParse {
                                    source,
                                    errors,
                                } => {
                                    return Err(report_stored_schema_errors(
                                        database,
                                        real_namespace,
                                        "parse",
                                        source,
                                        errors,
                                        options.enable_color,
                                    ))
                                }
                                pyre::db::introspect::SchemaResult::FailedToTypecheck {
                                    source,
                                    errors,
                                    ..
                                } => {
                                    return Err(report_stored_schema_errors(
                                        database,
                                        real_namespace,
                                        "typecheck",
                                        source,
                                        errors,
                                        options.enable_color,
                                    ))
                                }
                            }
                        }
                        Err(err) => {
                            return Err(failure(
                                "Failed to introspect database before applying schema changes.",
                                &err,
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

async fn rollback_failure(tx: libsql::Transaction, message: String) -> io::Error {
    let outcome = match tx.rollback().await {
        Ok(()) => "Changes from this push were rolled back.".to_string(),
        Err(error) => format!(
            "Could not confirm rollback: {}\nCheck the database's migration state before retrying.",
            db::error::format_libsql_error(&error)
        ),
    };
    io::Error::other(format!("{message}\n\n{outcome}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_defaults_to_no_and_requires_an_affirmative_answer() {
        for (input, expected) in [
            ("", false),
            ("\n", false),
            ("no\n", false),
            ("maybe\n", false),
            ("y\n", true),
            ("YES\n", true),
        ] {
            assert_eq!(
                read_confirmation(&mut input.as_bytes(), &mut Vec::new()).unwrap(),
                expected
            );
        }
    }
}
