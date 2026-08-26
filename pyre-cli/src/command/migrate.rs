use libsql;
use std::io;
use std::path::Path;

use super::shared::{check_namespace_requirements, parse_database_schemas, Options};
use crate::db;
use pyre::ast;
use pyre::ast::diff;
use pyre::error;
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::typecheck;

fn display_database_target(database: &str) -> String {
    let without_query = database
        .split(|character| character == '?' || character == '#')
        .next()
        .unwrap_or(database);

    match without_query.split_once("://") {
        Some((scheme, target)) => {
            let target_without_userinfo = target.rsplit_once('@').map_or(target, |(_, host)| host);
            format!("{}://{}", scheme, target_without_userinfo)
        }
        None => without_query.to_string(),
    }
}

fn report_stored_schema_errors(
    database: &str,
    namespace: &str,
    failure: &str,
    source: &str,
    errors: &[error::Error],
    enable_color: bool,
) -> ! {
    eprintln!(
        "Stored schema in database '{}' for namespace '{}' failed to {}:",
        display_database_target(database),
        namespace,
        failure
    );
    for schema_error in errors {
        eprintln!(
            "{}",
            error::format_error(source, &schema_error, enable_color)
        );
    }
    std::process::exit(1);
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
                            println!("{}", migration_error.format_error());
                            std::process::exit(1);
                        }
                    }
                }
                Err(err) => {
                    println!("{:?}", err);
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
                    println!("{:?}", err);
                }
                Ok(conn) => {
                    let introspection_result = crate::db::introspect::introspect(&conn).await;
                    match introspection_result {
                        Ok(introspection) => {
                            match &introspection.schema {
                                pyre::db::introspect::SchemaResult::Success {
                                    schema: db_recorded_schema,
                                    ..
                                } => {
                                    let schema_diff =
                                        diff::diff_schema(db_recorded_schema, &current_schema);

                                    // We diff the two schemas and report errors.

                                    let errors = diff::to_errors(schema_diff);
                                    if !errors.is_empty() {
                                        error::report_and_exit(
                                            errors,
                                            &paths,
                                            options.enable_color,
                                        );
                                    }

                                    // If there are no errors, we can now generate sql.

                                    let db_diff = pyre::db::diff::diff(
                                        &current_context,
                                        &current_schema,
                                        &introspection,
                                    );

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

                                    match conn.connect() {
                                        Ok(connected_conn) => {
                                            match connected_conn
                                                .transaction_with_behavior(
                                                    libsql::TransactionBehavior::Immediate,
                                                )
                                                .await
                                            {
                                                Ok(tx) => {
                                                    let mut has_error = false;
                                                    for sql_statement in sql {
                                                        match sql_statement {
                                                            SqlAndParams::Sql(sql_string) => {
                                                                if let Err(e) = tx
                                                                    .execute(
                                                                        &sql_string,
                                                                        libsql::params_from_iter::<
                                                                            Vec<libsql::Value>,
                                                                        >(
                                                                            vec![]
                                                                        ),
                                                                    )
                                                                    .await
                                                                {
                                                                    eprintln!(
                                                                        "Error executing SQL: {:?}",
                                                                        e
                                                                    );
                                                                    eprintln!(
                                                                        "SQL statement: {}",
                                                                        sql_string
                                                                    );
                                                                    has_error = true;
                                                                    break;
                                                                }
                                                            }
                                                            SqlAndParams::SqlWithParams {
                                                                sql,
                                                                args,
                                                            } => {
                                                                if let Err(e) = tx
                                                                    .execute(
                                                                        &sql,
                                                                        libsql::params_from_iter(
                                                                            args,
                                                                        ),
                                                                    )
                                                                    .await
                                                                {
                                                                    eprintln!(
                                                                        "Error executing SQL: {:?}",
                                                                        e
                                                                    );
                                                                    eprintln!(
                                                                        "SQL statement: {}",
                                                                        sql
                                                                    );
                                                                    has_error = true;
                                                                    break;
                                                                }
                                                            }
                                                        }
                                                    }

                                                    if has_error {
                                                        eprintln!("Migration failed due to SQL execution errors. Database may be in an inconsistent state.");
                                                        std::process::exit(1);
                                                    }

                                                    if let Err(e) = tx.commit().await {
                                                        eprintln!(
                                                            "Error committing transaction: {:?}",
                                                            e
                                                        );
                                                        eprintln!("Migration failed. Database may be in an inconsistent state.");
                                                        std::process::exit(1);
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!(
                                                        "Error creating transaction: {:?}",
                                                        e
                                                    );
                                                    eprintln!("Migration failed. Database may be in an inconsistent state.");
                                                    std::process::exit(1);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("Error connecting to database: {:?}", e);
                                            eprintln!("Migration failed. Could not establish database connection.");
                                            std::process::exit(1);
                                        }
                                    }
                                }
                                pyre::db::introspect::SchemaResult::FailedToParse {
                                    source,
                                    errors,
                                } => report_stored_schema_errors(
                                    database,
                                    real_namespace,
                                    "parse",
                                    source,
                                    errors,
                                    options.enable_color,
                                ),
                                pyre::db::introspect::SchemaResult::FailedToTypecheck {
                                    source,
                                    errors,
                                    ..
                                } => report_stored_schema_errors(
                                    database,
                                    real_namespace,
                                    "typecheck",
                                    source,
                                    errors,
                                    options.enable_color,
                                ),
                            }
                        }
                        Err(err) => {
                            println!("{:?}", err);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
