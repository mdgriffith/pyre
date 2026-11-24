//! Transaction plans shared by native and WASM hosts.

use super::{sql, Bundle};
use crate::db::{migrate, policy};
use crate::generate::sql::to_sql::SqlAndParams;
use serde::Serialize;
use std::str::FromStr;

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

pub const STATE: &str = "_pyre_finalized_schema";
pub const CREATE_STATE: &str = "create table main._pyre_finalized_schema (
    id integer primary key check (id = 1), namespace text not null,
    catalog text not null check (catalog in ('filesystem', 'database')),
    head text not null, baseline text, api_version text not null, fingerprint text not null
)";
pub const CREATE_RECEIPTS: &str = "create table main._pyre_migration_receipts (
    hash text primary key not null, predecessor text, applied_at integer not null,
    api_version text not null, update_by integer
)";
pub const CREATE_BUNDLES: &str = "create table if not exists main._pyre_migration_bundles (
    namespace text not null, name text not null, manifest text not null, files text not null,
    primary key (namespace, name)
)";
pub const READ_STATE: &str = "select namespace, catalog, head, baseline, api_version, fingerprint from main._pyre_finalized_schema where id = 1";
pub const READ_BUNDLES: &str = "select name, manifest, files from main._pyre_migration_bundles where namespace = ? order by name";
pub const READ_RECEIPT: &str = "select predecessor, applied_at, api_version, update_by from main._pyre_migration_receipts where hash = ?";
pub const INSERT_RECEIPT: &str = "insert into main._pyre_migration_receipts (hash, predecessor, applied_at, api_version, update_by) values (?, ?, ?, ?, ?)";
pub const TABLE_EXISTS: &str = "select 1 from main.sqlite_schema where type = 'table' and name = ?";
pub const NONEMPTY: &str =
    "select 1 from main.sqlite_schema where lower(substr(name, 1, 7)) != 'sqlite_' limit 1";
pub const UNSAFE_CONNECTION: &str = "select 1 from temp.sqlite_schema union all select 1 from temp.pragma_database_list() where name not in ('main', 'temp') limit 1";
pub const UNSAFE_WRITES: &str = "select 1 from main.sqlite_schema where type = 'trigger' union all select 1 from temp.pragma_table_list() where schema = 'main' and type in ('virtual', 'shadow') limit 1";
pub const HAS_METADATA_NAMESPACE: &str = "select name from temp.pragma_table_info('_pyre_migrations', 'main') where name = 'schema_namespace'";
pub const ADD_METADATA_NAMESPACE: &str =
    "alter table main._pyre_migrations add column schema_namespace text";

pub fn preserved_policy() -> SqlAndParams {
    statement(
        policy::WRITE,
        vec![policy::Policy::Preserved.as_str().into()],
    )
}

pub fn store_bundle(name: &str, bundle: &Bundle) -> SqlAndParams {
    statement("insert into main._pyre_migration_bundles(namespace, name, manifest, files) values (?, ?, ?, ?)", vec![
        bundle.namespace().into(), name.into(),
        String::from_utf8(bundle.manifest_json()).expect("manifest is UTF-8"),
        serde_json::to_string(bundle.files()).expect("bundle files serialize"),
    ])
}

fn statement(sql: &str, args: Vec<String>) -> SqlAndParams {
    SqlAndParams::SqlWithParams {
        sql: sql.into(),
        args,
    }
}

pub fn install_runtime(
    bundle: &Bundle,
    binding: Binding,
    fingerprint: &str,
    baseline: bool,
    invalidate_sync: bool,
) -> Vec<SqlAndParams> {
    let mut statements = vec![
        SqlAndParams::Sql(migrate::CREATE_SYNC_TABLE.into()),
        SqlAndParams::Sql(migrate::INSERT_SYNC_STATE_ROW.into()),
        SqlAndParams::Sql("delete from main._pyre_migrations where name = 'finalized:installed'".into()),
        statement("insert into main._pyre_migrations (name, sql, schema, schema_namespace, finished_at) values ('finalized:installed', '', ?, ?, unixepoch())",
            vec![bundle.schema_source().into(), bundle.namespace().into()]),
    ];
    statements.extend(migrate::external_identity_sql(bundle.external_identities()));
    statements.push(statement("insert into main._pyre_finalized_schema (id, namespace, catalog, head, baseline, api_version, fingerprint) values (1, ?, ?, ?, nullif(?, ''), ?, ?) on conflict(id) do update set head = excluded.head, api_version = excluded.api_version, fingerprint = excluded.fingerprint",
        vec![bundle.namespace().into(), binding.as_str().into(), bundle.hash().to_string(),
            if baseline { bundle.hash().to_string() } else { String::new() }, bundle.target_api_version().into(), fingerprint.into()]));
    if invalidate_sync {
        statements.push(SqlAndParams::Sql("update main._pyre_sync set database_epoch = lower(hex(randomblob(16))), server_revision = 0 where id = 1".into()));
    }
    statements
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step<'a> {
    pub hash: &'a super::BundleHash,
    pub predecessor: Option<&'a super::BundleHash>,
    pub api_version: &'a str,
    pub schema: &'a str,
    pub identities: &'a [crate::ast::ExternalIdentity],
    pub fingerprint: String,
    /// Decimal text preserves all 64 bits across the WASM boundary.
    pub grace_seconds: Option<String>,
    pub statements: Vec<&'a str>,
    pub assertions: Vec<(&'a str, &'a str)>,
    pub install: Vec<SqlAndParams>,
}

pub fn step<'a>(
    bundle: &'a Bundle,
    previous: Option<&Bundle>,
    binding: Binding,
) -> Result<Step<'a>, String> {
    let transition = bundle
        .verify_api_transition(previous)
        .map_err(|e| e.to_string())?;
    let fingerprint = bundle.runtime_fingerprint().map_err(|e| e.to_string())?;
    let sql = sql::plan(bundle).map_err(|e| e.to_string())?;
    let invalidate_sync = !sql.statements.is_empty()
        && bundle
            .load_target_schema()
            .map_err(|e| e.to_string())?
            .database
            .schemas[0]
            .sync_mode
            == crate::ast::SyncMode::Synced;
    Ok(Step {
        hash: bundle.hash(),
        predecessor: bundle.predecessor(),
        api_version: bundle.target_api_version(),
        schema: bundle.schema_source(),
        identities: bundle.external_identities(),
        install: install_runtime(bundle, binding, &fingerprint, false, invalidate_sync),
        fingerprint,
        grace_seconds: transition
            .from
            .filter(|from| *from != transition.to)
            .map(|_| bundle.grace_seconds().to_string()),
        statements: sql.statements,
        assertions: sql.assertions,
    })
}
