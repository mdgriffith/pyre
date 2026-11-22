use super::{introspect, migrate, policy};
use crate::ast;
use crate::generate::sql::to_sql::SqlAndParams;
use serde::Deserialize;

// SQLite-owned objects are identified by the reserved prefix, not a LIKE
// wildcard. Virtual-table shadow objects are managed by DROP of the virtual root.
pub const CATALOG_SQL: &str = r#"
select json_object(
  'foreign_keys', (select foreign_keys from pragma_foreign_keys),
  'extra', (select count(*) from temp.sqlite_schema) +
           (select count(*) from pragma_database_list where name not in ('main', 'temp')),
  'objects', (select json_group_array(json_object('kind', s.type, 'name', s.name,
    'has_foreign_keys', case when s.type = 'table' then (
      select exists(select 1 from pragma_foreign_key_list(s.name, 'main'))
    ) else 0 end))
    from main.sqlite_schema s
    left join pragma_table_list t on t.schema = 'main' and t.name = s.name and s.type = 'table'
    where lower(substr(s.name, 1, 7)) != 'sqlite_' and coalesce(t.type, '') != 'shadow')
) as catalog"#;

#[derive(Deserialize)]
pub struct Catalog {
    foreign_keys: u8,
    extra: u64,
    objects: Vec<Object>,
}

#[derive(Deserialize)]
struct Object {
    kind: String,
    name: String,
    has_foreign_keys: u8,
}

pub fn plan_disposable(
    catalog: Catalog,
    reset: bool,
    namespace: &str,
    source: &str,
    identities: &[ast::ExternalIdentity],
) -> Result<Vec<SqlAndParams>, String> {
    if catalog.extra != 0 {
        return Err("Database lifecycle operations require a connection without temporary objects or attached databases".into());
    }
    let mut statements = if reset {
        drop_statements(catalog)?
    } else if catalog.objects.is_empty() {
        vec![]
    } else {
        return Err("Initialization requires an empty database".into());
    };

    let empty = introspect::from_raw(introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: introspect::MigrationState::NoMigrationTable,
        schema_source: String::new(),
        links: vec![],
        schema_namespace: None,
        external_identities: vec![],
    });
    let plan = migrate::migrate_dynamic_with_external_identities(
        "disposable:working".into(),
        &empty,
        source,
        "schema.pyre",
        namespace,
        identities,
    )
    .map_err(|errors| {
        errors
            .iter()
            .map(|error| crate::error::to_error_description(error, false))
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    if plan.sql.is_empty() {
        statements.extend(migrate::internal_setup_sql());
    }
    statements.extend(plan.sql);
    statements.push(plan.mark_success);
    statements.push(migrate::record_namespace_sql(namespace));
    statements.extend(migrate::external_identity_sql(&plan.external_identities));
    statements.push(SqlAndParams::Sql(policy::CREATE.into()));
    statements.push(SqlAndParams::SqlWithParams {
        sql: policy::WRITE.into(),
        args: vec![policy::Policy::Disposable.as_str().into()],
    });
    Ok(statements)
}

#[cfg(feature = "database")]
pub use native::{
    init_disposable, reseed, reseed_with, reseed_with_connection_factory, reset,
    reset_with_connection_factory, Error, ReseedError,
};

#[cfg(feature = "database")]
mod native {
    use super::*;
    use crate::server::seed;
    use libsql::{Connection, Database, TransactionBehavior};
    use serde::Serialize;
    use std::convert::Infallible;
    use std::fmt;
    use std::ops::Deref;

    #[derive(Debug)]
    pub enum Error {
        Policy(policy::Error),
        RequiresDisposable,
        Database(libsql::Error),
        Plan(String),
        CommitUnknown(libsql::Error),
        RollbackUnknown {
            cause: Box<Self>,
            rollback: libsql::Error,
        },
    }

    impl From<libsql::Error> for Error {
        fn from(value: libsql::Error) -> Self {
            Self::Database(value)
        }
    }

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Policy(error) => error.fmt(f),
                Self::RequiresDisposable => {
                    f.write_str("Reset requires an existing disposable database")
                }
                Self::Database(error) => error.fmt(f),
                Self::Plan(message) => f.write_str(message),
                Self::CommitUnknown(error) => {
                    write!(f, "Database lifecycle commit outcome unknown: {error}")
                }
                Self::RollbackUnknown { cause, rollback } => write!(
                    f,
                    "{cause}; database lifecycle rollback outcome unknown: {rollback}"
                ),
            }
        }
    }

    impl std::error::Error for Error {}

    /// Install the current schema in an empty database and mark it disposable.
    pub async fn init_disposable(
        conn: &Connection,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
    ) -> Result<(), Error> {
        run(conn, false, namespace, source, identities).await
    }

    /// Replace the schema and lineage in place in one write transaction.
    /// Opens a dedicated connection so FK settings cannot leak into other work.
    pub async fn reset(
        db: &Database,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
    ) -> Result<(), Error> {
        reset_with_connection_factory(|| db.connect().map(Box::new), namespace, source, identities)
            .await
    }

    /// The factory must open a connection reserved exclusively for reset.
    #[doc(hidden)]
    pub async fn reset_with_connection_factory<C: Deref<Target = Connection>>(
        connect: impl FnOnce() -> Result<C, libsql::Error>,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
    ) -> Result<(), Error> {
        let conn = connect()?;
        // Some remote drivers start a separate session for the transaction.
        // The planner checks the setting again on that session before any DDL.
        let _ = conn.execute("pragma foreign_keys = off", ()).await;
        run(&conn, true, namespace, source, identities).await
    }

    #[derive(Debug)]
    pub enum ReseedError {
        ResetFailed(Error),
        ResetOutcomeUnknown(Error),
        FreshWithoutFixtures(String),
        SeedOutcomeUnknown(seed::Error),
    }

    impl fmt::Display for ReseedError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::ResetFailed(cause) => write!(f, "Reseed stopped during reset: {cause}"),
                Self::ResetOutcomeUnknown(cause) => write!(
                    f,
                    "Reseed reset outcome unknown. Inspect the target before retrying: {cause}"
                ),
                Self::FreshWithoutFixtures(cause) => write!(
                    f,
                    "Reset committed: the database has a fresh initialized schema and no fixture rows. Fix the failure and rerun reseed from the beginning: {cause}"
                ),
                Self::SeedOutcomeUnknown(cause) => write!(
                    f,
                    "Reset committed, but reseed fixture outcome is unknown. Inspect the target before retrying: {cause}"
                ),
            }
        }
    }

    impl std::error::Error for ReseedError {}

    /// Commit a reset, then serialize and insert fixtures in a separate seed operation.
    /// The caller must keep application traffic stopped until the database is ready.
    pub async fn reseed<T: Serialize>(
        db: &Database,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
        input: T,
    ) -> Result<seed::SeedResult, ReseedError> {
        reseed_with(db, namespace, source, identities, || {
            Ok::<_, Infallible>(input)
        })
        .await
    }

    /// Like [`reseed`], but load the fixture input only after reset commits.
    pub async fn reseed_with<T: Serialize, E: fmt::Display>(
        db: &Database,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
        load_fixtures: impl FnOnce() -> Result<T, E>,
    ) -> Result<seed::SeedResult, ReseedError> {
        reseed_with_connection_factory(
            || db.connect().map(Box::new),
            namespace,
            source,
            identities,
            load_fixtures,
        )
        .await
    }

    /// The factory must open separate connections for reset and fixture ingestion.
    #[doc(hidden)]
    pub async fn reseed_with_connection_factory<
        T: Serialize,
        E: fmt::Display,
        C: Deref<Target = Connection>,
    >(
        connect: impl Fn() -> Result<C, libsql::Error>,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
        load_fixtures: impl FnOnce() -> Result<T, E>,
    ) -> Result<seed::SeedResult, ReseedError> {
        reset_with_connection_factory(&connect, namespace, source, identities)
            .await
            .map_err(|error| match error {
                Error::CommitUnknown(_) | Error::RollbackUnknown { .. } => {
                    ReseedError::ResetOutcomeUnknown(error)
                }
                _ => ReseedError::ResetFailed(error),
            })?;
        let input = load_fixtures()
            .map_err(|error| ReseedError::FreshWithoutFixtures(error.to_string()))?;
        let conn =
            connect().map_err(|error| ReseedError::FreshWithoutFixtures(error.to_string()))?;
        seed::seed_for_namespace(&conn, Some(namespace), input)
            .await
            .map_err(|error| match error {
                seed::Error::CommitUnknown(_) | seed::Error::RollbackUnknown { .. } => {
                    ReseedError::SeedOutcomeUnknown(error)
                }
                _ => ReseedError::FreshWithoutFixtures(error.to_string()),
            })
    }

    async fn run(
        conn: &Connection,
        reset: bool,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
    ) -> Result<(), Error> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let result = async {
            if reset {
                let (policy, _) = policy::read_in_transaction(&tx, Some(namespace))
                    .await
                    .map_err(Error::Policy)?;
                if policy != policy::Policy::Disposable {
                    return Err(Error::RequiresDisposable);
                }
            }
            let catalog = {
                let mut rows = tx.query(CATALOG_SQL, ()).await?;
                let row = rows
                    .next()
                    .await?
                    .ok_or_else(|| Error::Plan("Could not read database object catalog".into()))?;
                serde_json::from_str(&row.get::<String>(0)?).map_err(|error| {
                    Error::Plan(format!("Invalid database object catalog: {error}"))
                })?
            };
            for statement in plan_disposable(catalog, reset, namespace, source, identities)
                .map_err(Error::Plan)?
            {
                match statement {
                    SqlAndParams::Sql(sql) => tx.execute(&sql, ()).await?,
                    SqlAndParams::SqlWithParams { sql, args } => {
                        tx.execute(&sql, libsql::params_from_iter(args)).await?
                    }
                };
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => tx.commit().await.map_err(Error::CommitUnknown),
            Err(cause) => match tx.rollback().await {
                Ok(()) => Err(cause),
                Err(rollback) => Err(Error::RollbackUnknown {
                    cause: Box::new(cause),
                    rollback,
                }),
            },
        }
    }
}

fn drop_statements(catalog: Catalog) -> Result<Vec<SqlAndParams>, String> {
    // DROP TABLE implicitly deletes rows with FK enforcement on. Deferring
    // checks does not stop cascades or other referential actions from running.
    if catalog.foreign_keys != 0 && catalog.objects.iter().any(|obj| obj.has_foreign_keys != 0) {
        return Err("Reset requires foreign-key enforcement to be disabled on the reset transaction; this connection cannot reset a schema with foreign keys".into());
    }
    let mut objects = vec![];
    for object in catalog.objects {
        match object.kind.as_str() {
            "trigger" | "view" | "table" => objects.push(object),
            // Explicit indexes disappear with their owning table.
            "index" => (),
            _ => {
                return Err(format!(
                    "Cannot reset object '{}' of type '{}'",
                    object.name, object.kind
                ))
            }
        }
    }
    objects.sort_by(|a, b| {
        // Drop triggers and views before their tables. Tables need no ordering
        // beyond deterministic output when there are no active FK actions.
        (a.kind == "table", &a.kind, &a.name).cmp(&(b.kind == "table", &b.kind, &b.name))
    });
    Ok(objects
        .into_iter()
        .map(|object| {
            let quoted = crate::ext::string::quote(&object.name);
            SqlAndParams::Sql(format!("DROP {} main.{quoted}", object.kind))
        })
        .collect())
}
