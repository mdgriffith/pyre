use crate::ast;
use crate::ext::string;
use crate::typecheck;
use serde::Serialize;
use serde_json::Value as JsonValue;

// Serializes in a format that libsql can use
#[derive(Serialize)]
#[serde(untagged)]
pub enum SqlAndParams {
    Sql(String),
    SqlWithParams { sql: String, args: Vec<String> },
}

pub struct Prepared {
    pub include: bool,
    pub sql: String,
}

pub fn include(sql: String) -> Prepared {
    Prepared { sql, include: true }
}

pub fn ignore(sql: String) -> Prepared {
    Prepared {
        sql,
        include: false,
    }
}

pub fn format_tablename(name: &str) -> String {
    format!("\"{}\"", crate::ext::string::decapitalize(name))
}

pub fn format_attach(info: &typecheck::QueryInfo) -> Vec<Prepared> {
    let mut attached = vec![];
    if info.attached_dbs.is_empty() {
        return attached;
    }

    let mut attached_db_names: Vec<&String> = info.attached_dbs.iter().collect();
    attached_db_names.sort_unstable();

    for name in attached_db_names {
        attached.push(Prepared {
            include: false,
            sql: format!("attach $db_{} as {}", name, name),
        })
    }

    attached
}

/// Real meaning it's in the db and we might need to use a schema to target it.
pub fn render_real_field(
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
) -> String {
    let table_name = string::quote(&ast::get_tablename(
        &table.record.name,
        &table.record.fields,
    ));

    if table.schema == query_info.primary_db {
        return format!("{}.{}", table_name, string::quote(&query_field.name),);
    } else {
        return format!(
            "{}.{}.{}",
            string::quote(&table.schema),
            table_name,
            string::quote(&query_field.name),
        );
    };
}

/// Real meaning it's in the db and we might need to use a schema to target it.
pub fn render_real_where_field(
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    is_session_var: bool,
    fieldname: &String,
) -> String {
    // Check if this is a Session variable (e.g., Session.userId, Session.role)
    if is_session_var {
        // Session variables are rendered as parameters: userId -> $session_userId
        return format!("$session_{}", fieldname);
    }

    let table_name = string::quote(&ast::get_tablename(
        &table.record.name,
        &table.record.fields,
    ));
    if table.schema == query_info.primary_db {
        return format!("{}.{}", table_name, string::quote(fieldname),);
    } else {
        return format!(
            "{}.{}.{}",
            string::quote(&table.schema),
            table_name,
            string::quote(fieldname),
        );
    };
}

pub fn render_value(value: &ast::QueryValue) -> String {
    match value {
        ast::QueryValue::Fn(func) => {
            if func.name == "now" && func.args.is_empty() {
                "unixepoch()".to_string()
            } else {
                format!(
                    "{}({})",
                    func.name,
                    func.args
                        .iter()
                        .map(|value| render_value(value))
                        .collect::<Vec<String>>()
                        .join(", ")
                )
            }
        }
        ast::QueryValue::Variable((_, var)) => {
            format!("${}", var.name)
        }
        ast::QueryValue::String((_, s)) => {
            // Escape single quotes by doubling them (SQL standard)
            let escaped = s.replace("'", "''");
            format!("'{}'", escaped)
        }
        ast::QueryValue::Int((_, i)) => i.to_string(),
        ast::QueryValue::Float((_, f)) => f.to_string(),
        ast::QueryValue::Bool((_, b)) => if *b { "1" } else { "0" }.to_string(),
        ast::QueryValue::Null(_) => "null".to_string(),
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            // Escape single quotes by doubling them (SQL standard)
            let escaped = details.name.replace("'", "''");
            format!("'{}'", escaped)
        }
    }
}

fn render_json_value(value: &ast::QueryValue) -> String {
    match value {
        ast::QueryValue::Variable((_, var)) => format!("jsonb(${})", var.name),
        _ => render_value(value),
    }
}

fn query_value_to_json_literal(value: &ast::QueryValue) -> Option<JsonValue> {
    match value {
        ast::QueryValue::String((_, s)) => Some(JsonValue::String(s.clone())),
        ast::QueryValue::Int((_, i)) => Some(JsonValue::Number((*i).into())),
        ast::QueryValue::Float((_, f)) => {
            serde_json::Number::from_f64(*f as f64).map(JsonValue::Number)
        }
        ast::QueryValue::Bool((_, b)) => Some(JsonValue::Bool(*b)),
        ast::QueryValue::Null(_) => Some(JsonValue::Null),
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            let mut object = serde_json::Map::new();
            object.insert("_type".to_string(), JsonValue::String(details.name.clone()));

            if let Some(fields) = &details.fields {
                for (name, value) in fields {
                    object.insert(name.clone(), query_value_to_json_literal(value)?);
                }
            }

            Some(JsonValue::Object(object))
        }
        ast::QueryValue::Variable(_) | ast::QueryValue::Fn(_) => None,
    }
}

fn render_typed_json_value(value: &ast::QueryValue) -> String {
    match value {
        ast::QueryValue::Variable((_, var)) => format!("jsonb(${})", var.name),
        _ => match query_value_to_json_literal(value) {
            Some(json) => {
                let rendered = serde_json::to_string(&json).expect("json literal should serialize");
                let escaped = rendered.replace("'", "''");
                format!("jsonb('{}')", escaped)
            }
            None => render_json_value(value),
        },
    }
}

pub fn render_column_value(column: &ast::Column, value: &ast::QueryValue) -> String {
    if matches!(column.type_, ast::ColumnType::Json) {
        return render_json_value(value);
    }

    if matches!(column.type_, ast::ColumnType::JsonTyped(_)) {
        return render_typed_json_value(value);
    }

    render_value(value)
}

pub fn operator(op: &ast::Operator) -> String {
    match op {
        ast::Operator::Equal => "=".to_string(),
        ast::Operator::NotEqual => "!=".to_string(),
        ast::Operator::GreaterThan => ">".to_string(),
        ast::Operator::LessThan => "<".to_string(),
        ast::Operator::GreaterThanOrEqual => ">=".to_string(),
        ast::Operator::LessThanOrEqual => "<=".to_string(),
        ast::Operator::In => "in".to_string(),
        ast::Operator::NotIn => "not in".to_string(),
        ast::Operator::Like => "like".to_string(),
        ast::Operator::NotLike => "not like".to_string(),
    }
}

// WHERE

pub fn render_where(
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
    operation: &ast::QueryOperation,
    result: &mut String,
) {
    render_where_with_table_ref(table, query_info, query_field, operation, None, result);
}

pub fn render_where_with_table_ref(
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
    operation: &ast::QueryOperation,
    table_ref: Option<&str>,
    result: &mut String,
) {
    // Normal @where
    let mut wheres = ast::collect_wheres(&query_field.fields);
    // Add any permissions from the table
    match ast::get_permissions(&table.record, operation) {
        Some(perms) => {
            wheres.push(perms);
        }
        None => {}
    }

    if wheres.is_empty() {
        return;
    }
    result.push_str("where\n");

    // Combine multiple WHERE clauses with AND
    if wheres.len() == 1 {
        let where_str =
            render_where_arg_with_table_ref(&wheres[0], table, query_info, query_field, table_ref);
        result.push_str(&format!(" {}\n", where_str));
    } else {
        // Multiple WHERE clauses need to be combined with AND
        let combined = ast::WhereArg::And(wheres.clone());
        let where_str =
            render_where_arg_with_table_ref(&combined, table, query_info, query_field, table_ref);
        result.push_str(&format!(" {}\n", where_str));
    }
}

pub fn render_where_arg(
    arg: &ast::WhereArg,
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
) -> String {
    render_where_arg_with_table_ref(arg, table, query_info, query_field, None)
}

pub fn render_where_arg_with_table_ref(
    arg: &ast::WhereArg,
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
    table_ref: Option<&str>,
) -> String {
    match arg {
        ast::WhereArg::Column(is_session_var, fieldname, op, value, _field_name_range) => {
            let qualified_column_name = if *is_session_var {
                render_real_where_field(table, query_info, true, fieldname)
            } else if let Some(table_ref) = table_ref {
                format!("{}.{}", table_ref, string::quote(fieldname))
            } else {
                render_real_where_field(table, query_info, false, fieldname)
            };

            let operator = operator(op);

            let rendered_value = if *is_session_var {
                render_value(value)
            } else {
                match table
                    .record
                    .fields
                    .iter()
                    .find(|field| ast::has_fieldname(field, fieldname))
                {
                    Some(ast::Field::Column(column)) => render_column_value(column, value),
                    _ => render_value(value),
                }
            };

            let value = if matches!(op, ast::Operator::In | ast::Operator::NotIn)
                && matches!(value, ast::QueryValue::Variable((_, var)) if var.session_field.is_some())
            {
                format!("(select value from json_each({}))", rendered_value)
            } else {
                rendered_value
            };
            format!("{} {} {}", qualified_column_name, operator, value)
        }
        ast::WhereArg::And(args) => {
            let mut inner_list = vec![];
            for arg in args {
                inner_list.push(render_where_arg_with_table_ref(
                    arg,
                    table,
                    query_info,
                    query_field,
                    table_ref,
                ));
            }
            format!("({})", inner_list.join(" and "))
        }
        ast::WhereArg::Or(args) => {
            let mut inner_list = vec![];
            for arg in args {
                inner_list.push(render_where_arg_with_table_ref(
                    arg,
                    table,
                    query_info,
                    query_field,
                    table_ref,
                ));
            }
            format!("({})", inner_list.join(" or "))
        }
    }
}

pub fn render_order_by(
    table: Option<&typecheck::Table>,
    query_info: Option<&typecheck::QueryInfo>,
    query_field: &ast::QueryField,
    result: &mut String,
) {
    let mut order_vals = vec![];

    for field in &query_field.fields {
        match field {
            ast::ArgField::Arg(located_arg) => {
                if let ast::Arg::OrderBy(dir, col) = &located_arg.arg {
                    let order_direction = ast::direction_to_string(dir);
                    let column_ref = if let (Some(table), Some(query_info)) = (table, query_info) {
                        // Use the actual table name with proper schema qualification
                        render_real_where_field(table, query_info, false, col)
                    } else {
                        // Fallback to query field alias (for backward compatibility)
                        let table_alias = &ast::get_aliased_name(&query_field);
                        format!("{}.{}", string::quote(table_alias), string::quote(col))
                    };
                    order_vals.push(format!("{} {}", column_ref, order_direction));
                }
            }
            _ => continue,
        }
    }

    if !&order_vals.is_empty() {
        result.push_str("order by ");

        let mut first = true;

        for order in order_vals.iter() {
            if first {
                result.push_str(order);
                first = false;
            } else {
                result.push_str(&format!(", {}", order));
            }
        }
        result.push_str("\n");
    }
}

// LIMIT

pub fn render_limit(query_field: &ast::QueryField, result: &mut String) {
    for field in &query_field.fields {
        match field {
            ast::ArgField::Arg(located_arg) => {
                if let ast::Arg::Limit(val) = &located_arg.arg {
                    result.push_str(&format!("limit {}\n", render_value(val)));
                    break;
                }
            }
            _ => continue,
        }
    }
}
