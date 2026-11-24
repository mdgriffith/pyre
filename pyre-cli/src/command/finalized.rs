use super::shared::{parse_database_schemas, Options};
use crate::{db, MigrationCatalogOptions, MigrationCommand};
use pyre::ast::{ExternalIdentity, DEFAULT_SCHEMANAME};
use pyre::db::bundle::{
    author::{self, Draft},
    database::DatabaseCatalog,
    execute::{self, Binding},
    filesystem::FilesystemCatalog,
    Bundle, BundleHash, History,
};
use std::{collections::BTreeMap, io, path::Path};

type NamedHistory = (History, BTreeMap<String, BundleHash>);

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

async fn connect(
    database: &str,
    auth: &Option<String>,
    create: bool,
) -> io::Result<db::ProcessConnection> {
    let db = if create {
        db::connect(database, auth).await
    } else {
        db::connect_existing(database, auth).await
    }
    .map_err(|e| other(e.format_error()))?;
    db::ProcessConnection::connect(&db).map_err(other)
}

pub fn project_schema(
    options: &Options<'_>,
    namespace: &str,
) -> io::Result<(String, Vec<ExternalIdentity>)> {
    let paths = crate::filesystem::collect_filepaths(options.in_dir)?;
    let database = parse_database_schemas(&paths, options.enable_color)?;
    let schema = database
        .schemas
        .iter()
        .find(|schema| schema.namespace == namespace)
        .ok_or_else(|| other(format!("no schema named {namespace}")))?;
    let context = pyre::typecheck::check_schema(&database).map_err(|errors| {
        other(
            errors
                .iter()
                .map(|e| pyre::error::to_error_description(e, false))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })?;
    Ok(pyre::generate::to_string::standalone_schema(
        &context, schema,
    ))
}

async fn load(
    conn: &libsql::Connection,
    root: &Path,
    namespace: &str,
    binding: Binding,
) -> io::Result<NamedHistory> {
    match binding {
        Binding::Filesystem => FilesystemCatalog::new(root)
            .read_named_history(namespace)
            .map_err(other),
        Binding::Database => DatabaseCatalog::new(conn)
            .read_history(namespace)
            .await
            .map_err(other),
    }
}

fn named<'a>(catalog: &'a NamedHistory, name: &str) -> io::Result<&'a Bundle> {
    let hash = catalog.1.get(name).cloned().or_else(|| name.parse().ok());
    catalog
        .0
        .bundles()
        .iter()
        .find(|bundle| Some(bundle.hash()) == hash.as_ref())
        .ok_or_else(|| other(format!("no finalized migration named {name}")))
}

fn note_application(root: &Path, history: &History) -> io::Result<()> {
    if history.bundles().is_empty() {
        return Ok(());
    }
    FilesystemCatalog::new(root)
        .note_application(history)
        .map_err(other)
}

pub async fn initialize(
    options: &Options<'_>,
    database: &str,
    auth: &Option<String>,
    namespace: Option<&str>,
    root: &Path,
    binding: Binding,
) -> io::Result<()> {
    let namespace = namespace.unwrap_or(DEFAULT_SCHEMANAME);
    if binding == Binding::Database {
        let (source, identities) = project_schema(options, namespace)?;
        let conn = connect(database, auth, true).await?;
        author::bootstrap_database_catalog(&conn, namespace, source, identities)
            .await
            .map_err(other)?;
        return Ok(());
    }
    let history = FilesystemCatalog::new(root)
        .read_history(namespace)
        .map_err(other)?;
    if history.bundles().is_empty() {
        return Err(other(
            "preserved initialization requires finalized history; run pyre migration create",
        ));
    }
    let conn = connect(database, auth, true).await?;
    note_application(root, &history)?;
    execute::initialize(&conn, &history, binding)
        .await
        .map_err(other)?;
    Ok(())
}

pub async fn apply(
    database: &str,
    auth: &Option<String>,
    namespace: Option<&str>,
    root: &Path,
    inspect: Option<&str>,
) -> io::Result<()> {
    let conn = connect(database, auth, false).await?;
    let installed = execute::installed(&conn)
        .await
        .map_err(other)?
        .ok_or_else(|| other("no finalized history; use pyre db init or pyre db baseline"))?;
    if namespace.is_some_and(|namespace| namespace != installed.namespace) {
        return Err(other(
            "requested namespace does not match the installed database",
        ));
    }
    let catalog = load(&conn, root, &installed.namespace, installed.binding).await?;
    if let Some(name) = inspect {
        let bundle = named(&catalog, name)?;
        match execute::inspect_outcome(&conn, bundle, installed.binding)
            .await
            .map_err(other)?
        {
            execute::Inspection::Applied(receipt) => println!("applied at {}", receipt.applied_at),
            execute::Inspection::NotApplied => {
                println!("not applied; target remains at the expected predecessor")
            }
        }
        return Ok(());
    }
    if installed.binding == Binding::Filesystem {
        note_application(root, &catalog.0)?;
    }
    let outcomes = execute::apply_history(&conn, &catalog.0, installed.binding)
        .await
        .map_err(other)?;
    let applied = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, execute::Application::Applied(_)))
        .count();
    println!("Applied {applied} finalized migration(s)");
    Ok(())
}

pub async fn baseline(
    database: &str,
    auth: &Option<String>,
    namespace: Option<&str>,
    root: &Path,
    binding: Binding,
    at: &str,
) -> io::Result<()> {
    let namespace = namespace.unwrap_or(DEFAULT_SCHEMANAME);
    let conn = connect(database, auth, false).await?;
    let catalog = load(&conn, root, namespace, binding).await?;
    let bundle = named(&catalog, at)?;
    if binding == Binding::Filesystem {
        note_application(root, &catalog.0)?;
    }
    execute::baseline(&conn, &catalog.0, bundle.hash(), binding)
        .await
        .map_err(other)?;
    println!("Recorded baseline {}", bundle.hash());
    Ok(())
}

pub async fn audit(database: &str, auth: &Option<String>) -> io::Result<()> {
    let conn = connect(database, auth, false).await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Deferred)
        .await
        .map_err(other)?;
    let schema = pyre::server::schema::load_schema_from_database(&tx)
        .await
        .map_err(other)?;
    let comparison = author::audit_schema(&tx, schema.context().map_err(other)?).await;
    tx.rollback().await.map_err(other)?;
    comparison.map_err(other)?;
    println!("Live structure matches the installed schema");
    Ok(())
}

pub async fn promote(
    conn: &libsql::Connection,
    namespace: Option<&str>,
    root: &Path,
) -> io::Result<pyre::db::policy::Policy> {
    let state = execute::installed(conn)
        .await
        .map_err(other)?
        .ok_or_else(|| {
            other("promotion requires verified finalized history or an explicit baseline")
        })?;
    if namespace.is_some_and(|namespace| namespace != state.namespace) {
        return Err(other(
            "requested namespace does not match the installed database",
        ));
    }
    let (history, _) = load(conn, root, &state.namespace, state.binding).await?;
    execute::promote(conn, &history, state.binding)
        .await
        .map_err(other)
}

pub async fn author(
    options: &Options<'_>,
    catalog: &MigrationCatalogOptions,
    operation: &MigrationCommand,
) -> io::Result<()> {
    let namespace = catalog.namespace.as_deref().unwrap_or(DEFAULT_SCHEMANAME);
    let fs = FilesystemCatalog::new(&catalog.migration_dir);
    let conn = match &catalog.database {
        Some(database) => Some(connect(database, &catalog.auth, false).await?),
        None => None,
    };
    let db = conn.as_deref().map(DatabaseCatalog::new);
    let history = match &db {
        Some(db) => db.read_history(namespace).await.map_err(other)?,
        None => fs.read_named_history(namespace).map_err(other)?,
    };
    match operation {
        MigrationCommand::Create {
            name,
            draft,
            grace,
            schema,
            external_identities,
        } => {
            let (source, identities) = match schema {
                Some(path) => (
                    std::fs::read_to_string(path)?,
                    external_identities
                        .as_ref()
                        .map(|path| -> io::Result<_> {
                            serde_json::from_slice(&std::fs::read(path)?).map_err(other)
                        })
                        .transpose()?
                        .unwrap_or_default(),
                ),
                None => project_schema(options, namespace)?,
            };
            let generated = author::create_draft(
                &history.0,
                source,
                identities,
                grace
                    .as_deref()
                    .map(author::parse_grace)
                    .transpose()
                    .map_err(other)?,
            )
            .await
            .map_err(other)?;
            for message in &generated.diagnostics {
                println!("{message}");
            }
            match &db {
                Some(db) => db
                    .write_draft(name, &generated.draft, None)
                    .await
                    .map_err(other)?,
                None => {
                    let path = fs.write_draft(name, &generated.draft).map_err(other)?;
                    println!("Draft: {}", path.display());
                }
            }
            if !draft && !generated.requires_manual_sql {
                finalize(&fs, db.as_ref(), namespace, name, None).await?;
            } else if generated.requires_manual_sql {
                println!("Draft requires explicit SQL; edit it and run migration check");
            }
        }
        MigrationCommand::Check { name, fixtures } => {
            let candidate = if let Some(name) = name {
                let draft = match &db {
                    Some(db) => db.draft(namespace, name).await.map_err(other)?,
                    None => fs.draft(namespace, name).map_err(other)?,
                };
                let (bundle, transition) = draft.candidate(&history.0).map_err(other)?;
                println!(
                    "API {} -> {} ({:?})",
                    transition
                        .from
                        .map_or_else(|| "initial".into(), |v| v.to_string()),
                    transition.to,
                    transition.change
                );
                Some(bundle)
            } else {
                None
            };
            let input: Option<Box<serde_json::value::RawValue>> = fixtures
                .as_ref()
                .map(|path| -> io::Result<_> {
                    serde_json::from_slice(&std::fs::read(path)?).map_err(other)
                })
                .transpose()?;
            let mut previous = None;
            for bundle in history.0.bundles() {
                let name = history
                    .1
                    .iter()
                    .find(|(_, hash)| *hash == bundle.hash())
                    .map(|(name, _)| name.as_str())
                    .unwrap_or("?");
                let transition = bundle.verify_api_transition(previous).map_err(other)?;
                println!(
                    "{name}  {}  API {} ({:?})",
                    bundle.hash(),
                    transition.to,
                    transition.change
                );
                previous = Some(bundle);
            }
            author::check(&history.0, candidate.as_ref(), input.as_deref())
                .await
                .map_err(other)?;
            println!("Replay and structural checks passed");
        }
        MigrationCommand::Finalize { name, grace } => {
            finalize(
                &fs,
                db.as_ref(),
                namespace,
                name,
                grace
                    .as_deref()
                    .map(author::parse_grace)
                    .transpose()
                    .map_err(other)?,
            )
            .await?
        }
        MigrationCommand::Unfinalize { name } => {
            if db.is_some() {
                return Err(other(
                    "database-catalog bundles cannot be unfinalized; create a follow-up migration",
                ));
            }
            fs.unfinalize(namespace, name, git_tracks_manifest)
                .map_err(other)?;
        }
        MigrationCommand::ShowDraft { name } => {
            let db = db.ok_or_else(|| other("show-draft requires a database catalog"))?;
            let draft = db.draft(namespace, name).await.map_err(other)?;
            println!("{}", serde_json::to_string_pretty(&draft).map_err(other)?);
        }
        MigrationCommand::EditDraft { name, file } => {
            let db = db.ok_or_else(|| other("edit-draft requires a database catalog"))?;
            let old = db.draft(namespace, name).await.map_err(other)?;
            let draft: Draft = serde_json::from_slice(&std::fs::read(file)?).map_err(other)?;
            if draft.namespace != namespace {
                return Err(other("draft namespace changed"));
            }
            db.write_draft(name, &draft, Some(&old))
                .await
                .map_err(other)?;
        }
    }
    Ok(())
}

async fn finalize(
    fs: &FilesystemCatalog,
    db: Option<&DatabaseCatalog<'_>>,
    namespace: &str,
    name: &str,
    grace: Option<u64>,
) -> io::Result<()> {
    let result = match db {
        Some(db) => db.finalize(namespace, name, grace).await.map_err(other),
        None => fs.finalize(namespace, name, grace).await.map_err(other),
    };
    let bundle = result.map_err(|error| other(format!("{error}; draft retained")))?;
    println!(
        "Finalized {name}  {}  API {}",
        bundle.hash(),
        bundle.target_api_version()
    );
    Ok(())
}

fn git_tracks_manifest(path: &Path) -> bool {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(path.parent().unwrap())
            .args(args)
            .output()
    };
    let Ok(repo) = git(&["rev-parse", "--is-inside-work-tree"]) else {
        return true;
    };
    if !repo.status.success() {
        return false;
    }
    let Ok(log) = git(&["log", "--all", "-1", "--format=%H", "--", "manifest.json"]) else {
        return true;
    };
    !log.status.success() || !log.stdout.is_empty()
}
