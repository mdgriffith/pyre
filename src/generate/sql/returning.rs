use crate::ast;
use crate::ext::string;
use crate::generate::sql::json::select as json_select;
use crate::typecheck;

pub fn response_expression(
    context: &typecheck::Context,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
) -> String {
    let mut values = Vec::new();
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);

    for field in &query_field.fields {
        let ast::ArgField::Field(query_field) = field else {
            continue;
        };
        let Some(ast::Field::Column(column)) = table
            .record
            .fields
            .iter()
            .find(|field| ast::has_field_or_linkname(field, &query_field.name))
        else {
            continue;
        };

        let column_name = string::quote(&query_field.name);
        let value = if column.type_.is_bool() {
            format!("json(case when {column_name} = 1 then 'true' else 'false' end)")
        } else if column.type_.is_json_like() {
            format!("json({column_name})")
        } else if matches!(
            column.type_.to_serialization_type(),
            ast::SerializationType::FromType(_)
        ) {
            json_select::select_type_expression(
                2,
                context,
                column,
                &table_name,
                &query_field.name,
                false,
            )
        } else {
            column_name
        }
        .trim_end()
        .to_string();
        let separator = if value.starts_with('\n') { "," } else { ", " };
        values.push(format!(
            "'{}'{}{}",
            ast::get_aliased_name(query_field),
            separator,
            value
        ));
    }

    format!("json_object({})", values.join(", "))
}

pub fn affected_rows_expression(context: &typecheck::Context, table: &typecheck::Table) -> String {
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let columns = typecheck::to_sql_column_info(context, &table.record.fields);
    let headers = columns
        .iter()
        .map(|column| format!("'{}'", column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|column| string::quote(&column.name))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "json_object('table_name', '{}', 'headers', json_array({}), 'rows', json_array(json_array({})))",
        table_name, headers, values
    )
}
