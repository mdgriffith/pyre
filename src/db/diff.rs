use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod to_sql;
// Define a type to represent the diff of two schemas
#[derive(Debug, Serialize, Deserialize)]
pub struct Diff {
    pub added: Vec<crate::db::introspect::Table>,
    pub removed: Vec<crate::db::introspect::Table>,
    pub modified_records: Vec<DetailedRecordDiff>,
}

pub fn is_empty(diff: &Diff) -> bool {
    diff.added.is_empty() && diff.removed.is_empty() && diff.modified_records.is_empty()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DetailedRecordDiff {
    pub name: String,
    pub changes: Vec<RecordChange>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RecordChange {
    AddedField(crate::db::introspect::ColumnInfo),
    RemovedField(crate::db::introspect::ColumnInfo),
    ModifiedField { name: String, changes: ColumnDiff },
    AddedIndex(crate::db::introspect::IndexInfo),
    RemovedIndex(crate::db::introspect::IndexInfo),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ColumnDiff {
    pub type_changed: Option<(String, String)>, // (old_type, new_type)
    pub nullable_changed: Option<(bool, bool)>, // (old_nullable, new_nullable)
}

pub fn diff(
    context: &crate::typecheck::Context,
    schema: &crate::ast::Schema,
    introspection: &crate::db::introspect::Introspection,
) -> Diff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified_records = Vec::new();

    // Create lookup maps for faster comparison - need to extract tables from all schema files
    let mut schema_tables: std::collections::HashMap<_, _> = HashMap::new();
    for file in &schema.files {
        for def in &file.definitions {
            if let crate::ast::Definition::Record { name, fields, .. } = def {
                let table_name = crate::ast::get_tablename(name, fields);
                schema_tables.insert(table_name, (name, fields));
            }
        }
    }

    let intro_tables: std::collections::HashMap<_, _> =
        introspection.tables.iter().map(|t| (&t.name, t)).collect();

    // Build a lookup map for context tables by table name (O(m) instead of O(n*m))
    let context_table_map: std::collections::HashMap<_, _> = context
        .tables
        .iter()
        .filter(|(_, table)| table.schema == schema.namespace)
        .map(|(_, table)| {
            let context_table_name =
                crate::ast::get_tablename(&table.record.name, &table.record.fields);
            (
                context_table_name,
                (table.record.name.as_str(), &table.record.fields),
            )
        })
        .collect();

    // Find added and modified tables
    for (table_name, (record_name, schema_fields)) in &schema_tables {
        // Use fields from context if available (includes updatedAt), otherwise fall back to schema fields
        let (record_name_to_use, fields_to_use) = match context_table_map.get(table_name) {
            Some((ctx_record_name, ctx_fields)) => (*ctx_record_name, *ctx_fields),
            None => (record_name.as_str(), *schema_fields),
        };

        match intro_tables.get(table_name) {
            None => {
                let table = create_table_from_fields(context, record_name_to_use, fields_to_use);
                added.push(table);
            }
            Some(intro_table) => {
                let schema_table =
                    create_table_from_fields(context, record_name_to_use, fields_to_use);
                if let Some(record_diff) = compare_record(
                    context,
                    &schema_table,
                    intro_table,
                    schema_fields,
                    introspection,
                ) {
                    modified_records.push(record_diff);
                }
            }
        }
    }

    // Find removed tables
    for (table_name, intro_table) in intro_tables {
        if !schema_tables.contains_key(table_name) {
            removed.push((*intro_table).clone());
        }
    }

    Diff {
        added,
        removed,
        modified_records,
    }
}

fn default_value_to_sql(default: &crate::ast::DefaultValue) -> Option<String> {
    match default {
        crate::ast::DefaultValue::Now => Some("unixepoch()".to_string()),
        crate::ast::DefaultValue::Value(value) => match value {
            crate::ast::QueryValue::String((_, s)) => Some(format!("'{}'", s)),
            crate::ast::QueryValue::Int((_, i)) => Some(i.to_string()),
            crate::ast::QueryValue::Float((_, f)) => Some(f.to_string()),
            crate::ast::QueryValue::Bool((_, b)) => Some(if *b { "1" } else { "0" }.to_string()),
            crate::ast::QueryValue::Null(_) => Some("null".to_string()),
            _ => None, // Other types not supported as defaults
        },
    }
}

fn add_fields(
    context: &crate::typecheck::Context,
    fields: &Vec<crate::ast::Field>,
    columns: &mut Vec<crate::db::introspect::ColumnInfo>,
    field_namespace: Option<String>,
    seen_fields: &mut std::collections::HashSet<String>,
    force_nullable: bool,
) {
    for f in fields {
        if let crate::ast::Field::Column(col) = f {
            let column_name = if let Some(namespace) = &field_namespace {
                format!("{}__{}", namespace, col.name)
            } else {
                col.name.clone()
            };

            if seen_fields.contains(&column_name) {
                continue;
            }
            seen_fields.insert(column_name.clone());

            let default_value = if crate::ast::is_managed_timestamp(col) {
                Some("(unixepoch())".to_string())
            } else {
                col.directives.iter().find_map(|d| match d {
                    crate::ast::ColumnDirective::Default { value, .. } => {
                        return default_value_to_sql(value).map(|sql| format!("({})", sql));
                    }
                    _ => None,
                })
            };

            match col.type_.to_serialization_type() {
                crate::ast::SerializationType::Concrete(concrete) => {
                    columns.push(crate::db::introspect::ColumnInfo {
                        cid: 0,
                        name: column_name,
                        column_type: concrete.to_sql_type(),
                        notnull: !force_nullable && !col.nullable,
                        default_value,
                        pk: col
                            .directives
                            .iter()
                            .any(|d| matches!(d, crate::ast::ColumnDirective::PrimaryKey)),
                        indexed: col
                            .directives
                            .iter()
                            .any(|d| matches!(d, crate::ast::ColumnDirective::Index)),
                    });
                }
                crate::ast::SerializationType::FromType(typename) => {
                    if let Some((_, type_)) = context.types.get(&typename) {
                        match type_ {
                            crate::typecheck::Type::OneOf { variants } => {
                                columns.push(crate::db::introspect::ColumnInfo {
                                    cid: 0,
                                    name: column_name.clone(),
                                    column_type: crate::ast::ConcreteSerializationType::Text
                                        .to_sql_type(),
                                    notnull: !force_nullable && !col.nullable,
                                    default_value: None,
                                    pk: false,
                                    indexed: col
                                        .directives
                                        .iter()
                                        .any(|d| matches!(d, crate::ast::ColumnDirective::Index)),
                                });

                                for variant in variants {
                                    if let Some(var_fields) = &variant.fields {
                                        // Pass true for force_nullable for variant fields
                                        add_fields(
                                            context,
                                            var_fields,
                                            columns,
                                            Some(column_name.clone()),
                                            seen_fields,
                                            true, // Force nullable for variant fields
                                        );
                                    }
                                }
                            }
                            _ => {
                                columns.push(crate::db::introspect::ColumnInfo {
                                    cid: 0,
                                    name: column_name,
                                    column_type: typename.clone(),
                                    notnull: !force_nullable && !col.nullable,
                                    default_value: None,
                                    pk: col.directives.iter().any(|d| {
                                        matches!(d, crate::ast::ColumnDirective::PrimaryKey)
                                    }),
                                    indexed: col
                                        .directives
                                        .iter()
                                        .any(|d| matches!(d, crate::ast::ColumnDirective::Index)),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_schema(namespace: &str, source: &str) -> crate::ast::Schema {
        let mut schema = crate::ast::Schema {
            namespace: namespace.to_string(),
            sync_mode: crate::ast::SyncMode::Synced,
            session: None,
            files: vec![],
        };

        crate::parser::run("schema.pyre", source, &mut schema).expect("schema parses");
        schema
    }

    #[test]
    fn unsupported_default_values_do_not_panic_or_emit_invalid_sql_default() {
        let fn_default =
            crate::ast::DefaultValue::Value(crate::ast::QueryValue::Fn(crate::ast::FnDetails {
                name: "custom".to_string(),
                args: vec![],
                location: crate::ast::empty_range(),
                location_fn_name: crate::ast::empty_range(),
                location_arg: crate::ast::empty_range(),
            }));

        let column = crate::ast::Column {
            name: "status".to_string(),
            type_: crate::ast::ColumnType::String,
            nullable: false,
            directives: vec![crate::ast::ColumnDirective::Default {
                id: "default".to_string(),
                value: fn_default,
                start: None,
                end: None,
            }],
            start: None,
            end: None,
            start_name: None,
            end_name: None,
            start_typename: None,
            end_typename: None,
            inline_comment: None,
        };

        let fields = vec![crate::ast::Field::Column(column)];
        let mut columns = Vec::new();
        let mut seen_fields = std::collections::HashSet::new();
        let context = crate::typecheck::empty_context();

        add_fields(
            &context,
            &fields,
            &mut columns,
            None,
            &mut seen_fields,
            false,
        );

        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].default_value, None);
    }

    #[test]
    fn diff_uses_context_tables_from_selected_schema_namespace() {
        let main_schema = parse_schema(
            "Main",
            r#"
record MainRecord {
    @public
    @tablename("shared_table")
    id       Id.Int @id
    mainOnly String
}
"#,
        );
        let campaign_schema = parse_schema(
            "Campaign",
            r#"
record CampaignRecord {
    @public
    @tablename("shared_table")
    id           Id.Int @id
    campaignOnly String
}
"#,
        );
        let database = crate::ast::Database {
            schemas: vec![main_schema.clone(), campaign_schema],
        };
        let context = crate::typecheck::check_schema(&database).expect("database typechecks");
        let introspection = crate::db::introspect::Introspection {
            tables: vec![],
            migration_state: crate::db::introspect::MigrationState::NoMigrationTable,
            schema: crate::db::introspect::SchemaResult::Success {
                schema: crate::ast::Schema::default(),
                context: crate::typecheck::empty_context(),
            },
        };

        let diff = super::diff(&context, &main_schema, &introspection);

        assert_eq!(diff.added.len(), 1);
        assert!(diff.added[0]
            .columns
            .iter()
            .any(|column| column.name == "mainOnly"));
        assert!(!diff.added[0]
            .columns
            .iter()
            .any(|column| column.name == "campaignOnly"));
    }

    fn uuid_foreign_key_schema() -> (crate::ast::Schema, crate::typecheck::Context) {
        let schema = parse_schema(
            "Main",
            r#"
record ClocktowerGame {
    @public
    id Id.Uuid @id
}

record ClocktowerParticipant {
    @public
    id Id.Int @id
    gameId ClocktowerGame.id
}
"#,
        );
        let database = crate::ast::Database {
            schemas: vec![schema.clone()],
        };
        let context = crate::typecheck::check_schema(&database).expect("database typechecks");
        (schema, context)
    }

    #[test]
    fn uuid_foreign_key_ddl_uses_text() {
        let (schema, context) = uuid_foreign_key_schema();
        let introspection = crate::db::introspect::Introspection {
            tables: vec![],
            migration_state: crate::db::introspect::MigrationState::NoMigrationTable,
            schema: crate::db::introspect::SchemaResult::Success {
                schema: crate::ast::Schema::default(),
                context: crate::typecheck::empty_context(),
            },
        };

        let diff = super::diff(&context, &schema, &introspection);
        let sql = crate::db::diff::to_sql::to_sql(&diff);
        assert!(sql.iter().any(|statement| matches!(
            statement,
            crate::generate::sql::to_sql::SqlAndParams::Sql(sql)
                if sql.contains("`gameId` TEXT")
        )));
    }

    #[test]
    fn uuid_foreign_key_text_column_does_not_diff_as_integer() {
        let (schema, context) = uuid_foreign_key_schema();
        let introspection = crate::db::introspect::Introspection {
            tables: vec![crate::db::introspect::Table {
                name: "clocktower_participant".to_string(),
                columns: vec![
                    crate::db::introspect::ColumnInfo {
                        cid: 0,
                        name: "id".to_string(),
                        column_type: "INTEGER".to_string(),
                        notnull: true,
                        default_value: None,
                        pk: true,
                        indexed: false,
                    },
                    crate::db::introspect::ColumnInfo {
                        cid: 1,
                        name: "gameId".to_string(),
                        column_type: "TEXT".to_string(),
                        notnull: true,
                        default_value: None,
                        pk: false,
                        indexed: false,
                    },
                ],
                foreign_keys: vec![],
                indexes: vec![],
            }],
            migration_state: crate::db::introspect::MigrationState::NoMigrationTable,
            schema: crate::db::introspect::SchemaResult::Success {
                schema: crate::ast::Schema::default(),
                context: crate::typecheck::empty_context(),
            },
        };

        let diff = super::diff(&context, &schema, &introspection);
        assert!(diff.modified_records.is_empty());
    }
}

fn create_table_from_fields(
    context: &crate::typecheck::Context,
    name: &str,
    fields: &Vec<crate::ast::Field>,
) -> crate::db::introspect::Table {
    let mut columns = Vec::new();
    add_fields(
        context,
        fields,
        &mut columns,
        None,
        &mut std::collections::HashSet::new(),
        false,
    );

    let table_name = crate::ast::get_tablename(name, fields);

    let mut indexes: Vec<crate::db::introspect::IndexInfo> = crate::ast::collect_indexes(fields)
        .iter()
        .map(|idx| materialized_index_to_index_info(&table_name, idx))
        .collect();

    for field in fields {
        if let crate::ast::Field::Column(column) = field {
            for directive in &column.directives {
                match directive {
                    crate::ast::ColumnDirective::Index => {
                        indexes.push(crate::db::introspect::IndexInfo {
                            name: format!("idx_{}_{}", table_name, column.name),
                            unique: false,
                            columns: vec![crate::db::introspect::IndexedColumnInfo {
                                name: column.name.clone(),
                                desc: false,
                            }],
                            where_clause: None,
                        })
                    }
                    crate::ast::ColumnDirective::Unique => {
                        indexes.push(crate::db::introspect::IndexInfo {
                            name: format!("uniq_{}_{}", table_name, column.name),
                            unique: true,
                            columns: vec![crate::db::introspect::IndexedColumnInfo {
                                name: column.name.clone(),
                                desc: false,
                            }],
                            where_clause: None,
                        })
                    }
                    _ => {}
                }
            }
        }
    }

    crate::db::introspect::Table {
        name: table_name,
        columns,
        foreign_keys: vec![],
        indexes,
    }
}

fn materialized_index_to_index_info(
    table_name: &str,
    idx: &crate::ast::MaterializedIndex,
) -> crate::db::introspect::IndexInfo {
    let prefix = if idx.unique { "uniq" } else { "idx" };
    let columns_for_name = idx
        .columns
        .iter()
        .map(|c| match c.direction {
            crate::ast::SortDirection::Asc => c.name.clone(),
            crate::ast::SortDirection::Desc => format!("{}_desc", c.name),
        })
        .collect::<Vec<String>>()
        .join("_");

    let where_fragment = idx
        .where_
        .as_ref()
        .map(where_arg_to_sql)
        .map(|w| format!("_{}", w.replace(' ', "_").replace('"', "")))
        .unwrap_or_else(|| "".to_string());

    crate::db::introspect::IndexInfo {
        name: format!(
            "{}_{}_{}{}",
            prefix, table_name, columns_for_name, where_fragment
        ),
        unique: idx.unique,
        columns: idx
            .columns
            .iter()
            .map(|c| crate::db::introspect::IndexedColumnInfo {
                name: c.name.clone(),
                desc: matches!(c.direction, crate::ast::SortDirection::Desc),
            })
            .collect(),
        where_clause: idx.where_.as_ref().map(where_arg_to_sql),
    }
}

fn query_value_to_sql(value: &crate::ast::QueryValue) -> String {
    match value {
        crate::ast::QueryValue::String((_, s)) => format!("'{}'", s.replace("'", "''")),
        crate::ast::QueryValue::Int((_, i)) => i.to_string(),
        crate::ast::QueryValue::Float((_, f)) => f.to_string(),
        crate::ast::QueryValue::Bool((_, b)) => {
            if *b {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        crate::ast::QueryValue::Null(_) => "null".to_string(),
        crate::ast::QueryValue::LiteralTypeValue((_, details)) => {
            format!("'{}'", details.name.replace("'", "''"))
        }
        crate::ast::QueryValue::Fn(_) | crate::ast::QueryValue::Variable(_) => "null".to_string(),
    }
}

fn where_arg_to_sql(where_arg: &crate::ast::WhereArg) -> String {
    match where_arg {
        crate::ast::WhereArg::Exists(..) => "0".to_string(),
        crate::ast::WhereArg::Column(_is_session_var, field, op, value, _) => {
            let quoted = format!("\"{}\"", field);
            match (op, value) {
                (crate::ast::Operator::Equal, crate::ast::QueryValue::Null(_)) => {
                    format!("{} is null", quoted)
                }
                (crate::ast::Operator::NotEqual, crate::ast::QueryValue::Null(_)) => {
                    format!("{} is not null", quoted)
                }
                _ => {
                    let op_text = match op {
                        crate::ast::Operator::Equal => "=",
                        crate::ast::Operator::NotEqual => "!=",
                        crate::ast::Operator::GreaterThan => ">",
                        crate::ast::Operator::LessThan => "<",
                        crate::ast::Operator::GreaterThanOrEqual => ">=",
                        crate::ast::Operator::LessThanOrEqual => "<=",
                        crate::ast::Operator::In => "in",
                        crate::ast::Operator::NotIn => "not in",
                        crate::ast::Operator::Like => "like",
                        crate::ast::Operator::NotLike => "not like",
                    };
                    format!("{} {} {}", quoted, op_text, query_value_to_sql(value))
                }
            }
        }
        crate::ast::WhereArg::And(items) => format!(
            "({})",
            items
                .iter()
                .map(where_arg_to_sql)
                .collect::<Vec<String>>()
                .join(" and ")
        ),
        crate::ast::WhereArg::Or(items) => format!(
            "({})",
            items
                .iter()
                .map(where_arg_to_sql)
                .collect::<Vec<String>>()
                .join(" or ")
        ),
    }
}

// Helper function to compare record fields
fn compare_record(
    _context: &crate::typecheck::Context,
    schema_table: &crate::db::introspect::Table,
    intro_table: &crate::db::introspect::Table,
    schema_fields: &Vec<crate::ast::Field>,
    introspection: &crate::db::introspect::Introspection,
) -> Option<DetailedRecordDiff> {
    let mut changes = Vec::new();

    let schema_columns: std::collections::HashMap<_, _> =
        schema_table.columns.iter().map(|c| (&c.name, c)).collect();

    let intro_columns: std::collections::HashMap<_, _> =
        intro_table.columns.iter().map(|c| (&c.name, c)).collect();

    // Find added and modified columns
    for (name, schema_col) in &schema_columns {
        match intro_columns.get(name) {
            None => changes.push(RecordChange::AddedField((*schema_col).clone())),
            Some(intro_col) => {
                let type_changed = if schema_col.column_type != intro_col.column_type {
                    Some((
                        intro_col.column_type.clone(),
                        schema_col.column_type.clone(),
                    ))
                } else {
                    None
                };

                let nullable_changed = if schema_col.notnull != intro_col.notnull {
                    Some((intro_col.notnull, schema_col.notnull))
                } else {
                    None
                };

                if type_changed.is_some() || nullable_changed.is_some() {
                    changes.push(RecordChange::ModifiedField {
                        name: name.to_string(),
                        changes: ColumnDiff {
                            type_changed,
                            nullable_changed,
                        },
                    });
                }
            }
        }
    }

    // Find removed columns
    for name in intro_columns.keys() {
        if !schema_columns.contains_key(name) {
            changes.push(RecordChange::RemovedField(intro_columns[name].clone()));
        }
    }

    // Compare relationships - relationships don't create columns, so we need to compare them separately
    let schema_indexes: std::collections::HashMap<_, _> = schema_table
        .indexes
        .iter()
        .map(|idx| (index_signature(idx), idx))
        .collect();

    let intro_indexes_source: Vec<crate::db::introspect::IndexInfo> =
        if !intro_table.indexes.is_empty() {
            intro_table.indexes.clone()
        } else {
            indexes_from_schema_result(&schema_table.name, introspection)
        };

    let intro_indexes: std::collections::HashMap<_, _> = intro_indexes_source
        .iter()
        .map(|idx| (index_signature(idx), idx))
        .collect();

    for (signature, schema_index) in &schema_indexes {
        if !intro_indexes.contains_key(signature) {
            changes.push(RecordChange::AddedIndex((*schema_index).clone()));
        }
    }

    for (signature, intro_index) in &intro_indexes {
        if !schema_indexes.contains_key(signature) {
            changes.push(RecordChange::RemovedIndex((*intro_index).clone()));
        }
    }

    // Compare relationships - relationships don't create columns, so we need to compare them separately
    // Get relationships from schema fields
    let schema_links: Vec<_> = schema_fields
        .iter()
        .filter_map(|f| match f {
            crate::ast::Field::FieldDirective(crate::ast::FieldDirective::Link(link)) => Some(link),
            _ => None,
        })
        .collect();

    // Extract relationships from introspection schema
    let intro_relationships = match &introspection.schema {
        crate::db::introspect::SchemaResult::Success { schema, .. } => {
            // Extract relationships from the introspection schema
            let mut relationships = Vec::new();
            for file in &schema.files {
                for def in &file.definitions {
                    if let crate::ast::Definition::Record { name, fields, .. } = def {
                        let table_name = crate::ast::get_tablename(name, fields);
                        if table_name == schema_table.name {
                            relationships.extend(crate::ast::collect_links(fields));
                        }
                    }
                }
            }
            relationships
        }
        _ => Vec::new(),
    };

    // Compare relationships by comparing link names and foreign table/fields
    // We can't use HashSet with Qualified directly because it doesn't implement Hash,
    // so we'll create a comparable signature for each relationship
    let schema_link_signatures: std::collections::HashSet<_> = schema_links
        .iter()
        .map(|link| {
            (
                link.link_name.clone(),
                link.local_ids.clone(),
                link.foreign.table.clone(),
                link.foreign.fields.clone(),
            )
        })
        .collect();
    let intro_link_signatures: std::collections::HashSet<_> = intro_relationships
        .iter()
        .map(|link| {
            (
                link.link_name.clone(),
                link.local_ids.clone(),
                link.foreign.table.clone(),
                link.foreign.fields.clone(),
            )
        })
        .collect();

    if schema_link_signatures != intro_link_signatures {
        // Relationships have changed - mark as modified if there are no other changes
        // or add to existing changes
        if changes.is_empty() {
            // Add a dummy change to mark the record as modified
            // We'll use a special change type for relationship changes
            changes.push(RecordChange::AddedField(
                crate::db::introspect::ColumnInfo {
                    cid: 0,
                    name: "__relationship_change__".to_string(),
                    column_type: "TEXT".to_string(),
                    notnull: false,
                    default_value: None,
                    pk: false,
                    indexed: false,
                },
            ));
        }
    }

    if changes.is_empty() {
        None
    } else {
        Some(DetailedRecordDiff {
            name: schema_table.name.clone(),
            changes,
        })
    }
}

fn index_signature(index: &crate::db::introspect::IndexInfo) -> String {
    let columns = index
        .columns
        .iter()
        .map(|c| {
            if c.desc {
                format!("{}:desc", c.name)
            } else {
                format!("{}:asc", c.name)
            }
        })
        .collect::<Vec<String>>()
        .join(",");

    format!(
        "{}|{}|{}",
        if index.unique { "unique" } else { "index" },
        columns,
        index.where_clause.clone().unwrap_or_default()
    )
}

fn indexes_from_schema_result(
    table_name: &str,
    introspection: &crate::db::introspect::Introspection,
) -> Vec<crate::db::introspect::IndexInfo> {
    match &introspection.schema {
        crate::db::introspect::SchemaResult::Success { schema, .. } => {
            for file in &schema.files {
                for def in &file.definitions {
                    if let crate::ast::Definition::Record { name, fields, .. } = def {
                        let candidate = crate::ast::get_tablename(name, fields);
                        if candidate == table_name {
                            return crate::ast::collect_indexes(fields)
                                .iter()
                                .map(|idx| materialized_index_to_index_info(table_name, idx))
                                .collect();
                        }
                    }
                }
            }
            vec![]
        }
        _ => vec![],
    }
}
