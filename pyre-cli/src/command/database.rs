use super::shared::{check_namespace_requirements, Options};
use crate::db;
use pyre::server::seed;
use std::io;
use std::path::Path;

pub async fn seed_database(
    options: &Options<'_>,
    database: &str,
    auth: &Option<String>,
    namespace: &Option<String>,
    fixtures: &Path,
) -> io::Result<()> {
    check_namespace_requirements(namespace, options);
    let namespace = namespace
        .as_deref()
        .unwrap_or(pyre::ast::DEFAULT_SCHEMANAME);
    let input = read_fixtures(fixtures)?;
    let db = db::connect_existing(database, auth)
        .await
        .map_err(|err| io::Error::other(err.format_error()))?;
    let conn = db::ProcessConnection::connect(&db)
        .map_err(|err| io::Error::other(db::error::format_libsql_error(&err)))?;
    let result = seed::seed_for_namespace(&conn, Some(namespace), input)
        .await
        .map_err(|err| {
            io::Error::other(format!(
                "{}\n\n{err}",
                db::error::context(database, namespace)
            ))
        })?;
    println!("{}", result.response);
    Ok(())
}

fn read_fixtures(path: &Path) -> io::Result<serde_json::Value> {
    let file = std::fs::File::open(path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("Could not read fixtures {}: {err}", path.display()),
        )
    })?;
    serde_json::from_reader(io::BufReader::new(file)).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid fixture JSON in {}: {err}", path.display()),
        )
    })
}
