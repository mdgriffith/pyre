//! Target-local catalog. Authoring never applies pending application SQL.

use super::{
    author::{self, Draft},
    execute, Bundle, Catalog, History,
};
use libsql::{Connection, TransactionBehavior};
use std::collections::BTreeMap;

const BUNDLES: &str = "_pyre_migration_bundles";
const DRAFTS: &str = "_pyre_migration_drafts";
const CREATE_BUNDLES: &str = "create table if not exists main._pyre_migration_bundles (
    namespace text not null, name text not null, manifest text not null, files text not null,
    primary key (namespace, name)
)";
const CREATE_DRAFTS: &str = "create table if not exists main._pyre_migration_drafts (
    namespace text not null, name text not null, draft text not null,
    primary key (namespace, name)
)";

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}

pub struct DatabaseCatalog<'a> {
    connection: &'a Connection,
}

impl<'a> DatabaseCatalog<'a> {
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub async fn history(&self, namespace: &str) -> Result<History, Error> {
        self.read_history(namespace)
            .await
            .map(|(history, _)| history)
    }

    pub async fn read_history(
        &self,
        namespace: &str,
    ) -> Result<(History, BTreeMap<String, super::BundleHash>), Error> {
        let mut names = BTreeMap::new();
        let mut bundles = Vec::new();
        if exists(self.connection, BUNDLES).await? {
            let mut rows = self.connection.query("select name, manifest, files from main._pyre_migration_bundles where namespace = ? order by name", [namespace]).await.map_err(error)?;
            while let Some(row) = rows.next().await.map_err(error)? {
                let name: String = row.get(0).map_err(error)?;
                let manifest: String = row.get(1).map_err(error)?;
                let files: BTreeMap<String, String> =
                    serde_json::from_str(&row.get::<String>(2).map_err(error)?).map_err(error)?;
                let bundle = Bundle::verify(
                    namespace,
                    manifest.as_bytes(),
                    files
                        .into_iter()
                        .map(|(path, text)| (path, text.into_bytes())),
                )
                .map_err(error)?;
                names.insert(name, bundle.hash().clone());
                bundles.push(bundle);
            }
        }
        Ok((History::new(namespace, bundles).map_err(error)?, names))
    }

    pub async fn draft(&self, namespace: &str, name: &str) -> Result<Draft, Error> {
        let source = if exists(self.connection, DRAFTS).await? {
            sql_draft(self.connection, namespace, name).await?
        } else {
            None
        }
        .ok_or_else(|| Error(format!("no draft named {name}")))?;
        serde_json::from_str(&source).map_err(error)
    }

    /// Create a new draft, or replace it only when the supplied old draft is
    /// still current. Authors can edit schema, SQL, assertions and grace via this API.
    pub async fn write_draft(
        &self,
        name: &str,
        draft: &Draft,
        previous: Option<&Draft>,
    ) -> Result<(), Error> {
        if !super::valid_component(name) || !super::valid_component(&draft.namespace) {
            return Err(error("invalid draft name or namespace"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(error)?;
        let result = async {
            check_binding(&tx, &draft.namespace).await?;
            let (history, names) = DatabaseCatalog::new(&tx).read_history(&draft.namespace).await?;
            if history.head().map(Bundle::hash) != draft.predecessor.as_ref() || names.contains_key(name) {
                return Err(error("draft does not follow the current head, or its name is already finalized"));
            }
            tx.execute(CREATE_DRAFTS, ()).await.map_err(error)?;
            let expected = previous.map(serde_json::to_string).transpose().map_err(error)?;
            if sql_draft(&tx, &draft.namespace, name).await? != expected {
                return Err(error("draft already exists or changed since it was read"));
            }
            let payload = serde_json::to_string(draft).map_err(error)?;
            tx.execute("insert into main._pyre_migration_drafts(namespace, name, draft) values (?, ?, ?) on conflict(namespace, name) do update set draft = excluded.draft", libsql::params![draft.namespace.clone(), name, payload]).await.map_err(error)?;
            Ok(())
        }.await;
        finish(tx, result).await
    }

    pub async fn finalize(
        &self,
        namespace: &str,
        name: &str,
        grace_override: Option<u64>,
    ) -> Result<Bundle, Error> {
        let mut draft = self.draft(namespace, name).await?;
        let original = serde_json::to_string(&draft).map_err(error)?;
        if grace_override.is_some() {
            draft.grace_seconds = grace_override;
        }
        let history = self.history(namespace).await?;
        let (candidate, _) = draft.candidate(&history).map_err(error)?;
        author::check(&history, Some(&candidate), None)
            .await
            .map_err(error)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(error)?;
        let result = async {
            check_binding(&tx, namespace).await?;
            let (current, names) = DatabaseCatalog::new(&tx).read_history(namespace).await?;
            if current.head().map(Bundle::hash) != candidate.predecessor()
                || names.contains_key(name)
                || sql_draft(&tx, namespace, name).await?.as_deref() != Some(&original)
            {
                return Err(error(
                    "draft or catalog head changed during replay; recheck before finalizing",
                ));
            }
            store(&tx, name, &candidate).await?;
            tx.execute(
                "delete from main._pyre_migration_drafts where namespace = ? and name = ?",
                [namespace, name],
            )
            .await
            .map_err(error)?;
            Ok(candidate)
        }
        .await;
        finish(tx, result).await
    }
}

impl Catalog for DatabaseCatalog<'_> {
    type Error = Error;

    async fn load_history(&self, namespace: &str) -> Result<History, Error> {
        self.history(namespace).await
    }
}

pub(super) async fn store(conn: &Connection, name: &str, bundle: &Bundle) -> Result<(), Error> {
    conn.execute(CREATE_BUNDLES, ()).await.map_err(error)?;
    conn.execute("insert into main._pyre_migration_bundles(namespace, name, manifest, files) values (?, ?, ?, ?)", libsql::params![
        bundle.namespace(), name, String::from_utf8(bundle.manifest_json()).expect("manifest is UTF-8"),
        serde_json::to_string(bundle.files()).map_err(error)?,
    ]).await.map_err(error)?;
    Ok(())
}

async fn exists(conn: &Connection, table: &str) -> Result<bool, Error> {
    Ok(conn
        .query(
            "select 1 from main.sqlite_schema where type = 'table' and name = ?",
            [table],
        )
        .await
        .map_err(error)?
        .next()
        .await
        .map_err(error)?
        .is_some())
}

async fn sql_draft(
    conn: &Connection,
    namespace: &str,
    name: &str,
) -> Result<Option<String>, Error> {
    conn.query(
        "select draft from main._pyre_migration_drafts where namespace = ? and name = ?",
        [namespace, name],
    )
    .await
    .map_err(error)?
    .next()
    .await
    .map_err(error)?
    .map(|row| row.get(0).map_err(error))
    .transpose()
}

async fn check_binding(conn: &Connection, namespace: &str) -> Result<(), Error> {
    if execute::installed(conn)
        .await
        .map_err(error)?
        .is_some_and(|state| {
            state.binding != execute::Binding::Database || state.namespace != namespace
        })
    {
        return Err(error(
            "database has a different namespace or catalog binding",
        ));
    }
    Ok(())
}

async fn finish<T>(tx: libsql::Transaction, result: Result<T, Error>) -> Result<T, Error> {
    match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|e| error(format!("catalog commit outcome unknown: {e}")))?;
            Ok(value)
        }
        Err(cause) => {
            tx.rollback()
                .await
                .map_err(|e| error(format!("{cause}; catalog rollback outcome unknown: {e}")))?;
            Err(cause)
        }
    }
}
