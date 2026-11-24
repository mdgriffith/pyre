use clap::{Parser, Subcommand};
use std::io::{self, IsTerminal};
use std::path::Path;

mod command;
mod db;
mod filesystem;

#[derive(Parser)]
#[command(name = "pyre")]
#[command(about = "A CLI tool for pyre operations", long_about = None)]
#[command(version)]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// The input directory to read from.
    #[arg(long, global = true, default_value = "pyre")]
    r#in: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Operate on a physical Pyre database.
    Db {
        #[command(subcommand)]
        command: DatabaseCommand,
    },
    /// Get started using Pyre.  Generates a starter schema.
    Init {
        /// Generate a setup that has multiple database schemas.
        #[arg(long, default_value_t = false)]
        multidb: bool,

        /// Shared session declaration source to write to session.pyre.
        #[arg(long)]
        session: Option<String>,
    },

    /// Generate files for querying your pyre schema.
    Generate {
        /// Directory where output files will be written.
        #[arg(long, default_value = "pyre/generated")]
        out: String,
    },

    /// Format files
    Format {
        #[arg(required = false)]
        files: Vec<String>,

        /// Output to stdout instead of files
        #[arg(long, default_value_t = false)]
        to_stdout: bool,
    },

    /// Typecheck your schema and queries.
    Check {
        #[arg(required = false)]
        files: Vec<String>,

        /// Format errors as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Introspect a database and generate a pyre schema.
    Introspect {
        /// A local filename, or a url, or an environment variable if prefixed with a $.
        database: String,

        /// The Pyre namespace to store this schema under.
        #[arg(long)]
        namespace: Option<String>,

        #[arg(long)]
        auth: Option<String>,
    },

    /// Apply pending finalized migrations through the target's catalog binding.
    Migrate {
        /// A local filename, or a url, or an environment variable if prefixed with a $.
        database: String,

        #[arg(long)]
        auth: Option<String>,

        /// The Pyre schema to migrate
        #[arg(long)]
        namespace: Option<String>,

        /// Push working-schema changes to an existing, unbound disposable target.
        #[arg(long, default_value_t = false)]
        push: bool,

        /// Apply destructive push changes without confirmation.
        #[arg(long, requires = "push", default_value_t = false)]
        force: bool,

        /// Directory where migration files are stored.
        #[arg(long, default_value = "pyre/migrations")]
        migration_dir: String,
        /// Resolve an earlier outcome on a fresh write transaction without applying SQL.
        #[arg(long, conflicts_with = "push")]
        inspect: Option<String>,
    },

    /// Author and verify finalized migration bundles.
    Migration {
        #[command(flatten)]
        catalog: MigrationCatalogOptions,
        #[command(subcommand)]
        command: MigrationCommand,
    },

    /// Start the built-in single-database Pyre server.
    Serve {
        /// A local filename, or a url, or an environment variable if prefixed with a $.
        database: String,

        /// Database auth token for remote libSQL/Turso databases.
        #[arg(long)]
        auth: Option<String>,

        /// Address to bind. Defaults to loopback for safety.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Port to bind.
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Directory containing generated Pyre artifacts.
        #[arg(long, default_value = "pyre/generated")]
        generated: String,

        /// Fixed database id exposed to clients.
        #[arg(long, default_value = "default")]
        database_id: String,

        /// Trusted session header name.
        #[arg(long)]
        session_header: Option<String>,

        /// HMAC secret for signed session headers.
        #[arg(long)]
        session_secret: Option<String>,

        /// Static JSON session used for local development.
        #[arg(long)]
        dev_session: Option<String>,

        /// Allowed CORS origin. May be passed multiple times.
        #[arg(long)]
        cors_origin: Vec<String>,

        /// Sync catchup page size.
        #[arg(long, default_value_t = pyre::sync::DEFAULT_SYNC_PAGE_SIZE)]
        page_size: usize,

        /// Allow --dev-session on non-loopback bind addresses.
        #[arg(long, default_value_t = false)]
        allow_unsafe_dev_session: bool,

        /// Allow unsigned trusted session headers on non-loopback bind addresses.
        #[arg(long, default_value_t = false)]
        allow_unsafe_unsigned_session: bool,
    },

    /// Start the Pyre MCP server over stdio.
    Mcp,

    /// Start the Pyre language server over stdio.
    Lsp,

    /// Print bundled documentation by topic, or list topics.
    Docs {
        /// Documentation topic name.
        topic: Option<String>,
    },
}

#[derive(Subcommand)]
enum DatabaseCommand {
    /// Install a schema on an empty database.
    Init {
        /// A local filename, URL, or environment variable prefixed with a $.
        database: String,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        /// Use the working schema and opt in to disposable workflows.
        #[arg(long)]
        disposable: bool,
        /// Catalog to bind to finalized history (defaults to filesystem).
        #[arg(long, conflicts_with = "disposable")]
        migration_catalog: Option<MigrationBinding>,
        #[arg(long, default_value = "pyre/migrations")]
        migration_dir: String,
    },
    /// Adopt existing tables at an exact finalized target schema.
    Baseline {
        database: String,
        #[arg(long)]
        at: String,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long, default_value = "filesystem")]
        migration_catalog: MigrationBinding,
        #[arg(long, default_value = "pyre/migrations")]
        migration_dir: String,
    },
    /// Compare live structure with installed schema without modifying the target.
    Audit {
        database: String,
        #[arg(long)]
        auth: Option<String>,
    },
    /// Replace a disposable database's schema in place and start a new lineage.
    Reset {
        /// An existing local filename, URL, or environment variable prefixed with a $.
        database: String,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Inspect or explicitly change the database's lifecycle policy.
    Policy {
        /// An existing local filename, URL, or environment variable prefixed with a $.
        database: String,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long, value_enum)]
        set: Option<DatabasePolicyArg>,
        #[arg(long, default_value = "pyre/migrations")]
        migration_dir: String,
    },
    /// Add fixtures to an initialized database in one transaction.
    Seed {
        /// A local filename, URL, or environment variable prefixed with a $.
        database: String,
        /// Database auth token or an environment variable prefixed with a $.
        #[arg(long)]
        auth: Option<String>,
        /// Which project schema namespace the database must contain.
        #[arg(long)]
        namespace: Option<String>,
        /// JSON file containing fixture rows keyed by physical table name.
        #[arg(long)]
        fixtures: std::path::PathBuf,
    },
    /// Reset an existing disposable database, then seed it in a separate transaction.
    Reseed {
        /// An existing local filename, URL, or environment variable prefixed with a $.
        database: String,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        /// JSON fixture file, read only after reset commits.
        #[arg(long)]
        fixtures: std::path::PathBuf,
    },
}

type MigrationBinding = pyre::db::bundle::execute::Binding;

#[derive(clap::Args)]
struct MigrationCatalogOptions {
    /// Author in the specified target's database-backed catalog.
    #[arg(long, global = true)]
    database: Option<String>,
    #[arg(long, global = true, requires = "database")]
    auth: Option<String>,
    #[arg(long, global = true)]
    namespace: Option<String>,
    #[arg(long, global = true, default_value = "pyre/migrations")]
    migration_dir: String,
}

#[derive(Subcommand)]
enum MigrationCommand {
    /// Generate a draft and finalize it if replay succeeds.
    Create {
        name: String,
        #[arg(long)]
        draft: bool,
        #[arg(long)]
        grace: Option<String>,
        /// Use standalone schema text instead of the working project schema.
        #[arg(long)]
        schema: Option<std::path::PathBuf>,
        /// External identity definitions for --schema, as a JSON array.
        #[arg(long, requires = "schema")]
        external_identities: Option<std::path::PathBuf>,
    },
    /// Replay and verify finalized history, and optionally a named draft.
    Check {
        name: Option<String>,
        #[arg(long, requires = "name")]
        fixtures: Option<std::path::PathBuf>,
    },
    /// Replay and seal a named draft against the latest finalized head.
    Finalize {
        name: String,
        #[arg(long)]
        grace: Option<String>,
    },
    /// Restore an unapplied and unshared filesystem head to an editable draft.
    Unfinalize { name: String },
    /// Export a database-catalog draft as JSON.
    ShowDraft { name: String },
    /// Update a database-catalog draft from JSON.
    EditDraft {
        name: String,
        file: std::path::PathBuf,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DatabasePolicyArg {
    Disposable,
    Preserved,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{}", error);
        std::process::exit(1);
    }
}

async fn run() -> io::Result<()> {
    let cli = Cli::parse();

    // Check if stderr is a TTY to determine if we should enable color output.
    // Disable color when output is redirected (e.g., `pyre check 2> errors.txt`)
    // or piped (e.g., `pyre check 2>&1 | grep error`) to avoid ANSI codes in files/pipes.
    let enable_color = io::stderr().is_terminal();
    let options = command::Options {
        in_dir: Path::new(&cli.r#in),
        enable_color,
    };

    match &cli.command {
        Commands::Db { command } => match command {
            DatabaseCommand::Init {
                database,
                auth,
                namespace,
                disposable,
                migration_catalog,
                migration_dir,
            } => {
                if !disposable {
                    command::finalized::initialize(
                        &options,
                        database,
                        auth,
                        namespace.as_deref(),
                        std::path::Path::new(migration_dir),
                        migration_catalog.unwrap_or(MigrationBinding::Filesystem),
                    )
                    .await?;
                    println!("Initialized preserved database");
                } else {
                    command::prepare_disposable_database(
                        &options,
                        database,
                        auth,
                        namespace,
                        command::DisposableOperation::Init,
                    )
                    .await?;
                    println!("Initialized disposable database");
                }
            }
            DatabaseCommand::Baseline {
                database,
                at,
                auth,
                namespace,
                migration_catalog,
                migration_dir,
            } => {
                command::finalized::baseline(
                    database,
                    auth,
                    namespace.as_deref(),
                    Path::new(migration_dir),
                    *migration_catalog,
                    at,
                )
                .await?;
            }
            DatabaseCommand::Audit { database, auth } => {
                command::finalized::audit(database, auth).await?
            }
            DatabaseCommand::Reset {
                database,
                auth,
                namespace,
            } => {
                command::prepare_disposable_database(
                    &options,
                    database,
                    auth,
                    namespace,
                    command::DisposableOperation::Reset,
                )
                .await?;
                println!("Reset disposable database");
            }
            DatabaseCommand::Policy {
                database,
                auth,
                namespace,
                set,
                migration_dir,
            } => {
                command::database_policy(database, auth, namespace, *set, Path::new(migration_dir))
                    .await?;
            }
            DatabaseCommand::Seed {
                database,
                auth,
                namespace,
                fixtures,
            } => {
                command::seed_database(&options, database, auth, namespace, fixtures).await?;
            }
            DatabaseCommand::Reseed {
                database,
                auth,
                namespace,
                fixtures,
            } => {
                command::prepare_disposable_database(
                    &options,
                    database,
                    auth,
                    namespace,
                    command::DisposableOperation::Reseed(fixtures),
                )
                .await?;
            }
        },
        Commands::Init { multidb, session } => {
            command::init(&options, *multidb, session.as_deref())?;
        }
        Commands::Generate { out } => {
            command::generate(&options, out)?;
        }
        Commands::Format { files, to_stdout } => {
            command::format(&options, files, *to_stdout)?;
        }
        Commands::Check { files, json } => {
            command::check(&options, files.clone(), *json)?;
        }
        Commands::Introspect {
            database,
            auth,
            namespace,
        } => {
            command::introspect(&options, database, auth, namespace).await?;
        }
        Commands::Migrate {
            database,
            auth,
            push,
            force,
            migration_dir,
            namespace,
            inspect,
        } => {
            if *push {
                command::push(&options, database, auth, namespace, *force).await?;
            } else {
                command::finalized::apply(
                    database,
                    auth,
                    namespace.as_deref(),
                    Path::new(migration_dir),
                    inspect.as_deref(),
                )
                .await?;
            }
        }
        Commands::Migration {
            catalog,
            command: operation,
        } => command::finalized::author(&options, catalog, operation).await?,
        Commands::Serve {
            database,
            auth,
            host,
            port,
            generated,
            database_id,
            session_header,
            session_secret,
            dev_session,
            cors_origin,
            page_size,
            allow_unsafe_dev_session,
            allow_unsafe_unsigned_session,
        } => {
            command::serve(
                &options,
                command::ServeOptions {
                    database,
                    auth,
                    host,
                    port: *port,
                    generated,
                    database_id,
                    session_header,
                    session_secret,
                    dev_session,
                    cors_origins: cors_origin,
                    page_size: *page_size,
                    allow_unsafe_dev_session: *allow_unsafe_dev_session,
                    allow_unsafe_unsigned_session: *allow_unsafe_unsigned_session,
                },
            )
            .await?;
        }
        Commands::Mcp => {
            command::mcp(&options).await?;
        }
        Commands::Lsp => {
            command::lsp(&options).await?;
        }
        Commands::Docs { topic } => {
            command::docs(topic)?;
        }
    }
    Ok(())
}
