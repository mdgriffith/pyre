use super::{introspect, migrate, policy};
use crate::ast;
use crate::generate::sql::to_sql::SqlAndParams;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

// SQLite-owned objects are identified by the reserved prefix, not a LIKE
// wildcard. Virtual-table shadow objects are managed by DROP of the virtual root.
pub const CATALOG_SQL: &str = r#"
select json_object(
  'foreign_keys', (select foreign_keys from pragma_foreign_keys),
  'extra', (select count(*) from temp.sqlite_schema) +
           (select count(*) from pragma_database_list where name not in ('main', 'temp')),
  'objects', (select json_group_array(json_object('kind', s.type, 'name', s.name,
    'parents', json(case when s.type = 'table' then (
      select json_group_array(f."table") from pragma_foreign_key_list(s.name, 'main') f
    ) else '[]' end)))
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
    parents: Vec<String>,
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
pub use native::{init_disposable, reset, Error};

#[cfg(feature = "database")]
mod native {
    use super::*;
    use libsql::{Connection, TransactionBehavior};
    use std::fmt;

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
    pub async fn reset(
        conn: &Connection,
        namespace: &str,
        source: &str,
        identities: &[ast::ExternalIdentity],
    ) -> Result<(), Error> {
        run(conn, true, namespace, source, identities).await
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
    let mut statements = vec![];
    let mut tables = BTreeMap::new();
    for object in catalog.objects {
        match object.kind.as_str() {
            "trigger" | "view" => statements.push(object),
            "table" => {
                tables.insert(object.name.to_ascii_lowercase(), object);
            }
            // Explicit indexes disappear with their owning table. Keep them
            // until then, as foreign keys may rely on their uniqueness.
            "index" => (),
            _ => {
                return Err(format!(
                    "Cannot reset object '{}' of type '{}'",
                    object.name, object.kind
                ))
            }
        }
    }
    statements.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    // Child-first ordering, taking the same FK setting as the write connection.
    let mut children = BTreeMap::<String, usize>::new();
    let mut dependencies = BTreeMap::new();
    for (key, object) in &tables {
        let parents: BTreeSet<_> = if catalog.foreign_keys == 0 {
            BTreeSet::new()
        } else {
            object
                .parents
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .filter(|name| name != key && tables.contains_key(name))
                .collect()
        };
        for parent in &parents {
            *children.entry(parent.clone()).or_default() += 1;
        }
        dependencies.insert(key.clone(), parents);
    }
    let mut ready: BTreeSet<_> = tables
        .keys()
        .filter(|key| !children.contains_key(*key))
        .cloned()
        .collect();
    while let Some(key) = ready.pop_first() {
        statements.push(tables.remove(&key).expect("known table"));
        for parent in &dependencies[&key] {
            let count = children.get_mut(parent).expect("known reference");
            *count -= 1;
            if *count == 0 {
                ready.insert(parent.clone());
            }
        }
    }
    if !tables.is_empty() {
        return Err("Cannot atomically reset tables with cyclic foreign keys while foreign keys are enforced".into());
    }
    Ok(statements
        .into_iter()
        .map(|object| {
            let quoted = crate::ext::string::quote(&object.name);
            SqlAndParams::Sql(format!("DROP {} main.{quoted}", object.kind))
        })
        .collect())
}
