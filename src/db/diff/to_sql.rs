use super::*;
use crate::generate::sql::to_sql::SqlAndParams;
pub fn to_sql(diff: &Diff) -> Vec<SqlAndParams> {
    let mut sql_statements = Vec::new();

    // Handle removed tables first (to avoid foreign key conflicts)
    for table in &diff.removed {
        sql_statements.push(SqlAndParams::Sql(format!(
            "drop table if exists \"{}\"",
            table.name
        )));
    }

    // Handle added tables
    for table in &diff.added {
        let columns: Vec<String> = table.columns.iter().map(column_definition).collect();

        let mut create_stmt = format!(
            "create table \"{}\" (\n  {}\n)",
            table.name,
            columns.join(",\n  ")
        );

        // Add foreign key constraints if any exist
        if !table.foreign_keys.is_empty() {
            create_stmt = create_stmt.trim_end_matches(')').to_string();
            for fk in &table.foreign_keys {
                create_stmt.push_str(&format!(
                    ",\n  foreign key ({}) references {}({})",
                    fk.from, fk.table, fk.to
                ));
            }
            create_stmt.push(')');
        }

        sql_statements.push(SqlAndParams::Sql(create_stmt));

        for index in &table.indexes {
            sql_statements.push(SqlAndParams::Sql(render_index_sql(&table.name, index)));
        }

        // Legacy support for column-level @index directives.
        for column in &table.columns {
            if column.indexed && !has_column_index(table.indexes.iter(), &column.name) {
                sql_statements.push(SqlAndParams::Sql(format!(
                    "create index if not exists \"idx_{}_{}\" on \"{}\" (\"{}\")",
                    table.name, column.name, table.name, column.name
                )));
            }
        }
    }

    // Handle modified tables
    for record_diff in &diff.modified_records {
        let added_indexes: Vec<&crate::db::introspect::IndexInfo> = record_diff
            .changes
            .iter()
            .filter_map(|change| match change {
                RecordChange::AddedIndex(index) => Some(index),
                _ => None,
            })
            .collect();

        for change in &record_diff.changes {
            match change {
                RecordChange::AddedField(column) => {
                    sql_statements.push(SqlAndParams::Sql(format!(
                        "alter table \"{}\" add column {}",
                        record_diff.name,
                        column_definition(column)
                    )));

                    if column.indexed
                        && !has_column_index(added_indexes.iter().copied(), &column.name)
                    {
                        sql_statements.push(SqlAndParams::Sql(format!(
                            "create index if not exists \"idx_{}_{}\" on \"{}\" (\"{}\")",
                            record_diff.name, column.name, record_diff.name, column.name
                        )));
                    }
                }
                RecordChange::RemovedField(column) => {
                    sql_statements.push(SqlAndParams::Sql(format!(
                        "alter table \"{}\" drop column \"{}\"",
                        record_diff.name, column.name
                    )));
                }
                RecordChange::ModifiedField { name, .. } => {
                    // For SQLite, we can't directly modify columns, so we need to recreate the table
                    // This would require more complex migration logic
                    sql_statements.push(SqlAndParams::Sql(format!(
                        "-- WARNING: Column modification for `{}`.`{}` requires table recreation",
                        record_diff.name, name
                    )));
                }
                RecordChange::AddedIndex(index) => {
                    sql_statements.push(SqlAndParams::Sql(render_index_sql(
                        &record_diff.name,
                        index,
                    )));
                }
                RecordChange::RemovedIndex(index) => {
                    sql_statements.push(SqlAndParams::Sql(format!(
                        "drop index if exists \"{}\"",
                        index.name
                    )));
                }
            }
        }
    }

    sql_statements
}

fn column_definition(column: &crate::db::introspect::ColumnInfo) -> String {
    let mut def = format!("`{}` {}", column.name, column.column_type);

    if column.pk {
        if column.column_type.eq_ignore_ascii_case("INTEGER") {
            def.push_str(" primary key autoincrement");
        } else {
            def.push_str(" primary key");
        }
    }

    if column.notnull {
        def.push_str(" not null");
    }

    if let Some(default_value) = &column.default_value {
        def.push_str(&format!(" default {}", default_value));
    }

    def
}

fn render_index_sql(table_name: &str, index: &crate::db::introspect::IndexInfo) -> String {
    let columns = index
        .columns
        .iter()
        .map(|c| {
            if c.expression {
                c.name.clone()
            } else if c.desc {
                format!("\"{}\" desc", c.name)
            } else {
                format!("\"{}\" asc", c.name)
            }
        })
        .collect::<Vec<String>>()
        .join(", ");

    let mut sql = format!(
        "create {}index if not exists \"{}\" on \"{}\" ({})",
        if index.unique { "unique " } else { "" },
        index.name,
        table_name,
        columns
    );

    if let Some(where_clause) = &index.where_clause {
        if !where_clause.trim().is_empty() {
            sql.push_str(" where ");
            sql.push_str(where_clause);
        }
    }

    sql
}

fn has_column_index<'a, I>(indexes: I, column_name: &str) -> bool
where
    I: IntoIterator<Item = &'a crate::db::introspect::IndexInfo>,
{
    indexes.into_iter().any(|index| {
        !index.unique
            && index.where_clause.is_none()
            && index.columns.len() == 1
            && !index.columns[0].expression
            && index.columns[0].name == column_name
            && !index.columns[0].desc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_primary_keys_use_autoincrement() {
        let col = crate::db::introspect::ColumnInfo {
            cid: 0,
            name: "id".to_string(),
            column_type: "INTEGER".to_string(),
            notnull: true,
            default_value: None,
            pk: true,
            indexed: false,
        };

        let sql = column_definition(&col);
        assert!(sql.contains("primary key autoincrement"));
    }

    #[test]
    fn non_integer_primary_keys_do_not_use_autoincrement() {
        let col = crate::db::introspect::ColumnInfo {
            cid: 0,
            name: "id".to_string(),
            column_type: "TEXT".to_string(),
            notnull: true,
            default_value: None,
            pk: true,
            indexed: false,
        };

        let sql = column_definition(&col);
        assert!(sql.contains("primary key"));
        assert!(!sql.contains("autoincrement"));
    }

    #[test]
    fn added_table_does_not_duplicate_column_level_index() {
        let diff = Diff {
            added: vec![crate::db::introspect::Table {
                name: "events".to_string(),
                columns: vec![crate::db::introspect::ColumnInfo {
                    cid: 0,
                    name: "updatedAt".to_string(),
                    column_type: "INTEGER".to_string(),
                    notnull: true,
                    default_value: None,
                    pk: false,
                    indexed: true,
                }],
                foreign_keys: vec![],
                indexes: vec![crate::db::introspect::IndexInfo {
                    name: "idx_events_updatedAt".to_string(),
                    unique: false,
                    columns: vec![crate::db::introspect::IndexedColumnInfo {
                        name: "updatedAt".to_string(),
                        desc: false,
                        expression: false,
                    }],
                    where_clause: None,
                }],
            }],
            removed: vec![],
            modified_records: vec![],
        };

        let sql = to_sql(&diff);
        let index_count = sql
            .iter()
            .filter(|statement| match statement {
                SqlAndParams::Sql(sql) => sql.contains("idx_events_updatedAt"),
                SqlAndParams::SqlWithParams { sql, .. } => sql.contains("idx_events_updatedAt"),
            })
            .count();

        assert_eq!(index_count, 1);
    }

    #[test]
    fn added_field_does_not_duplicate_column_level_index() {
        let index = crate::db::introspect::IndexInfo {
            name: "idx_events_updatedAt".to_string(),
            unique: false,
            columns: vec![crate::db::introspect::IndexedColumnInfo {
                name: "updatedAt".to_string(),
                desc: false,
                expression: false,
            }],
            where_clause: None,
        };
        let diff = Diff {
            added: vec![],
            removed: vec![],
            modified_records: vec![DetailedRecordDiff {
                name: "events".to_string(),
                changes: vec![
                    RecordChange::AddedField(crate::db::introspect::ColumnInfo {
                        cid: 0,
                        name: "updatedAt".to_string(),
                        column_type: "INTEGER".to_string(),
                        notnull: true,
                        default_value: None,
                        pk: false,
                        indexed: true,
                    }),
                    RecordChange::AddedIndex(index),
                ],
            }],
        };

        let sql = to_sql(&diff);
        let index_count = sql
            .iter()
            .filter(|statement| match statement {
                SqlAndParams::Sql(sql) => sql.contains("idx_events_updatedAt"),
                SqlAndParams::SqlWithParams { sql, .. } => sql.contains("idx_events_updatedAt"),
            })
            .count();

        assert_eq!(index_count, 1);
    }
}
