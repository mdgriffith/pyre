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

    /// Execute any migrations that are needed.
    Migrate {
        /// A local filename, or a url, or an environment variable if prefixed with a $.
        database: String,

        #[arg(long)]
        auth: Option<String>,

        /// The Pyre schema to migrate
        #[arg(long)]
        namespace: Option<String>,

        /// Push changes directly to the DB
        #[arg(long, default_value_t = false)]
        push: bool,

        /// Directory where migration files are stored.
        #[arg(long, default_value = "pyre/migrations")]
        migration_dir: String,
    },

    /// Generate a migration
    Migration {
        /// The migration name.
        name: String,

        #[arg(long)]
        db: String,

        #[arg(long)]
        auth: Option<String>,

        /// The Pyre namespace to generate a migration for.
        #[arg(long)]
        namespace: Option<String>,

        /// Directory where migration files are stored.
        #[arg(long, default_value = "pyre/migrations")]
        migration_dir: String,
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

    /// Print bundled documentation by topic, or list topics.
    Docs {
        /// Documentation topic name.
        topic: Option<String>,
    },
}

#[tokio::main]
async fn main() -> io::Result<()> {
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
            migration_dir,
            namespace,
        } => {
            if *push {
                command::push(&options, database, auth, namespace).await?;
            } else {
                command::migrate(&options, database, auth, migration_dir, namespace).await?;
            }
        }
        Commands::Migration {
            name,
            db,
            auth,
            migration_dir,
            namespace,
        } => {
            command::generate_migration(
                &options,
                name,
                db,
                auth,
                Path::new(migration_dir),
                namespace,
            )
            .await?;
        }
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
        Commands::Docs { topic } => {
            command::docs(topic)?;
        }
    }
    Ok(())
}
