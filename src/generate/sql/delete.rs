use crate::ast;
use crate::ext::string;
use crate::generate::sql::returning;
use crate::generate::sql::to_sql;
use crate::typecheck;

pub fn delete_to_string(
    context: &typecheck::Context,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
    include_affected_rows: bool,
) -> Vec<to_sql::Prepared> {
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let mut statements = to_sql::format_attach(query_info);

    // DELETE FROM users
    // WHERE username = 'john_doe';

    let mut sql = format!("delete from {}\n", table_name);

    let mut where_clause = String::new();
    to_sql::render_where(
        context,
        table,
        query_info,
        query_field,
        &ast::QueryOperation::Delete,
        &mut where_clause,
    );
    sql.push_str(&where_clause);

    let response = returning::response_expression(context, table, query_field);
    sql.push_str(&format!(
        " returning {} as {}",
        response,
        string::quote(&ast::get_aliased_name(query_field))
    ));
    if include_affected_rows {
        sql.push_str(&format!(
            ", json_array({}) as _affectedRows",
            returning::affected_rows_expression(context, table)
        ));
    }
    statements.push(to_sql::include(sql));

    statements
}

#[allow(dead_code)]
fn generate_typed_response_query(
    table: &typecheck::Table,
    query_field: &ast::QueryField,
    _primary_table_name: &str,
    temp_table_name: &str,
) -> String {
    let query_field_name = &query_field.name;

    let mut sql = String::new();
    sql.push_str("select\n");
    sql.push_str("  coalesce(json_group_array(\n");
    sql.push_str("    json_object(\n");

    // Generate JSON object fields directly from temp table
    let mut first_field = true;
    for field in &query_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    let aliased_field_name = ast::get_aliased_name(query_field);

                    match table_field {
                        ast::Field::Column(column) => {
                            if !first_field {
                                sql.push_str(",\n");
                            }
                            sql.push_str(&format!("      '{}', ", aliased_field_name));

                            // Handle boolean types: SQLite stores booleans as 0/1, convert to JSON boolean
                            if matches!(column.type_, ast::ColumnType::Bool) {
                                sql.push_str(&format!(
                                    "json(case when {}.{} = 1 then 'true' else 'false' end)",
                                    temp_table_name,
                                    string::quote(&query_field.name)
                                ));
                            } else if column.type_.is_json_like() {
                                sql.push_str(&format!(
                                    "json({}.{})",
                                    temp_table_name,
                                    string::quote(&query_field.name)
                                ));
                            } else {
                                sql.push_str(&format!(
                                    "{}.{}",
                                    temp_table_name,
                                    string::quote(&query_field.name)
                                ));
                            }
                            first_field = false;
                        }
                        _ => continue,
                    }
                }
            }
            _ => continue,
        }
    }

    sql.push_str("\n    )\n  ), json('[]')) as ");
    sql.push_str(query_field_name);
    sql.push_str("\nfrom ");
    sql.push_str(temp_table_name);

    sql
}

#[allow(dead_code)]
fn generate_affected_rows_query(table: &typecheck::Table, temp_table_name: &str) -> String {
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let columns = ast::collect_columns(&table.record.fields);

    // Generate column names
    let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

    // Build json_array call for each row - values in same order as headers
    let mut row_value_parts = Vec::new();
    for col in &column_names {
        let quoted_col = string::quote(col);
        row_value_parts.push(format!("{}.{}", temp_table_name, quoted_col));
    }

    // Build json_array call for headers
    let mut header_parts = Vec::new();
    for col in &column_names {
        header_parts.push(format!("'{}'", col));
    }

    // Format affected rows query - select from temp table and aggregate
    // The temp table contains the rows that were deleted (captured before deletion)
    // Format: { table_name, headers, rows: [[...], [...]] }
    format!(
        "select json_group_array(json(affected_row)) as _affectedRows\nfrom (\n  select json_object(\n    'table_name', '{}',\n    'headers', json_array({}),\n    'rows', json_group_array(json_array({}))\n  ) as affected_row\n  from {}\n)",
        table_name,
        header_parts.join(", "),
        row_value_parts.join(", "),
        temp_table_name
    )
}
