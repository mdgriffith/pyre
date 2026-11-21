use super::shared::{check_namespace_requirements, parse_database_schemas, Options};
use crate::db;
use crate::DatabasePolicyArg;
use pyre::db::policy;
use pyre::server::seed;
use std::io;
use std::path::Path;

pub(crate) async fn prepare_disposable_database(
    options: &Options<'_>,
    database: &str,
    auth: &Option<String>,
    namespace: &Option<String>,
    reset: bool,
) -> io::Result<()> {
    check_namespace_requirements(namespace, options);
    let namespace = namespace
        .as_deref()
        .unwrap_or(pyre::ast::DEFAULT_SCHEMANAME);
    let paths = crate::filesystem::collect_filepaths(options.in_dir)?;
    let schemas = parse_database_schemas(&paths, options.enable_color)?;
    let schema = schemas
        .schemas
        .iter()
        .find(|schema| schema.namespace == namespace)
        .ok_or_else(|| io::Error::other(format!("No schema found for namespace '{namespace}'")))?;
    let context = match pyre::typecheck::check_schema(&schemas) {
        Ok(context) => context,
        Err(errors) => pyre::error::report_and_exit(errors, &paths, options.enable_color),
    };
    let (source, identities) = pyre::generate::to_string::standalone_schema(&context, schema);
    let db = if reset {
        db::connect_existing(database, auth).await
    } else {
        db::connect(database, auth).await
    }
    .map_err(|err| io::Error::other(err.format_error()))?;
    let conn = db::ProcessConnection::connect(&db)
        .map_err(|err| io::Error::other(db::error::format_libsql_error(&err)))?;
    let result = if reset {
        pyre::db::lifecycle::reset(&conn, namespace, &source, &identities).await
    } else {
        pyre::db::lifecycle::init_disposable(&conn, namespace, &source, &identities).await
    };
    result.map_err(|err| {
        io::Error::other(format!(
            "{}\n\n{err}",
            db::error::context(database, namespace)
        ))
    })
}

pub(crate) async fn database_policy(
    options: &Options<'_>,
    database: &str,
    auth: &Option<String>,
    namespace: &Option<String>,
    set: Option<DatabasePolicyArg>,
) -> io::Result<()> {
    check_namespace_requirements(namespace, options);
    let namespace = namespace
        .as_deref()
        .unwrap_or(pyre::ast::DEFAULT_SCHEMANAME);
    let db = db::connect_existing(database, auth)
        .await
        .map_err(|err| io::Error::other(err.format_error()))?;
    let conn = db::ProcessConnection::connect(&db)
        .map_err(|err| io::Error::other(db::error::format_libsql_error(&err)))?;
    let policy = match set {
        None => policy::inspect(&conn, Some(namespace)).await,
        Some(DatabasePolicyArg::Disposable) => policy::set_disposable(&conn, Some(namespace)).await,
        Some(DatabasePolicyArg::Preserved) => return Err(io::Error::other(
            "Changing policy to preserved requires verified finalized migration history; promotion is not yet supported",
        )),
    }.map_err(|err| io::Error::other(format!("{}\n\n{err}", db::error::context(database, namespace))))?;
    println!("{policy}");
    Ok(())
}

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
