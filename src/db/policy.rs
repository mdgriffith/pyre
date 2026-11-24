use std::fmt;

pub const TABLE: &str = "_pyre_database_policy";
pub const CREATE: &str = "CREATE TABLE _pyre_database_policy (
    id integer not null primary key check (id = 1),
    policy text not null check (policy in ('preserved', 'disposable'))
)";

// Use main explicitly: a TEMP object or an attached database cannot authorize writes.
pub const FIND: &str = "select sql from main.sqlite_schema where name = '_pyre_database_policy' collate nocase or (type = 'trigger' and tbl_name = '_pyre_database_policy' collate nocase) union all select sql from temp.sqlite_schema where type = 'trigger' and tbl_name = '_pyre_database_policy' collate nocase";
pub const READ: &str = "select case when count(*) = 1 and min(typeof(id)) = 'integer' and min(id) = 1 then min(policy) end as policy from main._pyre_database_policy";
pub const WRITE: &str = "insert into main._pyre_database_policy (id, policy) values (1, ?) on conflict (id) do update set policy = excluded.policy";
pub const HAS_NAMESPACE: &str = "select name from temp.pragma_table_info('_pyre_migrations', 'main') where name = 'schema_namespace'";
pub const READ_NAMESPACE: &str = "select schema_namespace from main._pyre_migrations where finished_at is not null and error is null and schema is not null order by id desc limit 1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Preserved,
    Disposable,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preserved => "preserved",
            Self::Disposable => "disposable",
        }
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(feature = "database")]
pub use native::read_in_transaction;
#[cfg(feature = "database")]
pub use native::{inspect, set_disposable, Error};
#[cfg(feature = "database")]
mod native {
    use super::*;
    use libsql::{Connection, TransactionBehavior};

    #[derive(Debug)]
    pub enum Error {
        Database(libsql::Error),
        Malformed,
        WrongNamespace {
            expected: String,
            actual: String,
        },
        CommitUnknown(libsql::Error),
        RollbackUnknown {
            cause: Box<Self>,
            rollback: libsql::Error,
        },
    }

    impl From<libsql::Error> for Error {
        fn from(error: libsql::Error) -> Self {
            Self::Database(error)
        }
    }

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Database(error) => error.fmt(f),
                Self::Malformed => f.write_str("Malformed database policy metadata"),
                Self::WrongNamespace { expected, actual } => write!(
                    f,
                    "Expected database namespace '{expected}', found '{actual}'"
                ),
                Self::CommitUnknown(error) => {
                    write!(f, "Database policy commit outcome unknown: {error}")
                }
                Self::RollbackUnknown { cause, rollback } => write!(
                    f,
                    "{cause}; database policy rollback outcome unknown: {rollback}"
                ),
            }
        }
    }

    impl std::error::Error for Error {}

    /// Inspect the physical database's policy without creating metadata.
    pub async fn inspect(conn: &Connection, namespace: Option<&str>) -> Result<Policy, Error> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .await?;
        let result = super::read_in_transaction(&tx, namespace)
            .await
            .map(|(policy, _)| policy);
        tx.rollback().await?;
        result
    }

    /// Establish an explicit disposable policy on an existing database.
    pub async fn set_disposable(
        conn: &Connection,
        namespace: Option<&str>,
    ) -> Result<Policy, Error> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let result = async {
            let (_, marked) = read_in_transaction(&tx, namespace).await?;
            if !marked {
                tx.execute(CREATE, ()).await?;
            }
            tx.execute(WRITE, [Policy::Disposable.as_str()]).await?;
            Ok(Policy::Disposable)
        }
        .await;
        match result {
            Ok(policy) => {
                tx.commit().await.map_err(Error::CommitUnknown)?;
                Ok(policy)
            }
            Err(cause) => match tx.rollback().await {
                Ok(()) => Err(cause),
                Err(rollback) => Err(Error::RollbackUnknown {
                    cause: Box::new(cause),
                    rollback,
                }),
            },
        }
    }

    /// Destructive callers must read this after acquiring their write transaction.
    pub async fn read_in_transaction(
        conn: &Connection,
        namespace: Option<&str>,
    ) -> Result<(Policy, bool), Error> {
        if let Some(expected) = namespace {
            if conn.query(HAS_NAMESPACE, ()).await?.next().await?.is_some() {
                if let Some(row) = conn.query(READ_NAMESPACE, ()).await?.next().await? {
                    if let Some(actual) = row.get::<Option<String>>(0)? {
                        if actual != expected {
                            return Err(Error::WrongNamespace {
                                expected: expected.to_string(),
                                actual,
                            });
                        }
                    }
                }
            }
        }
        let mut objects = conn.query(FIND, ()).await?;
        let Some(object) = objects.next().await? else {
            return Ok((Policy::Preserved, false));
        };
        let sql = object.get::<Option<String>>(0)?;
        if sql.as_deref() != Some(CREATE) || objects.next().await?.is_some() {
            return Err(Error::Malformed);
        }
        let row = conn
            .query(READ, ())
            .await?
            .next()
            .await?
            .ok_or(Error::Malformed)?;
        match row.get::<Option<String>>(0)?.as_deref() {
            Some("preserved") => Ok((Policy::Preserved, true)),
            Some("disposable") => Ok((Policy::Disposable, true)),
            _ => Err(Error::Malformed),
        }
    }
}
