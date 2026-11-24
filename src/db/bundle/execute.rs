//! Single-target finalized migration execution. Hosts own the maintenance window.

use super::{sql, Bundle, BundleHash, History};
use crate::db::{migrate, policy};
use crate::generate::sql::to_sql::SqlAndParams;
use libsql::{Connection, TransactionBehavior};
use std::{fmt, str::FromStr};

const STATE: &str = "_pyre_finalized_schema";
const CREATE_STATE: &str = "create table main._pyre_finalized_schema (
    id integer primary key check (id = 1), namespace text not null,
    catalog text not null check (catalog in ('filesystem', 'database')),
    head text not null, baseline text, api_version text not null, fingerprint text not null
)";
const CREATE_RECEIPTS: &str = "create table main._pyre_migration_receipts (
    hash text primary key not null, predecessor text, applied_at integer not null,
    api_version text not null, update_by integer
)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Filesystem,
    Database,
}

impl Binding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Database => "database",
        }
    }
}

impl FromStr for Binding {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "filesystem" => Ok(Self::Filesystem),
            "database" => Ok(Self::Database),
            _ => Err("expected filesystem or database migration catalog"),
        }
    }
}

#[derive(Debug)]
pub struct Installed {
    pub namespace: String,
    pub binding: Binding,
    pub head: BundleHash,
    pub baseline: Option<BundleHash>,
    pub api_version: String,
    pub fingerprint: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Receipt {
    pub applied_at: i64,
    pub update_by: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Application {
    Applied(Receipt),
    AlreadyApplied(Receipt),
    CoveredByBaseline,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Inspection {
    Applied(Receipt),
    NotApplied,
}

#[derive(Debug)]
pub enum Error {
    NotApplied(String),
    RolledBack(String),
    OutcomeUnknown(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApplied(reason) => write!(f, "not applied: {reason}"),
            Self::RolledBack(reason) => write!(f, "rolled back: {reason}"),
            Self::OutcomeUnknown(reason) => write!(
                f,
                "outcome unknown: {reason}; reconnect and inspect before retrying"
            ),
        }
    }
}

impl std::error::Error for Error {}

async fn first(
    conn: &Connection,
    sql: &str,
    args: impl libsql::params::IntoParams,
) -> Result<Option<libsql::Row>, String> {
    conn.query(sql, args)
        .await
        .map_err(message)?
        .next()
        .await
        .map_err(message)
}

fn message(error: impl fmt::Display) -> String {
    error.to_string()
}

async fn table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
    Ok(first(
        conn,
        "select 1 from main.sqlite_schema where type = 'table' and name = ?",
        [table],
    )
    .await?
    .is_some())
}

/// Read target-owned catalog binding and runtime installation metadata.
pub async fn installed(conn: &Connection) -> Result<Option<Installed>, String> {
    if !table_exists(conn, STATE).await? {
        return Ok(None);
    }
    let row = first(conn, "select namespace, catalog, head, baseline, api_version, fingerprint from main._pyre_finalized_schema where id = 1", ()).await?
        .ok_or_else(|| "missing finalized installation metadata".to_string())?;
    let parse_hash = |s: String| s.parse::<BundleHash>().map_err(message);
    Ok(Some(Installed {
        namespace: row.get(0).map_err(message)?,
        binding: row
            .get::<String>(1)
            .map_err(message)?
            .parse()
            .map_err(message)?,
        head: parse_hash(row.get(2).map_err(message)?)?,
        baseline: row
            .get::<Option<String>>(3)
            .map_err(message)?
            .map(parse_hash)
            .transpose()?,
        api_version: row.get(4).map_err(message)?,
        fingerprint: row.get(5).map_err(message)?,
    }))
}

async fn receipt(conn: &Connection, bundle: &Bundle) -> Result<Option<Receipt>, String> {
    let Some(row) = first(conn, "select predecessor, applied_at, api_version, update_by from main._pyre_migration_receipts where hash = ?", [bundle.hash().to_string()]).await? else {
        return Ok(None);
    };
    let predecessor: Option<String> = row.get(0).map_err(message)?;
    if predecessor != bundle.predecessor().map(ToString::to_string)
        || row.get::<String>(2).map_err(message)? != bundle.target_api_version()
    {
        return Err("recorded receipt conflicts with the migration artifact".into());
    }
    Ok(Some(Receipt {
        applied_at: row.get(1).map_err(message)?,
        update_by: row.get(3).map_err(message)?,
    }))
}

fn chain_position(history: &History, hash: &BundleHash) -> Result<usize, String> {
    history
        .bundles()
        .iter()
        .position(|bundle| bundle.hash() == hash)
        .ok_or_else(|| format!("target history diverges: {hash} is absent from the bound catalog"))
}

fn validate_installation(
    history: &History,
    binding: Binding,
    state: &Installed,
) -> Result<usize, String> {
    if state.namespace != history.namespace() || state.binding != binding {
        return Err("target namespace or catalog does not match its stored binding".into());
    }
    let position = chain_position(history, &state.head)?;
    let bundle = &history.bundles()[position];
    if state.api_version != bundle.target_api_version()
        || state.fingerprint != bundle.runtime_fingerprint().map_err(message)?
    {
        return Err("installed schema metadata conflicts with its catalog artifact".into());
    }
    Ok(position)
}

/// Apply all bundles in a verified catalog. Each step commits independently.
pub async fn apply_history(
    conn: &Connection,
    history: &History,
    binding: Binding,
) -> Result<Vec<Application>, Error> {
    if history.bundles().is_empty() {
        return Err(Error::NotApplied(
            "no finalized migrations in this namespace".into(),
        ));
    }
    let mut results = Vec::new();
    for index in 0..history.bundles().len() {
        results.push(run_step(conn, history, index, binding, false).await?);
    }
    Ok(results)
}

/// Start preserved history on an empty target. The first commit establishes the
/// policy and catalog binding; a later failure leaves the committed prefix.
pub async fn initialize(
    conn: &Connection,
    history: &History,
    binding: Binding,
) -> Result<Vec<Application>, Error> {
    if history.bundles().is_empty() {
        return Err(Error::NotApplied(
            "preserved initialization requires finalized history".into(),
        ));
    }
    let mut results = vec![run_step(conn, history, 0, binding, true).await?];
    for index in 1..history.bundles().len() {
        results.push(run_step(conn, history, index, binding, false).await?);
    }
    Ok(results)
}

pub(super) async fn run_step(
    conn: &Connection,
    history: &History,
    index: usize,
    binding: Binding,
    initialize: bool,
) -> Result<Application, Error> {
    let bundle = &history.bundles()[index];
    let previous = index.checked_sub(1).map(|i| &history.bundles()[i]);
    let transition = bundle
        .verify_api_transition(previous)
        .map_err(|e| Error::NotApplied(message(e)))?;
    let fingerprint = bundle
        .runtime_fingerprint()
        .map_err(|e| Error::NotApplied(message(e)))?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|e| Error::NotApplied(message(e)))?;
    let mut started = false;
    let result = async {
        validate_connection(&tx).await?;
        let state = installed(&tx).await?;
        if initialize {
            if state.is_some() || first(&tx, "select 1 from main.sqlite_schema where lower(substr(name, 1, 7)) != 'sqlite_' limit 1", ()).await?.is_some() {
                return Err("initialization requires an empty database".into());
            }
        } else {
            let state = state.ok_or_else(|| "target has no finalized provenance; use db init or db baseline".to_string())?;
            let position = validate_installation(history, binding, &state)?;
            if binding == Binding::Database {
                let local = super::database::DatabaseCatalog::new(&tx).history(history.namespace()).await.map_err(message)?;
                chain_position(&local, bundle.hash())?;
                chain_position(&local, &state.head)?;
            }
            let baseline = state.baseline.as_ref().map(|hash| chain_position(history, hash)).transpose()?;
            if baseline.is_some_and(|baseline| index <= baseline && baseline <= position) {
                return Ok(Application::CoveredByBaseline);
            }
            if let Some(receipt) = receipt(&tx, bundle).await? {
                if index > position {
                    return Err("migration receipt is ahead of installed history".into());
                }
                return Ok(Application::AlreadyApplied(receipt));
            }
            if bundle.predecessor() != Some(&state.head) {
                return Err(format!("unexpected predecessor for {}; target head is {}", bundle.hash(), state.head));
            }
        }

        let plan = sql::plan(bundle).map_err(message)?;
        if !plan.statements.is_empty() {
            validate_writable_schema(&tx).await?;
        }
        started = true;
        if initialize {
            tx.execute(CREATE_STATE, ()).await.map_err(message)?;
            tx.execute(CREATE_RECEIPTS, ()).await.map_err(message)?;
            tx.execute(policy::CREATE, ()).await.map_err(message)?;
            tx.execute(policy::WRITE, [policy::Policy::Preserved.as_str()]).await.map_err(message)?;
            if binding == Binding::Database {
                for (index, bundle) in history.bundles().iter().enumerate() {
                    let name = if index == 0 { "root".into() } else { bundle.hash().to_string() };
                    super::database::store(&tx, &name, bundle).await.map_err(message)?;
                }
            }
        }
        for (index, sql) in plan.statements.iter().enumerate() {
            tx.execute(sql, ()).await.map_err(|e| format!("migration.sql statement {}: {e}", index + 1))?;
        }
        for (name, sql) in plan.assertions {
            if first(&tx, sql, ()).await.map_err(|e| format!("assertion {name}: {e}"))?.is_some() {
                return Err(format!("assertion {name} returned a row"));
            }
        }
        install_runtime(&tx, bundle, binding, &fingerprint, false, !plan.statements.is_empty()).await?;
        let timestamp = first(&tx, "select unixepoch()", ()).await?.ok_or("could not obtain application time")?.get::<i64>(0).map_err(message)?;
        let update_by = if transition.from.is_some_and(|from| from != transition.to) {
            Some(i64::try_from(bundle.grace_seconds()).ok().and_then(|seconds| timestamp.checked_add(seconds)).ok_or("grace deadline overflows database timestamp")?)
        } else { None };
        tx.execute("insert into main._pyre_migration_receipts (hash, predecessor, applied_at, api_version, update_by) values (?, ?, ?, ?, ?)",
            libsql::params![bundle.hash().to_string(), bundle.predecessor().map(ToString::to_string), timestamp, bundle.target_api_version(), update_by]).await.map_err(message)?;
        Ok(Application::Applied(Receipt { applied_at: timestamp, update_by }))
    }.await;

    match result {
        Ok(outcome @ Application::Applied(_)) => {
            tx.commit()
                .await
                .map_err(|e| Error::OutcomeUnknown(format!("bundle {}: {e}", bundle.hash())))?;
            Ok(outcome)
        }
        Ok(outcome) => {
            tx.rollback()
                .await
                .map_err(|e| Error::OutcomeUnknown(message(e)))?;
            Ok(outcome)
        }
        Err(reason) => match tx.rollback().await {
            Err(e) => Err(Error::OutcomeUnknown(format!(
                "bundle {}: {reason}; rollback failed: {e}",
                bundle.hash()
            ))),
            Ok(()) if started => Err(Error::RolledBack(reason)),
            Ok(()) => Err(Error::NotApplied(reason)),
        },
    }
}

/// After an unknown outcome, the caller supplies a fresh connection. Only
/// report a safe retry when the locked target is at the expected predecessor.
pub async fn inspect_outcome(
    conn: &Connection,
    bundle: &Bundle,
    binding: Binding,
) -> Result<Inspection, Error> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|e| Error::OutcomeUnknown(message(e)))?;
    let result = async {
        validate_connection(&tx).await?;
        let Some(state) = installed(&tx).await? else {
            if bundle.predecessor().is_none() && first(&tx, "select 1 from main.sqlite_schema where lower(substr(name, 1, 7)) != 'sqlite_' limit 1", ()).await?.is_none() {
                return Ok(Inspection::NotApplied);
            }
            return Err("no matching target history".into());
        };
        if state.namespace != bundle.namespace() || state.binding != binding {
            return Err("target has a different namespace or catalog binding".into());
        }
        if let Some(receipt) = receipt(&tx, bundle).await? {
            Ok(Inspection::Applied(receipt))
        } else if bundle.predecessor() == Some(&state.head) {
            Ok(Inspection::NotApplied)
        } else {
            Err("target history diverged; do not retry".into())
        }
    }.await;
    tx.rollback()
        .await
        .map_err(|e| Error::OutcomeUnknown(message(e)))?;
    result.map_err(Error::OutcomeUnknown)
}

async fn validate_connection(conn: &Connection) -> Result<(), String> {
    if first(conn, "select 1 from temp.sqlite_schema union all select 1 from temp.pragma_database_list() where name not in ('main', 'temp') limit 1", ()).await?.is_some() {
        return Err("migration requires a dedicated connection without temp objects or attached databases".into());
    }
    Ok(())
}

async fn validate_writable_schema(conn: &Connection) -> Result<(), String> {
    // Remote libSQL cannot install an SQLite authorizer hook. Block schema
    // objects that could otherwise cause writes outside the approved SQL AST.
    if first(conn, "select 1 from main.sqlite_schema where type = 'trigger' union all select 1 from temp.pragma_table_list() where schema = 'main' and type in ('virtual', 'shadow') limit 1", ()).await?.is_some() {
        return Err("v1 migration writes do not support targets with triggers or virtual tables".into());
    }
    Ok(())
}

async fn install_runtime(
    conn: &Connection,
    bundle: &Bundle,
    binding: Binding,
    fingerprint: &str,
    baseline: bool,
    wrote_sql: bool,
) -> Result<(), String> {
    migrate::ensure_metadata_storage(conn)
        .await
        .map_err(message)?;
    conn.execute(migrate::CREATE_SYNC_TABLE, ())
        .await
        .map_err(message)?;
    conn.execute(migrate::INSERT_SYNC_STATE_ROW, ())
        .await
        .map_err(message)?;
    conn.execute(
        "delete from main._pyre_migrations where name = 'finalized:installed'",
        (),
    )
    .await
    .map_err(message)?;
    conn.execute("insert into main._pyre_migrations (name, sql, schema, schema_namespace, finished_at) values ('finalized:installed', '', ?, ?, unixepoch())",
        libsql::params![bundle.schema_source(), bundle.namespace()]).await.map_err(message)?;
    for statement in migrate::external_identity_sql(bundle.external_identities()) {
        execute_statement(conn, statement).await?;
    }
    let hash = bundle.hash().to_string();
    conn.execute("insert into main._pyre_finalized_schema (id, namespace, catalog, head, baseline, api_version, fingerprint) values (1, ?, ?, ?, ?, ?, ?) on conflict(id) do update set head = excluded.head, api_version = excluded.api_version, fingerprint = excluded.fingerprint",
        libsql::params![bundle.namespace(), binding.as_str(), hash.clone(), baseline.then_some(hash), bundle.target_api_version(), fingerprint]).await.map_err(message)?;
    if wrote_sql
        && bundle
            .load_target_schema()
            .map_err(message)?
            .database
            .schemas[0]
            .sync_mode
            == crate::ast::SyncMode::Synced
    {
        conn.execute("update main._pyre_sync set database_epoch = lower(hex(randomblob(16))), server_revision = 0 where id = 1", ()).await.map_err(message)?;
    }
    Ok(())
}

/// Adopt an existing database at an exact finalized schema without running
/// artifact SQL or creating any historical receipts.
pub async fn baseline(
    conn: &Connection,
    history: &History,
    hash: &BundleHash,
    binding: Binding,
) -> Result<(), Error> {
    let bundle = chain_position(history, hash)
        .map(|i| &history.bundles()[i])
        .map_err(Error::NotApplied)?;
    let fingerprint = bundle
        .runtime_fingerprint()
        .map_err(|e| Error::NotApplied(message(e)))?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|e| Error::NotApplied(message(e)))?;
    let result = async {
        validate_connection(&tx).await?;
        if installed(&tx).await?.is_some() || table_exists(&tx, "_pyre_migration_receipts").await? {
            return Err("target already has finalized migration history".into());
        }
        policy::read_in_transaction(&tx, Some(history.namespace()))
            .await
            .map_err(message)?;
        super::author::audit_bundle(&tx, bundle).await?;
        let catalog = super::database::DatabaseCatalog::new(&tx)
            .history(history.namespace())
            .await
            .map_err(message)?;
        if binding == Binding::Database {
            if catalog.head().map(Bundle::hash) != history.head().map(Bundle::hash) {
                return Err(
                    "target-local catalog does not match the supplied finalized history".into(),
                );
            }
        } else if !catalog.bundles().is_empty()
            || table_exists(&tx, "_pyre_migration_drafts").await?
        {
            return Err("target already contains database-catalog authoring data".into());
        }
        tx.execute(CREATE_STATE, ()).await.map_err(message)?;
        tx.execute(CREATE_RECEIPTS, ()).await.map_err(message)?;
        install_runtime(&tx, bundle, binding, &fingerprint, true, false).await
    }
    .await;
    finish_change(tx, result).await
}

pub async fn promote(
    conn: &Connection,
    history: &History,
    binding: Binding,
) -> Result<policy::Policy, Error> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|e| Error::NotApplied(message(e)))?;
    let result = async {
        validate_connection(&tx).await?;
        let (current, marked) = policy::read_in_transaction(&tx, Some(history.namespace()))
            .await
            .map_err(message)?;
        let state = installed(&tx)
            .await?
            .ok_or("promotion requires verified finalized history or an explicit baseline")?;
        let index = validate_installation(history, binding, &state)?;
        let bundle = &history.bundles()[index];
        if state.baseline.as_ref() != Some(bundle.hash()) && receipt(&tx, bundle).await?.is_none() {
            return Err("installed head has neither a matching receipt nor a baseline".into());
        }
        super::author::audit_bundle(&tx, bundle).await?;
        // A legacy/current-schema snapshot can exist alongside finalized
        // metadata. Check what the runtime would actually load.
        let loaded = crate::server::schema::load_schema_from_database(&tx)
            .await
            .map_err(message)?;
        if crate::schema_contract::runtime_fingerprint(loaded.context().map_err(message)?)
            != state.fingerprint
        {
            return Err("runtime schema does not match the finalized installation".into());
        }
        if !marked {
            tx.execute(policy::CREATE, ()).await.map_err(message)?;
        }
        if current != policy::Policy::Preserved || !marked {
            tx.execute(policy::WRITE, [policy::Policy::Preserved.as_str()])
                .await
                .map_err(message)?;
        }
        Ok(policy::Policy::Preserved)
    }
    .await;
    finish_change(tx, result).await
}

async fn finish_change<T>(tx: libsql::Transaction, result: Result<T, String>) -> Result<T, Error> {
    match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|e| Error::OutcomeUnknown(message(e)))?;
            Ok(value)
        }
        Err(reason) => {
            tx.rollback()
                .await
                .map_err(|e| Error::OutcomeUnknown(format!("{reason}; rollback failed: {e}")))?;
            Err(Error::NotApplied(reason))
        }
    }
}

pub(super) async fn execute_statement(conn: &Connection, sql: SqlAndParams) -> Result<(), String> {
    match sql {
        SqlAndParams::Sql(sql) => conn.execute(&sql, ()).await,
        SqlAndParams::SqlWithParams { sql, args } => {
            conn.execute(&sql, libsql::params_from_iter(args)).await
        }
    }
    .map_err(message)?;
    Ok(())
}
