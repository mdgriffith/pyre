use std::io;
use std::path::{Path, PathBuf};

use super::shared::{write_schema, Options};
use crate::db;
use pyre::ast;

pub async fn introspect<'a>(
    options: &'a Options<'a>,
    database: &str,
    auth: &Option<String>,
    namespace: &Option<String>,
) -> io::Result<()> {
    let conn_result = db::connect(&database.to_string(), auth).await;
    match conn_result {
        Ok(conn) => {
            let full_namespace = namespace
                .clone()
                .unwrap_or(ast::DEFAULT_SCHEMANAME.to_string());

            let introspection_result = crate::db::introspect::introspect(&conn).await;
            match introspection_result {
                Ok(introspection) => {
                    let path: PathBuf = if full_namespace != ast::DEFAULT_SCHEMANAME {
                        Path::new(&options.in_dir)
                            .join("schema")
                            .join(&full_namespace)
                            .join("schema.pyre")
                    } else {
                        Path::new(&options.in_dir).join("schema.pyre")
                    };

                    if path.exists() {
                        println!(
                            "\nSchema already exists\n\n   {}",
                            pyre::color::yellow(options.enable_color, &path.display().to_string())
                        );
                        println!("\nRemove it if you want to generate a new one!");
                    } else {
                        println!("Schema written to {:?}", path.to_str());

                        if introspection.tables.is_empty() {
                            println!("I was able to successfully connect to the database, but I couldn't find any tables or views!");
                        } else {
                            let mut schema_file =
                                pyre::db::introspect::to_schema::to_schema(&introspection);
                            schema_file.path = path.to_string_lossy().to_string();

                            let schema = ast::Schema {
                                namespace: full_namespace,
                                sync_mode: ast::SyncMode::Synced,
                                session: None,
                                files: vec![schema_file],
                            };

                            write_schema(options, &false, &schema)?;
                        }
                    }
                }
                Err(libsql_error) => {
                    println!("{}", crate::db::error::format_libsql_error(&libsql_error));
                    return Err(io::Error::other("Failed to introspect database"));
                }
            }
        }
        Err(err) => {
            println!("{}", err.format_error());
            return Err(io::Error::other("Failed to connect to database"));
        }
    }
    Ok(())
}
