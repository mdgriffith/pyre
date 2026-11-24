//! Shared authoring and replay checks; catalogs supply storage and publication.

use super::{
    execute, load_standalone_schema, Bundle, BundleHash, DraftMetadata, History, MIGRATION_PATH,
    SCHEMA_PATH,
};
use crate::ast;
use crate::db::{diff, introspect};
use crate::generate::sql::to_sql::SqlAndParams;
use crate::schema_contract::ApiTransition;
use libsql::{Builder, Connection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Draft {
    pub namespace: String,
    pub predecessor: Option<BundleHash>,
    pub external_identities: Vec<ast::ExternalIdentity>,
    pub grace_seconds: Option<u64>,
    pub schema: String,
    pub sql: String,
    pub assertions: BTreeMap<String, String>,
}

impl Draft {
    pub fn candidate(&self, history: &History) -> Result<(Bundle, ApiTransition), String> {
        if self.namespace != history.namespace()
            || self.predecessor.as_ref() != history.head().map(Bundle::hash)
        {
            return Err("draft does not follow the latest namespace head; rebase it and carry forward handwritten SQL and assertions".into());
        }
        Bundle::finalize(
            DraftMetadata {
                namespace: self.namespace.clone(),
                grace_seconds: self.grace_seconds,
                external_identities: self.external_identities.clone(),
            },
            history.head(),
            [
                (MIGRATION_PATH.into(), self.sql.as_bytes().to_vec()),
                (SCHEMA_PATH.into(), self.schema.as_bytes().to_vec()),
            ]
            .into_iter()
            .chain(
                self.assertions
                    .iter()
                    .map(|(name, sql)| (format!("assertions/{name}"), sql.as_bytes().to_vec())),
            ),
        )
        .map_err(string)
    }
}

#[derive(Debug)]
pub struct GeneratedDraft {
    pub draft: Draft,
    pub diagnostics: Vec<String>,
    pub requires_manual_sql: bool,
}

/// Bootstrap a fresh preserved database from the supplied standalone working
/// schema, storing the checked root in the target-local catalog atomically.
pub async fn bootstrap_database_catalog(
    conn: &Connection,
    namespace: &str,
    source: String,
    identities: Vec<ast::ExternalIdentity>,
) -> Result<Vec<execute::Application>, execute::Error> {
    let not_applied = |e: String| execute::Error::NotApplied(e);
    let empty = History::new(namespace, []).map_err(|e| not_applied(string(e)))?;
    let generated = create_draft(&empty, source, identities, None)
        .await
        .map_err(not_applied)?;
    let (root, _) = generated.draft.candidate(&empty).map_err(not_applied)?;
    check(&empty, Some(&root), None)
        .await
        .map_err(not_applied)?;
    let history = History::new(namespace, [root]).map_err(|e| not_applied(string(e)))?;
    execute::initialize(conn, &history, execute::Binding::Database).await
}

fn string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn memory() -> Result<Connection, String> {
    Builder::new_local(":memory:")
        .build()
        .await
        .map_err(string)?
        .connect()
        .map_err(string)
}

pub async fn create_draft(
    history: &History,
    schema: String,
    identities: Vec<ast::ExternalIdentity>,
    grace_seconds: Option<u64>,
) -> Result<GeneratedDraft, String> {
    let namespace = history.namespace();
    let target = load_standalone_schema(namespace, &schema, &identities).map_err(string)?;
    let target_schema = &target.database.schemas[0];
    // Generate against the predecessor's declared physical layout. The check
    // below uses actual replay and catches both historical and generator gaps.
    let predecessor = memory().await?;
    let mut diagnostics = Vec::new();
    let mut requires_manual_sql = false;
    if let Some(previous) = history.head() {
        let old_context = previous.load_target_schema().map_err(string)?;
        materialize(&predecessor, &old_context).await?;
        let logical = ast::diff::diff_schema(&old_context.database.schemas[0], target_schema);
        for definition in logical.added.iter().chain(&logical.removed) {
            let (kind, name) = match definition {
                ast::Definition::Record { name, .. } => ("record", name),
                ast::Definition::Tagged { name, .. } => ("union", name),
                _ => continue,
            };
            let removed = logical.removed.iter().any(|other| other == definition);
            diagnostics.push(format!(
                "{} {kind} {name}",
                if removed { "Remove" } else { "Add" }
            ));
            requires_manual_sql |= removed;
        }
        for record in logical.modified_records {
            for change in record.changes {
                use ast::diff::RecordChange::*;
                diagnostics.push(match change {
                    AddedField(field) => format!("Add column {}.{}", record.name, field.name),
                    RemovedField(field) => {
                        requires_manual_sql = true;
                        format!(
                            "Remove column {}.{} (explicit SQL required)",
                            record.name, field.name
                        )
                    }
                    ModifiedField { name, changes } => {
                        let manual = changes.type_changed.is_some()
                            || changes.nullable_changed.is_some()
                            || changes
                                .added_directives
                                .iter()
                                .chain(&changes.removed_directives)
                                .any(|d| {
                                    matches!(
                                        d,
                                        ast::ColumnDirective::PrimaryKey
                                            | ast::ColumnDirective::Default { .. }
                                            | ast::ColumnDirective::CreatedAt
                                            | ast::ColumnDirective::UpdatedAt
                                    )
                                });
                        requires_manual_sql |= manual;
                        format!(
                            "Change column {}.{}{}",
                            record.name,
                            name,
                            if manual {
                                " (explicit SQL required)"
                            } else {
                                ""
                            }
                        )
                    }
                });
            }
        }
        for tagged in logical.modified_taggeds {
            for change in tagged.changes {
                use ast::diff::TaggedChange::*;
                match change {
                    AddedVariant(variant) => diagnostics.push(format!(
                        "Add union variant {}.{}",
                        tagged.name, variant.name
                    )),
                    RemovedVariant(variant) => {
                        requires_manual_sql = true;
                        diagnostics.push(format!(
                            "Remove union variant {}.{} (explicit SQL required)",
                            tagged.name, variant.name
                        ));
                    }
                    ModifiedVariant { name, .. } => {
                        diagnostics.push(format!("Change union variant {}.{}", tagged.name, name))
                    }
                }
            }
        }
    }
    let previous = uninitialized_introspection(&predecessor).await?;
    let mut changes = diff::diff(&target, target_schema, &previous);
    changes.added.sort_by(|a, b| a.name.cmp(&b.name));
    changes.modified_records.sort_by(|a, b| a.name.cmp(&b.name));
    requires_manual_sql |= !changes.removed.is_empty();
    changes.removed.clear();
    for record in &mut changes.modified_records {
        record.changes.retain(|change| {
            use diff::RecordChange::*;
            match change {
                RemovedField(_) | ModifiedField { .. } => {
                    requires_manual_sql = true;
                    false
                }
                AddedField(field)
                    if field.pk
                        || (field.notnull && field.default_value.is_none())
                        || field
                            .default_value
                            .as_ref()
                            .is_some_and(|value| value.contains("unixepoch(")) =>
                {
                    requires_manual_sql = true;
                    diagnostics.push(format!(
                        "Populate column {}.{} with explicit SQL",
                        record.name, field.name
                    ));
                    false
                }
                _ => true,
            }
        });
    }
    // A table or ID rename can otherwise look like an independent addition.
    // Leave the whole transformation to the author when any part is manual.
    diagnostics.sort();
    diagnostics.dedup();
    let mut sql = String::new();
    for diagnostic in &diagnostics {
        sql.push_str(&format!("-- {diagnostic}\n"));
    }
    if requires_manual_sql {
        sql.push_str(
            "-- Write the value-preserving transformation below, then run migration check.\n",
        );
    } else {
        for statement in diff::to_sql::to_sql(&changes) {
            let SqlAndParams::Sql(text) = statement else {
                return Err("generated migration unexpectedly requires parameters".into());
            };
            sql.push_str(&text);
            sql.push_str(";\n");
        }
    }
    Ok(GeneratedDraft {
        draft: Draft {
            namespace: namespace.into(),
            predecessor: history.head().map(|bundle| bundle.hash().clone()),
            external_identities: identities,
            grace_seconds,
            schema,
            sql,
            assertions: BTreeMap::new(),
        },
        diagnostics,
        requires_manual_sql,
    })
}

pub fn parse_grace(value: &str) -> Result<u64, String> {
    let invalid = || "grace must be a positive whole number followed by m, h, or d".to_string();
    let unit = match value.as_bytes().last() {
        Some(b'm') => 60,
        Some(b'h') => 3600,
        Some(b'd') => 86400,
        _ => return Err(invalid()),
    };
    let digits = &value[..value.len() - 1];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    digits
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(unit))
        .ok_or_else(invalid)
}

pub async fn check(
    history: &History,
    candidate: Option<&Bundle>,
    fixtures: Option<&serde_json::value::RawValue>,
) -> Result<(), String> {
    let target = memory().await?;
    for (index, bundle) in history.bundles().iter().enumerate() {
        execute::run_step(
            &target,
            history,
            index,
            execute::Binding::Filesystem,
            index == 0,
        )
        .await
        .map_err(string)?;
        audit_bundle(&target, bundle).await?;
    }
    if let Some(input) = fixtures {
        if candidate.is_none() || history.head().is_none() {
            return Err("fixtures require a draft and a finalized predecessor".into());
        }
        crate::server::seed::seed_for_namespace(&target, Some(history.namespace()), input)
            .await
            .map_err(string)?;
    }
    if let Some(bundle) = candidate {
        let extended = History::new(
            history.namespace(),
            history.bundles().iter().cloned().chain([bundle.clone()]),
        )
        .map_err(string)?;
        let index = history.bundles().len();
        execute::run_step(
            &target,
            &extended,
            index,
            execute::Binding::Filesystem,
            index == 0,
        )
        .await
        .map_err(string)?;
        audit_bundle(&target, bundle).await?;
    }
    Ok(())
}

async fn uninitialized_introspection(
    conn: &Connection,
) -> Result<introspect::Introspection, String> {
    let row = conn
        .query(introspect::INTROSPECT_UNINITIALIZED_SQL, ())
        .await
        .map_err(string)?
        .next()
        .await
        .map_err(string)?
        .ok_or("no introspection result")?;
    let raw = serde_json::from_str(&row.get::<String>(0).map_err(string)?).map_err(string)?;
    Ok(introspect::from_raw(raw))
}

async fn materialize(conn: &Connection, context: &crate::typecheck::Context) -> Result<(), String> {
    let layout = diff::diff(
        context,
        &context.database.schemas[0],
        &uninitialized_introspection(conn).await?,
    );
    for sql in diff::to_sql::to_sql(&layout) {
        execute::execute_statement(conn, sql).await?;
    }
    Ok(())
}

pub async fn audit_bundle(conn: &Connection, bundle: &Bundle) -> Result<(), String> {
    let context = bundle.load_target_schema().map_err(string)?;
    audit_schema(conn, &context).await
}

pub async fn audit_schema(
    conn: &Connection,
    context: &crate::typecheck::Context,
) -> Result<(), String> {
    let mut rows = conn
        .query(super::structure::READ_STRUCTURE, ())
        .await
        .map_err(string)?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await.map_err(string)? {
        actual.push((
            row.get(0).map_err(string)?,
            row.get(1).map_err(string)?,
            row.get(2).map_err(string)?,
        ));
    }
    super::structure::audit(context, actual)
}
