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
        let (bundle, transition) = Bundle::finalize(
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
        .map_err(string)?;
        history.check_new_names(&bundle).map_err(string)?;
        Ok((bundle, transition))
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
    let mut retirements = BTreeMap::new();
    if let Some(previous) = history.head() {
        let old_context = previous.load_target_schema().map_err(string)?;
        crate::schema_contract::transition(Some((previous.api_version(), &old_context)), &target)
            .map_err(string)?;
        for (key, old) in &old_context.tables {
            let Some(new) = target.tables.get(key) else {
                continue;
            };
            let old_columns = ast::collect_columns(&old.record.fields);
            let new_columns = ast::collect_columns(&new.record.fields);
            let table_name = ast::get_tablename(&old.record.name, &old.record.fields);
            for column in new_columns {
                let Some(previous) = old_columns.iter().find(|c| c.name == column.name) else {
                    continue;
                };
                let before = ast::column_lifecycle(previous);
                let after = ast::column_lifecycle(&column);
                if before != after {
                    diagnostics.push(format!(
                        "{} {}.{}",
                        lifecycle_diagnostic(after),
                        old.record.name,
                        column.name
                    ));
                }
                if before != ast::Lifecycle::Retired && after == ast::Lifecycle::Retired {
                    if table_name != ast::get_tablename(&new.record.name, &new.record.fields) {
                        requires_manual_sql = true;
                    } else {
                        retirements
                            .entry(table_name.clone())
                            .or_insert_with(Vec::new)
                            .push(column.name.clone());
                    }
                }
            }
            if ast::record_lifecycle(&old.record.fields)
                != ast::record_lifecycle(&new.record.fields)
            {
                diagnostics.push(format!(
                    "{} {}",
                    lifecycle_diagnostic(ast::record_lifecycle(&new.record.fields)),
                    new.record.name
                ));
            }
        }
        for (name, (_, type_)) in &target.types {
            if let (
                crate::typecheck::Type::OneOf { variants },
                Some((_, crate::typecheck::Type::OneOf { variants: old })),
            ) = (type_, old_context.types.get(name))
            {
                for variant in variants {
                    if old
                        .iter()
                        .any(|v| v.name == variant.name && v.lifecycle != variant.lifecycle)
                    {
                        diagnostics.push(format!(
                            "{} {name}.{}",
                            lifecycle_diagnostic(variant.lifecycle),
                            variant.name
                        ));
                    }
                }
            }
        }
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
                        let retirement = target.tables.values().any(|t| {
                            t.record.name == record.name
                                && retirements
                                    .get(&ast::get_tablename(&t.record.name, &t.record.fields))
                                    .is_some_and(|cols| cols.contains(&name))
                        });
                        let manual = changes.type_changed.is_some()
                            || (!retirement
                                && (changes.nullable_changed.is_some()
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
                                        })));
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
    let mut retirement_sql = Vec::new();
    for (table, mut columns) in retirements {
        columns.sort();
        let Some(old) = previous.tables.iter().find(|t| t.name == table) else {
            continue;
        };
        let new = target
            .tables
            .values()
            .find(|t| ast::get_tablename(&t.record.name, &t.record.fields) == table)
            .unwrap();
        let layout = diff::create_table_from_fields(
            &target,
            &new.record.name,
            &new.record.fields,
            target_schema.sync_mode == ast::SyncMode::Synced,
        );
        columns.retain(|name| {
            old.columns
                .iter()
                .any(|c| &c.name == name && (c.notnull || c.default_value.is_some()))
        });
        if columns.is_empty() {
            continue;
        }
        for changes in changes
            .modified_records
            .iter_mut()
            .filter(|r| r.name == table)
        {
            changes.changes.retain(|change| match change {
                diff::RecordChange::ModifiedField { name, changes } if columns.contains(name) => {
                    changes.type_changed.is_some() || changes.primary_key_changed.is_some()
                }
                diff::RecordChange::AddedIndex(_) | diff::RecordChange::RemovedIndex(_) => false,
                _ => true,
            });
        }
        // Dropping and restoring indexes permits replacement of the physical column in
        // place. Keep the table and rowids (including any AUTOINCREMENT high-water mark).
        for index in &old.indexes {
            retirement_sql.push(format!("drop index \"{}\"", index.name));
        }
        for name in columns {
            let mut replacement = layout
                .columns
                .iter()
                .find(|c| c.name == name)
                .unwrap()
                .clone();
            let mut suffix = 0;
            replacement.name = loop {
                let candidate = format!("pyre_retirement_{suffix}");
                if !old
                    .columns
                    .iter()
                    .chain(&layout.columns)
                    .any(|c| c.name.eq_ignore_ascii_case(&candidate))
                {
                    break candidate;
                }
                suffix += 1;
            };
            retirement_sql.push(format!(
                "alter table \"{table}\" add column {}",
                diff::to_sql::column_definition(&replacement)
            ));
            retirement_sql.push(format!(
                "update \"{table}\" set \"{}\" = \"{name}\"",
                replacement.name
            ));
            retirement_sql.push(format!("alter table \"{table}\" drop column \"{name}\""));
            retirement_sql.push(format!(
                "alter table \"{table}\" rename column \"{}\" to \"{name}\"",
                replacement.name
            ));
        }
        for index in &layout.indexes {
            retirement_sql.push(diff::to_sql::render_index_sql(&table, index));
        }
    }
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
        for statement in retirement_sql {
            sql.push_str(&statement);
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

fn lifecycle_diagnostic(state: ast::Lifecycle) -> &'static str {
    match state {
        ast::Lifecycle::Active => "Restore active status for",
        ast::Lifecycle::Deprecated => "Deprecate",
        ast::Lifecycle::Retired => "Retire",
    }
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
