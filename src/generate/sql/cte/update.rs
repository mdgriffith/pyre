use crate::ast;
use crate::ext::string;
use crate::generate::sql::json::select as json_select;
use crate::generate::sql::returning;
use crate::generate::sql::to_sql;
use crate::typecheck;

pub fn update_to_string(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
    include_affected_rows: bool,
) -> Vec<to_sql::Prepared> {
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);

    let mut statements = to_sql::format_attach(query_info);

    let mut result = String::new();
    result.push_str(&format!("update {}\n", table_name));

    // UPDATE users
    // SET credit = 150
    // WHERE username = 'john_doe';

    let mut values: Vec<String> = Vec::new();

    let all_query_fields = ast::collect_query_fields(&query_field.fields);
    let new_values = &to_field_set_values(context, query, table, &all_query_fields);
    values.append(&mut new_values.clone());

    // @updatedAt fields are managed by Pyre. Keep legacy name-based behavior for
    // the sync-added updatedAt field, but allow explicit non-managed overrides.
    let has_updated_at_field = table.record.fields.iter().any(|f| {
        matches!(f, ast::Field::Column(col) if ast::is_updated_at(col))
            || ast::has_fieldname(f, "updatedAt")
    });
    let updated_at_explicitly_set = all_query_fields.iter().any(|f| f.name == "updatedAt");

    if has_updated_at_field && !updated_at_explicitly_set {
        values.push("updatedAt = unixepoch()".to_string());
    }

    result.push_str(&format!("set {}", values.join(", ")));

    result.push_str("\n");
    let mut where_clause = String::new();
    to_sql::render_where(
        context,
        table,
        query_info,
        query_field,
        &ast::QueryOperation::Update,
        &mut where_clause,
    );
    result.push_str(&where_clause);

    let response = returning::response_expression(context, table, query_field);
    result.push_str(&format!(
        " returning {} as {}",
        response,
        string::quote(&ast::get_aliased_name(query_field))
    ));
    if include_affected_rows {
        result.push_str(&format!(
            ", json_array({}) as _affectedRows",
            returning::affected_rows_expression(context, table)
        ));
    }
    statements.push(to_sql::include(result));

    statements
}

#[allow(dead_code)]
fn generate_typed_response_query(
    context: &typecheck::Context,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
    table_name: &str,
    where_clause: &str,
) -> String {
    let query_field_name = &query_field.name;
    let quoted_table_name = string::quote(table_name);

    // Replace table name in WHERE clause with alias 't'
    // Use the exact same replacement logic as generate_affected_rows_query
    // quoted_table_name is already "users" (with quotes), so we need to match "users"."column"
    let mut where_with_alias = where_clause.to_string();
    // Pattern 1: "users"."id" -> t."id" (most common)
    where_with_alias = where_with_alias.replace(&format!("{}.\"", quoted_table_name), "t.\"");
    // Pattern 2: users.id -> t.id (unquoted, shouldn't happen but be safe)
    where_with_alias = where_with_alias.replace(&format!("{}.", table_name), "t.");

    let mut sql = String::new();
    sql.push_str("select\n");
    sql.push_str("  coalesce(json_group_array(\n");
    sql.push_str("    json_object(\n");

    // Generate JSON object fields directly from table
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
                                    "json(case when t.{} = 1 then 'true' else 'false' end)",
                                    string::quote(&query_field.name)
                                ));
                            } else if column.type_.is_json_like() {
                                sql.push_str(&format!(
                                    "json(t.{})",
                                    string::quote(&query_field.name)
                                ));
                            } else if matches!(
                                column.type_.to_serialization_type(),
                                ast::SerializationType::FromType(_)
                            ) {
                                sql.push_str(&json_select::select_type_expression(
                                    6,
                                    context,
                                    column,
                                    "t",
                                    &query_field.name,
                                    false,
                                ));
                            } else {
                                sql.push_str(&format!("t.{}", string::quote(&query_field.name)));
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
    sql.push_str(&quoted_table_name);
    sql.push_str(" t");
    if !where_with_alias.trim().is_empty() {
        // WHERE clause already includes "where\n" prefix, so just append it
        sql.push_str("\n");
        sql.push_str(&where_with_alias);
    }

    sql
}

#[allow(dead_code)]
fn generate_affected_rows_query(
    context: &typecheck::Context,
    table: &typecheck::Table,
    where_clause: &str,
) -> String {
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let column_names: Vec<String> = typecheck::to_sql_column_info(context, &table.record.fields)
        .into_iter()
        .map(|column| column.name)
        .collect();

    // Build json_array call for each row - values in same order as headers
    let mut row_value_parts = Vec::new();
    for col in &column_names {
        let quoted_col = string::quote(col);
        row_value_parts.push(format!("t.{}", quoted_col));
    }

    // Build json_array call for headers
    let mut header_parts = Vec::new();
    for col in &column_names {
        header_parts.push(format!("'{}'", col));
    }

    // Select affected rows using the same WHERE conditions
    // Note: For UPDATE, this selects rows after the update, so the WHERE conditions
    // should still match the updated rows
    // Replace table name in WHERE clause with alias 't'
    // WHERE clauses use format: "users"."id" so we need to replace "users"." with t."
    let quoted_table_name = string::quote(&table_name);
    // Replace quoted table name references: "users"."column" -> t."column"
    // The WHERE clause format is: "where\n "users"."id" = $id\n"
    // We need to replace "users"." with t." to use the alias
    // quoted_table_name is already "users" (with quotes), so we need to match "users"."column"
    let mut where_with_alias = where_clause.to_string();
    // Pattern 1: "users"."id" -> t."id" (most common)
    // quoted_table_name is "users", so we match {quoted_table_name}." which is "users"."
    where_with_alias = where_with_alias.replace(&format!("{}.\"", quoted_table_name), "t.\"");
    // Pattern 2: users.id -> t.id (unquoted, shouldn't happen but be safe)
    where_with_alias = where_with_alias.replace(&format!("{}.", table_name), "t.");
    // Format: { table_name, headers, rows: [[...], [...]] }
    format!(
        "select json_group_array(json(affected_row)) as _affectedRows\nfrom (\n  select json_object(\n    'table_name', '{}',\n    'headers', json_array({}),\n    'rows', json_group_array(json_array({}))\n  ) as affected_row\n  from {} t\n{}\n)",
        table_name,
        header_parts.join(", "),
        row_value_parts.join(", "),
        quoted_table_name,
        where_with_alias.trim()
    )
}

// SET values

fn to_field_set_values(
    context: &typecheck::Context,
    query: &ast::Query,
    table: &typecheck::Table,
    fields: &Vec<&ast::QueryField>,
) -> Vec<String> {
    let mut result = vec![];

    for field in fields {
        let table_field = &table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &field.name))
            .unwrap();

        match &field.set {
            None => (),
            Some(val) => match table_field {
                ast::Field::Column(column) => {
                    if let ast::SerializationType::FromType(typename) =
                        column.type_.to_serialization_type()
                    {
                        if let ast::QueryValue::Variable((_, var)) = val {
                            result.append(&mut render_type_variable_update_values(
                                context,
                                query,
                                column,
                                &field.name,
                                &var.name,
                                &typename,
                            ));
                            continue;
                        }
                    }

                    if let ast::QueryValue::LiteralTypeValue((_, details)) = val {
                        if let ast::SerializationType::FromType(typename) =
                            column.type_.to_serialization_type()
                        {
                            result.push(format!(
                                "{} = '{}'",
                                column.name,
                                details.name.replace("'", "''")
                            ));

                            if let Some((_, type_)) = context.types.get(&typename) {
                                if let typecheck::Type::OneOf { variants } = type_ {
                                    let field_map: std::collections::HashMap<
                                        &String,
                                        &ast::QueryValue,
                                    > = details
                                        .fields
                                        .as_ref()
                                        .map(|fields| {
                                            fields
                                                .iter()
                                                .map(|(name, value)| (name, value))
                                                .collect()
                                        })
                                        .unwrap_or_default();

                                    for variant in variants {
                                        if let Some(variant_fields) = &variant.fields {
                                            for variant_field in variant_fields {
                                                if let ast::Field::Column(variant_col) =
                                                    variant_field
                                                {
                                                    let column_name = format!(
                                                        "{}__{}",
                                                        column.name, variant_col.name
                                                    );
                                                    let value_sql = if variant.name == details.name
                                                    {
                                                        field_map
                                                            .get(&variant_col.name)
                                                            .map(|value| {
                                                                to_sql::render_column_value(
                                                                    variant_col,
                                                                    value,
                                                                )
                                                            })
                                                            .unwrap_or_else(|| "null".to_string())
                                                    } else {
                                                        "null".to_string()
                                                    };
                                                    result.push(format!(
                                                        "{} = {}",
                                                        column_name, value_sql
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            continue;
                        }
                    }

                    let mut str = String::new();

                    str.push_str(&column.name);
                    str.push_str(" = ");
                    str.push_str(&render_update_column_value(
                        query,
                        column,
                        &field.name,
                        &val,
                    ));

                    result.push(str);
                }
                _ => (),
            },
        }
    }

    result
}

fn render_type_variable_update_values(
    context: &typecheck::Context,
    query: &ast::Query,
    column: &ast::Column,
    field_name: &str,
    variable_name: &str,
    typename: &str,
) -> Vec<String> {
    if is_enum_type(context, typename) {
        return vec![format!(
            "{} = {}",
            column.name,
            render_omittable_update_value(
                query,
                field_name,
                &format!("${}", variable_name),
                &column.name,
            )
        )];
    }

    let mut result = vec![format!(
        "{} = {}",
        column.name,
        render_update_type_json_extract(query, field_name, variable_name, "", &column.name)
    )];

    if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(typename) {
        for variant in variants {
            if let Some(fields) = &variant.fields {
                for variant_field in fields {
                    append_type_field_variable_update_values(
                        context,
                        query,
                        &column.name,
                        field_name,
                        variable_name,
                        "",
                        variant_field,
                        &mut result,
                    );
                }
            }
        }
    }

    result
}

fn is_enum_type(context: &typecheck::Context, typename: &str) -> bool {
    matches!(
        context.types.get(typename),
        Some((_, typecheck::Type::OneOf { variants }))
            if variants.iter().all(|variant| variant.fields.is_none())
    )
}

fn append_type_field_variable_update_values(
    context: &typecheck::Context,
    query: &ast::Query,
    column_prefix: &str,
    field_name: &str,
    variable_name: &str,
    json_prefix: &str,
    field: &ast::Field,
    result: &mut Vec<String>,
) {
    let ast::Field::Column(inner_column) = field else {
        return;
    };

    let column_name = format!("{}__{}", column_prefix, inner_column.name);
    let json_path = if json_prefix.is_empty() {
        inner_column.name.clone()
    } else {
        format!("{}.{}", json_prefix, inner_column.name)
    };

    match inner_column.type_.to_serialization_type() {
        ast::SerializationType::Concrete(_) => {
            result.push(format!(
                "{} = {}",
                column_name,
                render_update_json_extract(
                    query,
                    field_name,
                    variable_name,
                    &json_path,
                    &column_name
                )
            ));
        }
        ast::SerializationType::FromType(typename) => {
            result.push(format!(
                "{} = {}",
                column_name,
                render_update_type_json_extract(
                    query,
                    field_name,
                    variable_name,
                    &json_path,
                    &column_name
                )
            ));

            if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(&typename) {
                for variant in variants {
                    if let Some(fields) = &variant.fields {
                        for variant_field in fields {
                            append_type_field_variable_update_values(
                                context,
                                query,
                                &column_name,
                                field_name,
                                variable_name,
                                &json_path,
                                variant_field,
                                result,
                            );
                        }
                    }
                }
            }
        }
    }
}

fn render_update_type_json_extract(
    query: &ast::Query,
    field_name: &str,
    variable_name: &str,
    json_path: &str,
    column_name: &str,
) -> String {
    let type_path = if json_path.is_empty() {
        "$._type".to_string()
    } else {
        format!("$.{}._type", json_path)
    };
    let fallback = if json_path.is_empty() {
        format!("${variable_name}")
    } else {
        "null".to_string()
    };
    let rendered = format!(
        "case when json_valid(${variable_name}) then json_extract(${variable_name}, '{type_path}') else {fallback} end"
    );
    render_omittable_update_value(query, field_name, &rendered, column_name)
}

fn render_update_json_extract(
    query: &ast::Query,
    field_name: &str,
    variable_name: &str,
    json_path: &str,
    column_name: &str,
) -> String {
    let rendered = format!(
        "case when json_valid(${variable_name}) then json_extract(${variable_name}, '$.{json_path}') else null end"
    );
    render_omittable_update_value(query, field_name, &rendered, column_name)
}

fn render_omittable_update_value(
    query: &ast::Query,
    field_name: &str,
    rendered: &str,
    column_name: &str,
) -> String {
    let is_omittable = query
        .args
        .iter()
        .find(|arg| arg.name == field_name)
        .map(|arg| arg.omittable)
        .unwrap_or(false);

    if is_omittable {
        format!(
            "case when ${field_name}__is_set then {rendered} else {column_name} end",
            field_name = field_name,
            rendered = rendered,
            column_name = column_name
        )
    } else {
        rendered.to_string()
    }
}

fn render_update_column_value(
    query: &ast::Query,
    column: &ast::Column,
    field_name: &str,
    value: &ast::QueryValue,
) -> String {
    let rendered = to_sql::render_column_value(column, value);

    let is_omittable = query
        .args
        .iter()
        .find(|arg| arg.name == field_name)
        .map(|arg| arg.omittable)
        .unwrap_or(false);

    if is_omittable {
        format!(
            "case when ${field_name}__is_set then {rendered} else {column_name} end",
            field_name = field_name,
            rendered = rendered,
            column_name = column.name
        )
    } else {
        rendered
    }
}
