use crate::ast;
use crate::ext::string;
use crate::filesystem;
use crate::filesystem::generate_text_file;
use crate::generate::sql;
use crate::generate::typealias;
use crate::generate::typescript::common;
use crate::typecheck;
use std::collections::HashMap;
use std::path::Path;

pub fn generate_schema(
    context: &typecheck::Context,
    database: &ast::Database,
    base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    files.push(generate_text_file(
        base_out_dir.join("decode.ts"),
        generate_decode_file(database),
    ));
    files.push(generate_text_file(
        base_out_dir.join("schema.ts"),
        to_schema_metadata(context),
    ));
    files.push(generate_text_file(
        base_out_dir.join("queries/sql/types.ts"),
        sql_types_file(),
    ));
}

pub fn generate_queries(
    context: &typecheck::Context,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    query_list: &ast::QueryList,
    base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    let formatter = to_metadata_formatter();

    for operation in &query_list.queries {
        match operation {
            ast::QueryDef::Query(q) => {
                let query_info = all_query_info.get(&q.name);
                files.push(generate_text_file(
                    base_out_dir
                        .join("queries/metadata")
                        .join(format!("{}.ts", string::decapitalize(&q.name))),
                    to_query_metadata_file(context, q, query_info, &formatter),
                ));

                if let Some(query_info) = query_info {
                    files.push(generate_text_file(
                        base_out_dir
                            .join("queries/sql")
                            .join(format!("{}.ts", string::decapitalize(&q.name))),
                        to_query_sql_file(context, query_info, q),
                    ));
                } else {
                    eprintln!("Warning: Query '{}' was found but not in typecheck results. Skipping SQL generation.", q.name);
                }
            }
            _ => continue,
        }
    }
}

fn sql_types_file() -> String {
    let mut result = String::new();
    result.push_str("export type SqlInfo = {\n");
    result.push_str("  include: boolean;\n");
    result.push_str("  params: string[];\n");
    result.push_str("  sql: string;\n");
    result.push_str("};\n");
    result
}

fn generate_decode_file(database: &ast::Database) -> String {
    let mut result = String::new();

    result.push_str("import { z } from 'zod';\n");
    result.push_str("\n");
    result.push_str(common::coercion_helpers());
    result.push_str(common::json_type_definition());
    result.push_str(
        "export function decodeOrThrow<T>(validator: z.ZodType<T>, data: unknown, label: string = 'data'): T {\n",
    );
    result.push_str("  const decoded = validator.safeParse(data);\n");
    result.push_str("  if (!decoded.success) {\n");
    result.push_str("    const errorStr = JSON.stringify(decoded.error, null, 2);\n");
    result.push_str("    throw new Error(`Failed to decode ${label}: ${errorStr}`);\n");
    result.push_str("  }\n");
    result.push_str("  return decoded.data;\n");
    result.push_str("}\n\n");

    // Get session definition
    let session = database
        .schemas
        .iter()
        .find_map(|s| s.session.clone())
        .unwrap_or_else(|| ast::default_session_details());

    // Generate Session type
    result.push_str("// Session type\n");
    result.push_str("export interface Session {\n");
    for field in &session.fields {
        if let ast::Field::Column(col) = field {
            let type_str = col.type_.to_string();
            let ts_type = match type_str.as_str() {
                "String" => "string",
                "Int" | "Float" => "number",
                "Bool" => "boolean",
                "DateTime" => "Date",
                _ if type_str == "Id.Int"
                    || type_str == "Id.Uuid"
                    || type_str.starts_with("Id.Int<")
                    || type_str.starts_with("Id.Uuid<")
                    || type_str.contains('.') =>
                {
                    "number"
                }
                other => other,
            };
            let optional = if col.nullable { "?" } else { "" };
            result.push_str(&format!("  {}{}: {};\n", col.name, optional, ts_type));
        }
    }
    result.push_str("}\n\n");

    result.push_str("export const SessionValidator = z.object({\n");
    for field in &session.fields {
        if let ast::Field::Column(col) = field {
            let type_str = col.type_.to_string();
            let validator = match type_str.as_str() {
                "String" => "z.string()".to_string(),
                "Int" | "Float" => "z.number()".to_string(),
                "Bool" => "CoercedBool".to_string(),
                "DateTime" => "CoercedDate".to_string(),
                _ if type_str == "Id.Int"
                    || type_str == "Id.Uuid"
                    || type_str.starts_with("Id.Int<")
                    || type_str.starts_with("Id.Uuid<")
                    || type_str.contains('.') =>
                {
                    "z.number()".to_string()
                }
                other => format!("z.any() /* {} */", other),
            };
            let validator = if col.nullable {
                format!("{}.optional()", validator)
            } else {
                validator
            };
            result.push_str(&format!("  {}: {},\n", col.name, validator));
        }
    }
    result.push_str("});\n\n");

    // Generate custom type definitions (tagged unions) in dependency order
    let sorted_types = common::sort_types_by_dependency(database);
    for (name, variants) in sorted_types {
        result.push_str(&common::generate_tagged_union(&name, &variants));
    }

    result
}

fn to_metadata_formatter() -> typealias::TypeFormatter {
    typealias::TypeFormatter {
        to_comment: Box::new(|s| format!("// {}\n", s)),
        to_type_def_start: Box::new(|name| format!("const {} = z.object({{\n", name)),
        to_field: Box::new(
            |name,
             type_,
             typealias::FieldMetadata {
                 is_link,
                 is_optional,
                 is_array_relationship,
             }| {
                let parsed_type = ast::ColumnType::from_str(type_);
                let (base_type, is_primitive, needs_coercion) = if is_link {
                    (type_.to_string(), false, false)
                } else {
                    match &parsed_type {
                        ast::ColumnType::String => ("z.string()".to_string(), true, false),
                        ast::ColumnType::Int | ast::ColumnType::Float => {
                            ("z.number()".to_string(), true, false)
                        }
                        ast::ColumnType::Bool => ("z.boolean()".to_string(), true, true),
                        ast::ColumnType::DateTime => ("z.date()".to_string(), true, true),
                        ast::ColumnType::IdInt { .. } => ("z.number()".to_string(), true, false),
                        ast::ColumnType::IdUuid { .. } => ("z.string()".to_string(), true, false),
                        ast::ColumnType::ForeignKey { .. } => {
                            ("z.number()".to_string(), true, false)
                        }
                        _ => (output_zod_type_for_column_type(&parsed_type), false, false),
                    }
                };

                let type_str = if needs_coercion {
                    match type_ {
                        "DateTime" => match (is_link, is_array_relationship, is_optional) {
                            (true, true, _) => "CoercedDate.array()".to_string(),
                            (true, false, true) => "CoercedDate.nullable()".to_string(),
                            (true, false, false) => "CoercedDate".to_string(),
                            (false, _, true) => "CoercedDate.nullable()".to_string(),
                            (false, _, false) => "CoercedDate".to_string(),
                        },
                        "Bool" => match (is_link, is_array_relationship, is_optional) {
                            (true, true, _) => "CoercedBool.array()".to_string(),
                            (true, false, true) => "CoercedBool.nullable()".to_string(),
                            (true, false, false) => "CoercedBool".to_string(),
                            (false, _, true) => "CoercedBool.nullable()".to_string(),
                            (false, _, false) => "CoercedBool".to_string(),
                        },
                        _ => unreachable!(),
                    }
                } else {
                    match (is_primitive, is_link, is_array_relationship, is_optional) {
                        (true, true, true, _) => format!("{}.array()", base_type),
                        (true, true, false, true) => format!("{}.nullable()", base_type),
                        (true, true, false, false) => base_type.to_string(),
                        (true, false, _, true) => format!("{}.nullable()", base_type),
                        (true, false, _, false) => base_type.to_string(),
                        (false, true, true, _) => format!("{}.array()", base_type),
                        (false, true, false, true) => format!("{}.nullable()", base_type),
                        (false, true, false, false) => base_type.to_string(),
                        (false, false, _, true) => format!("{}.nullable()", base_type),
                        (false, false, _, false) => base_type.to_string(),
                    }
                };

                format!("  {}: {}", name, type_str)
            },
        ),
        to_type_def_end: Box::new(|| "});\n".to_string()),
        to_field_separator: Box::new(|is_last| {
            if is_last {
                "\n".to_string()
            } else {
                ",\n".to_string()
            }
        }),
    }
}

fn to_query_metadata_file(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: Option<&typecheck::QueryInfo>,
    formatter: &typealias::TypeFormatter,
) -> String {
    let mut return_data = String::new();
    typealias::return_data_aliases(context, query, &mut return_data, formatter);
    let uses_coerced_date = return_data.contains("CoercedDate");
    let uses_coerced_bool = return_data.contains("CoercedBool");

    let mut imports = String::new();
    imports.push_str("import { z } from 'zod';\n");
    if query.operation == ast::QueryOperation::Query {
        imports.push_str("import type { QueryShape } from '@pyre/core';\n");
    }
    if uses_coerced_bool || uses_coerced_date {
        imports.push_str("import { ");
        if uses_coerced_bool {
            imports.push_str("CoercedBool");
        }
        if uses_coerced_bool && uses_coerced_date {
            imports.push_str(", ");
        }
        if uses_coerced_date {
            imports.push_str("CoercedDate");
        }
        imports.push_str(" } from '../../decode';\n");
    }
    imports.push_str("import * as Decode from '../../decode';\n");

    let input_block = to_param_type_alias(&query.args).trim_end().to_string();

    let query_shape_block = if query.operation == ast::QueryOperation::Query {
        Some(to_query_shape(context, query).trim_end().to_string())
    } else {
        None
    };

    let session_args = match query_info {
        Some(info) => get_session_args(&info.variables),
        None => "[]".to_string(),
    };
    let omittable_args = format!(
        "[{}]",
        query
            .args
            .iter()
            .filter(|arg| arg.omittable)
            .map(|arg| format!("\"{}\"", arg.name))
            .collect::<Vec<String>>()
            .join(", ")
    );
    let json_input_args = format!(
        "[{}]",
        query
            .args
            .iter()
            .filter(|arg| {
                arg.type_
                    .as_ref()
                    .map(|type_name| ast::ColumnType::from_str(type_name).is_json_like())
                    .unwrap_or(false)
            })
            .map(|arg| format!("\"{}\"", arg.name))
            .collect::<Vec<String>>()
            .join(", ")
    );

    let mut meta_block = String::new();
    meta_block.push_str("export const meta = {\n");
    meta_block.push_str(&format!("  id: \"{}\",\n", &query.interface_hash));
    if let Some(info) = query_info {
        meta_block.push_str(&format!(
            "  primary_db: {},\n",
            string::quote(&info.primary_db)
        ));
        let mut attached_dbs: Vec<&String> = info.attached_dbs.iter().collect();
        attached_dbs.sort();
        meta_block.push_str(&format!(
            "  attached_dbs: [{}],\n",
            attached_dbs
                .iter()
                .map(|db| string::quote(db))
                .collect::<Vec<String>>()
                .join(", ")
        ));
    }
    meta_block.push_str(&format!(
        "  operation: \"{}\" as const,\n",
        match query.operation {
            ast::QueryOperation::Query => "query",
            ast::QueryOperation::Insert => "insert",
            ast::QueryOperation::Update => "update",
            ast::QueryOperation::Delete => "delete",
        }
    ));
    meta_block.push_str(&format!("  session_args: {},\n", session_args));
    meta_block.push_str(&format!("  optional_input_args: {},\n", omittable_args));
    meta_block.push_str(&format!("  json_input_args: {},\n", json_input_args));
    meta_block.push_str("  InputValidator,\n");
    meta_block.push_str("  SessionValidator: Decode.SessionValidator,\n");
    meta_block.push_str("  ReturnData,\n");
    if query.operation == ast::QueryOperation::Query {
        meta_block.push_str("  queryShape,\n");
        meta_block.push_str("  toQueryShape: (_input: Input) => queryShape,\n");
    }
    if let Some(optimistic) = to_optimistic_update_metadata(query) {
        meta_block.push_str(&format!("  optimistic: {},\n", optimistic));
    }
    meta_block.push_str("};\n");

    let result_block = "export type Result = z.infer<typeof ReturnData>;".to_string();

    let mut blocks: Vec<String> = Vec::new();
    blocks.push(imports.trim_end().to_string());
    if !input_block.is_empty() {
        blocks.push(input_block);
    }
    if let Some(query_shape_block) = query_shape_block {
        blocks.push(query_shape_block);
    }
    if !return_data.trim().is_empty() {
        blocks.push(return_data.trim_end().to_string());
    }
    blocks.push(result_block);
    blocks.push(meta_block.trim_end().to_string());

    format!("{}\n", blocks.join("\n\n"))
}

fn to_query_sql_file(
    context: &typecheck::Context,
    query_info: &typecheck::QueryInfo,
    query: &ast::Query,
) -> String {
    let mut result = String::new();

    result.push_str("import type { SqlInfo } from './types';\n\n");
    result.push_str("export const sql: SqlInfo[] = ");
    result.push_str(&format_sql_entries(context, query_info, query, false));
    result.push_str(";\n");

    if query.operation == ast::QueryOperation::Query {
        result.push_str("export const syncSql: SqlInfo[] | undefined = undefined;\n");
    } else {
        result.push_str("export const syncSql: SqlInfo[] = ");
        result.push_str(&format_sql_entries(context, query_info, query, true));
        result.push_str(";\n");
    }

    result
}

fn format_sql_entries(
    context: &typecheck::Context,
    query_info: &typecheck::QueryInfo,
    query: &ast::Query,
    sync_mode: bool,
) -> String {
    let mut result = String::new();
    result.push('[');
    let mut written_field = false;
    for field in &query.fields {
        match field {
            ast::TopLevelQueryField::Field(query_field) => {
                match context.tables.get(&query_field.name) {
                    Some(table) => {
                        let param_info = get_formatted_used_params(
                            query,
                            &ast::get_aliased_name(query_field),
                            &query_info.variables,
                        );
                        let prepared = if query.operation == ast::QueryOperation::Query {
                            sql::to_string(context, query, query_info, table, query_field)
                        } else {
                            sql::to_string_with_affected_rows(
                                context,
                                query,
                                query_info,
                                table,
                                query_field,
                                sync_mode,
                            )
                        };

                        for prepped in prepared {
                            if sync_mode
                                && prepped.include
                                && !prepped.sql.contains("_affectedRows")
                            {
                                continue;
                            }
                            if written_field {
                                result.push_str(",\n");
                            }
                            result.push_str(&format!(
                                "  {{\n    include: {},\n    params: {},\n    sql: {}\n  }}",
                                bool_to_ts_bool(prepped.include),
                                &param_info,
                                &literal_quote(&prepped.sql)
                            ));
                            written_field = true;
                        }
                    }
                    None => {
                        eprintln!("Error: Table '{}' referenced in query '{}' was not found in typecheck context. This should not happen after successful typechecking. Skipping field generation.", query_field.name, query.name);
                    }
                }
            }
            _ => {}
        }
    }

    result.push_str("\n]");
    result
}

fn literal_quote(s: &str) -> String {
    format!("`{}`", s)
}

fn get_formatted_used_params(
    query: &ast::Query,
    top_level_field_alias: &str,
    query_params: &HashMap<String, typecheck::ParamInfo>,
) -> String {
    let mut used_params: Vec<String> = Vec::new();

    for (_, param_info) in query_params {
        match param_info {
            typecheck::ParamInfo::NotDefinedButUsed { .. } => continue,
            typecheck::ParamInfo::Defined {
                used_by_top_level_field_alias,
                raw_variable_name,
                from_session,
                ..
            } => {
                if !*from_session || used_by_top_level_field_alias.contains(top_level_field_alias) {
                    used_params.push(raw_variable_name.clone());
                }
            }
        }
    }

    for arg in &query.args {
        if arg.omittable {
            used_params.push(format!("{}__is_set", arg.name));
        }
    }

    used_params.sort_unstable();
    used_params.dedup();

    let mut formatted = String::new();
    formatted.push_str("[ ");
    for (index, param_name) in used_params.iter().enumerate() {
        if index > 0 {
            formatted.push_str(", ");
        }
        formatted.push_str(&string::quote(param_name));
    }

    formatted.push_str(" ]");
    formatted
}

fn bool_to_ts_bool(bool: bool) -> String {
    if bool {
        return "true".to_string();
    }
    "false".to_string()
}

fn get_session_args(params: &HashMap<String, typecheck::ParamInfo>) -> String {
    let mut session_args: Vec<String> = Vec::new();

    for (_name, info) in params {
        match info {
            typecheck::ParamInfo::Defined {
                from_session,
                used,
                session_name,
                ..
            } => {
                if *from_session && *used {
                    match session_name {
                        None => continue,
                        Some(session_name_string) => {
                            session_args.push(session_name_string.clone());
                        }
                    }
                }
            }
            _ => continue,
        }
    }

    session_args.sort_unstable();

    let mut result = "[ ".to_string();
    for (index, session_name) in session_args.iter().enumerate() {
        if index > 0 {
            result.push_str(", ");
        }
        result.push_str(&crate::ext::string::quote(session_name));
    }

    result.push_str("]");
    result
}

struct OptimisticUpdateMetadata {
    query_field: String,
    where_field: String,
    where_input: String,
    set_fields: Vec<(String, String)>,
}

fn optimistic_update_metadata(query: &ast::Query) -> Option<OptimisticUpdateMetadata> {
    if query.operation != ast::QueryOperation::Update {
        return None;
    }

    let mut top_fields = query.fields.iter().filter_map(|field| match field {
        ast::TopLevelQueryField::Field(query_field) => Some(query_field),
        _ => None,
    });

    let query_field = top_fields.next()?;
    if top_fields.next().is_some() {
        return None;
    }

    let wheres = ast::collect_wheres(&query_field.fields);
    let where_ = match wheres.as_slice() {
        [ast::WhereArg::Column(
            false,
            field,
            ast::Operator::Equal,
            ast::QueryValue::Variable((_, variable)),
            _,
        )] => {
            if variable.session_field.is_some() {
                return None;
            }
            (field.clone(), variable.name.clone())
        }
        _ => return None,
    };

    let set_fields: Vec<(String, String)> = ast::collect_query_fields(&query_field.fields)
        .into_iter()
        .filter_map(|field| match &field.set {
            Some(ast::QueryValue::Variable((_, variable))) if variable.session_field.is_none() => {
                Some((field.name.clone(), variable.name.clone()))
            }
            _ => None,
        })
        .collect();

    if set_fields.is_empty() {
        return None;
    }

    Some(OptimisticUpdateMetadata {
        query_field: query_field.name.clone(),
        where_field: where_.0,
        where_input: where_.1,
        set_fields,
    })
}

fn to_optimistic_update_metadata(query: &ast::Query) -> Option<String> {
    let metadata = optimistic_update_metadata(query)?;
    let set_fields = metadata
        .set_fields
        .iter()
        .map(|(field, input)| {
            format!(
                "{{ field: {}, input: {} }}",
                string::quote(field),
                string::quote(input)
            )
        })
        .collect::<Vec<String>>()
        .join(", ");

    Some(format!(
        "{{ queryField: {}, where: {{ field: {}, input: {} }}, set: [{}] }}",
        string::quote(&metadata.query_field),
        string::quote(&metadata.where_field),
        string::quote(&metadata.where_input),
        set_fields
    ))
}

fn to_query_shape(context: &typecheck::Context, query: &ast::Query) -> String {
    let mut result = "const queryShape: QueryShape = {\n".to_string();

    let mut is_first_table = true;
    for field in &query.fields {
        match field {
            ast::TopLevelQueryField::Field(query_field) => {
                if !is_first_table {
                    result.push_str(",\n");
                }
                is_first_table = false;

                let field_name = ast::get_aliased_name(query_field);

                result.push_str(&format!("  {}: {{\n", string::quote(&field_name)));

                let table = context.tables.get(&query_field.name);
                result.push_str(&to_query_field_spec(context, query_field, table));

                result.push_str("\n  }");
            }
            _ => {}
        }
    }

    result.push_str("\n};\n");
    result
}

fn to_query_field_spec(
    context: &typecheck::Context,
    query_field: &ast::QueryField,
    table: Option<&typecheck::Table>,
) -> String {
    let mut result = String::new();
    let mut is_first = true;
    let table = table.or_else(|| context.tables.get(&query_field.name));

    let mut where_clause: Option<String> = None;
    let mut sort_clauses: Vec<String> = Vec::new();
    let mut limit: Option<i32> = None;

    let mut field_selections: Vec<(String, String, bool, bool)> = Vec::new();
    let mut selected_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut explicit_columns: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(table_info) = table {
        for arg_field in &query_field.fields {
            if let ast::ArgField::Field(nested_field) = arg_field {
                if nested_field.name == "*" {
                    continue;
                }
                if let Some(ast::Field::Column(column)) = table_info
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(f, &nested_field.name))
                {
                    explicit_columns.insert(column.name.clone());
                }
            }
        }
    }

    for arg_field in &query_field.fields {
        match arg_field {
            ast::ArgField::Arg(located_arg) => match &located_arg.arg {
                ast::Arg::Where(where_arg) => {
                    where_clause = Some(to_where_clause_ts(where_arg));
                }
                ast::Arg::OrderBy(direction, field_name) => {
                    let dir_str = match direction {
                        ast::Direction::Asc => "asc",
                        ast::Direction::Desc => "desc",
                    };
                    sort_clauses.push(format!(
                        "{{ field: {}, direction: {} }}",
                        string::quote(field_name),
                        string::quote(dir_str)
                    ));
                }
                ast::Arg::Limit(query_value) => {
                    if let ast::QueryValue::Int((_, val)) = query_value {
                        limit = Some(*val);
                    }
                }
            },
            ast::ArgField::Field(nested_field) => {
                if nested_field.name == "*" {
                    if let Some(table_info) = table {
                        for table_field in &table_info.record.fields {
                            if let ast::Field::Column(column) = table_field {
                                if explicit_columns.contains(&column.name) {
                                    continue;
                                }
                                if selected_names.insert(column.name.clone()) {
                                    field_selections.push((
                                        column.name.clone(),
                                        column.name.clone(),
                                        false,
                                        false,
                                    ));
                                }
                            }
                        }
                    }
                    continue;
                }

                let is_relationship = if let Some(table_info) = table {
                    let links = ast::collect_links(&table_info.record.fields);
                    links.iter().any(|link| link.link_name == nested_field.name)
                } else {
                    false
                };

                let has_nested_fields = !nested_field.fields.is_empty()
                    && nested_field
                        .fields
                        .iter()
                        .any(|f| matches!(f, ast::ArgField::Field(_)));

                let aliased_name = ast::get_aliased_name(nested_field);
                if selected_names.insert(aliased_name.clone()) {
                    field_selections.push((
                        nested_field.name.clone(),
                        aliased_name,
                        is_relationship,
                        has_nested_fields,
                    ));
                }
            }
            _ => {}
        }
    }

    if let Some(where_clause) = where_clause {
        result.push_str(&format!("  \"@where\": {}", where_clause));
        is_first = false;
    }

    for (field_name, aliased_name, is_relationship, has_nested_fields) in field_selections {
        if !is_first {
            result.push_str(",\n");
        }
        is_first = false;

        if is_relationship && has_nested_fields {
            if let Some(nested_field) = query_field.fields.iter().find_map(|f| match f {
                ast::ArgField::Field(qf) if qf.name == field_name => Some(qf),
                _ => None,
            }) {
                let nested_table = table.and_then(|table_info| {
                    table_info
                        .record
                        .fields
                        .iter()
                        .find_map(|field| match field {
                            ast::Field::FieldDirective(ast::FieldDirective::Link(link))
                                if link.link_name == field_name =>
                            {
                                typecheck::get_linked_table(context, link)
                            }
                            _ => None,
                        })
                });
                result.push_str(&format!("    {}: {{\n", string::quote(&aliased_name)));
                if aliased_name != field_name {
                    result.push_str(&format!(
                        "      \"@source\": {},\n",
                        string::quote(&field_name)
                    ));
                }
                result.push_str(&to_query_field_spec(context, nested_field, nested_table));
                result.push_str("\n    }");
            }
        } else if is_relationship {
            result.push_str(&to_query_shape_leaf(&field_name, &aliased_name));
        } else {
            result.push_str(&to_query_shape_leaf(&field_name, &aliased_name));
        }
    }

    if !sort_clauses.is_empty() || limit.is_some() {
        if !is_first {
            result.push_str(",\n");
        }

        if !sort_clauses.is_empty() {
            if sort_clauses.len() == 1 {
                result.push_str(&format!("    '@sort': {}", sort_clauses[0]));
            } else {
                result.push_str(&format!("    '@sort': [{}]", sort_clauses.join(", ")));
            }
        }

        if let Some(limit_val) = limit {
            if !sort_clauses.is_empty() {
                result.push_str(",\n");
            }
            result.push_str(&format!("    '@limit': {}", limit_val));
        }
    }

    result
}

fn to_query_shape_leaf(field_name: &str, aliased_name: &str) -> String {
    if aliased_name == field_name {
        format!("    {}: true", string::quote(aliased_name))
    } else {
        format!(
            "    {}: {{ \"@source\": {}, \"@select\": true }}",
            string::quote(aliased_name),
            string::quote(field_name)
        )
    }
}

fn to_where_clause_ts(where_arg: &ast::WhereArg) -> String {
    match where_arg {
        ast::WhereArg::Column(is_session_field, field_name, operator, value, _) => {
            let key = if *is_session_field {
                format!("Session.{}", field_name)
            } else {
                field_name.clone()
            };

            match operator {
                ast::Operator::Equal => format!(
                    "{{ {}: {} }}",
                    string::quote(&key),
                    to_query_value_ts(value)
                ),
                _ => format!(
                    "{{ {}: {{ {}: {} }} }}",
                    string::quote(&key),
                    string::quote(to_filter_operator_key(operator)),
                    to_query_value_ts(value)
                ),
            }
        }
        ast::WhereArg::And(items) => format!(
            "{{ \"$and\": [ {} ] }}",
            items
                .iter()
                .map(to_where_clause_ts)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ast::WhereArg::Or(items) => format!(
            "{{ \"$or\": [ {} ] }}",
            items
                .iter()
                .map(to_where_clause_ts)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn to_query_value_ts(value: &ast::QueryValue) -> String {
    match value {
        ast::QueryValue::Variable((_, details)) => match &details.session_field {
            Some(field) => format!("{{ \"$session\": {} }}", string::quote(field)),
            None => format!("{{ \"$var\": {} }}", string::quote(&details.name)),
        },
        ast::QueryValue::String((_, value)) => string::quote(value),
        ast::QueryValue::Int((_, value)) => value.to_string(),
        ast::QueryValue::Float((_, value)) => value.to_string(),
        ast::QueryValue::Bool((_, value)) => value.to_string(),
        ast::QueryValue::Null(_) => "null".to_string(),
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            let mut fields = vec![format!("\"type\": {}", string::quote(&details.name))];
            if let Some(assignments) = &details.fields {
                for (name, value) in assignments {
                    fields.push(format!(
                        "{}: {}",
                        string::quote(name),
                        to_query_value_ts(value)
                    ));
                }
            }
            format!("{{ {} }}", fields.join(", "))
        }
        ast::QueryValue::Fn(func) => format!("{{ \"$fn\": {} }}", string::quote(&func.name)),
    }
}

fn to_filter_operator_key(operator: &ast::Operator) -> &'static str {
    match operator {
        ast::Operator::Equal => "$eq",
        ast::Operator::NotEqual => "$ne",
        ast::Operator::GreaterThan => "$gt",
        ast::Operator::LessThan => "$lt",
        ast::Operator::GreaterThanOrEqual => "$gte",
        ast::Operator::LessThanOrEqual => "$lte",
        ast::Operator::In => "$in",
        ast::Operator::NotIn => "$nin",
        ast::Operator::Like => "$like",
        ast::Operator::NotLike => "$nlike",
    }
}

fn to_schema_metadata(context: &typecheck::Context) -> String {
    let mut result = String::new();
    result.push_str("import type { SchemaMetadata } from '@pyre/core';\n\n");

    result.push_str("export const schemaMetadata: SchemaMetadata = {\n");
    result.push_str("  tables: {\n");

    let mut tables: Vec<&typecheck::Table> = context.tables.values().collect();
    tables.sort_by(|a, b| {
        let a_table_name = ast::get_tablename(&a.record.name, &a.record.fields);
        let b_table_name = ast::get_tablename(&b.record.name, &b.record.fields);
        a_table_name.cmp(&b_table_name)
    });

    let mut is_first_table = true;
    for table in &tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);

        if !is_first_table {
            result.push_str(",\n");
        }
        is_first_table = false;

        result.push_str(&format!("    {}: {{\n", string::quote(&table_name)));
        result.push_str(&format!("      name: {},\n", string::quote(&table_name)));
        result.push_str(&format!(
            "      namespace: {},\n",
            string::quote(&table.schema)
        ));
        let sync_mode = context
            .namespace_sync_modes
            .get(&table.schema)
            .copied()
            .unwrap_or(ast::SyncMode::Synced);
        result.push_str(&format!(
            "      sync: {},\n",
            string::quote(sync_mode.as_str())
        ));
        result.push_str("      columns: [\n");

        let mut is_first_column = true;
        for field in &table.record.fields {
            if let ast::Field::Column(column) = field {
                if !is_first_column {
                    result.push_str(",\n");
                }
                is_first_column = false;

                let is_primary = ast::is_primary_key(column);
                let is_unique = column
                    .directives
                    .iter()
                    .any(|d| matches!(d, ast::ColumnDirective::Unique));
                let is_indexed = column
                    .directives
                    .iter()
                    .any(|d| matches!(d, ast::ColumnDirective::Index));

                result.push_str("        {\n");
                result.push_str(&format!(
                    "          name: {},\n",
                    string::quote(&column.name)
                ));
                result.push_str(&format!(
                    "          type: {},\n",
                    string::quote(&column.type_.to_string())
                ));
                result.push_str(&format!(
                    "          nullable: {},\n",
                    if column.nullable { "true" } else { "false" }
                ));
                result.push_str(&format!(
                    "          primary: {},\n",
                    if is_primary { "true" } else { "false" }
                ));
                result.push_str(&format!(
                    "          unique: {},\n",
                    if is_unique || is_primary {
                        "true"
                    } else {
                        "false"
                    }
                ));
                result.push_str(&format!(
                    "          indexed: {}",
                    if is_indexed || is_unique || is_primary {
                        "true"
                    } else {
                        "false"
                    }
                ));
                if let Some(comment) = &column.inline_comment {
                    result.push_str(&format!(",\n          comment: {}", string::quote(comment)));
                }
                result.push_str("\n        }");
            }
        }

        result.push_str("\n      ],\n");
        result.push_str("      links: {\n");

        let links = ast::collect_links(&table.record.fields);
        let primary_key_name = ast::get_primary_id_field_name(&table.record.fields);

        let mut is_first_rel = true;
        for link in links {
            if !is_first_rel {
                result.push_str(",\n");
            }
            is_first_rel = false;

            let is_many_to_one = link
                .local_ids
                .iter()
                .any(|id| primary_key_name.as_ref().map(|pk| id != pk).unwrap_or(true));

            let foreign_table = get_linked_table(context, &link).expect(&format!(
                "Failed to find linked table '{}' in context. This indicates a schema error.",
                link.foreign.table
            ));
            let foreign_table_name =
                ast::get_tablename(&foreign_table.record.name, &foreign_table.record.fields);

            let is_one_to_one = if is_many_to_one {
                let foreign_field_is_unique =
                    ast::linked_to_unique_field_with_record(&link, &foreign_table.record);
                let local_field_is_unique = if link.local_ids.len() == 1 {
                    ast::field_is_unique(&link.local_ids[0], &table.record)
                } else {
                    false
                };
                foreign_field_is_unique && local_field_is_unique
            } else {
                false
            };

            let link_type = if is_one_to_one {
                "one-to-one"
            } else if is_many_to_one {
                "many-to-one"
            } else {
                "one-to-many"
            };

            let (from_column, to_table, to_column) = if is_many_to_one {
                (
                    link.local_ids[0].clone(),
                    foreign_table_name,
                    link.foreign.fields[0].clone(),
                )
            } else {
                (
                    primary_key_name.clone().unwrap_or_else(|| "id".to_string()),
                    foreign_table_name,
                    link.foreign.fields[0].clone(),
                )
            };

            result.push_str(&format!("        {}: {{\n", string::quote(&link.link_name)));
            result.push_str(&format!("          type: {},\n", string::quote(link_type)));
            result.push_str(&format!(
                "          from: {},\n",
                string::quote(&from_column)
            ));
            result.push_str("          to: {\n");
            result.push_str(&format!(
                "            table: {},\n",
                string::quote(&to_table)
            ));
            result.push_str(&format!(
                "            column: {}\n",
                string::quote(&to_column)
            ));
            result.push_str("          }\n");
            result.push_str("        }");
        }

        result.push_str("\n      },\n");

        result.push_str("      indices: [\n");

        let mut is_first_index = true;
        for field in &table.record.fields {
            if let ast::Field::Column(column) = field {
                let is_primary = ast::is_primary_key(column);
                let is_unique = column
                    .directives
                    .iter()
                    .any(|d| matches!(d, ast::ColumnDirective::Unique));
                let is_index = column
                    .directives
                    .iter()
                    .any(|d| matches!(d, ast::ColumnDirective::Index));

                if is_primary || is_unique || is_index {
                    if !is_first_index {
                        result.push_str(",\n");
                    }
                    is_first_index = false;

                    result.push_str("        {\n");
                    result.push_str(&format!(
                        "          field: {},\n",
                        crate::ext::string::quote(&column.name)
                    ));
                    result.push_str(&format!(
                        "          unique: {},\n",
                        if is_unique || is_primary {
                            "true"
                        } else {
                            "false"
                        }
                    ));
                    result.push_str(&format!(
                        "          primary: {}\n",
                        if is_primary { "true" } else { "false" }
                    ));
                    result.push_str("        }");
                }
            }
        }

        result.push_str("\n      ]\n");
        result.push_str("    }");
    }

    result.push_str("\n  },\n");
    result.push_str("  queryFieldToTable: {\n");

    let mut is_first_mapping = true;
    for table in &tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let query_field_name = crate::ext::string::decapitalize(&table.record.name);

        if !is_first_mapping {
            result.push_str(",\n");
        }
        is_first_mapping = false;

        result.push_str(&format!(
            "    {}: {}",
            string::quote(&query_field_name),
            string::quote(&table_name)
        ));
    }

    result.push_str("\n  }\n");
    result.push_str("};\n");
    result
}

fn get_linked_table<'a>(
    context: &'a typecheck::Context,
    link: &ast::LinkDetails,
) -> Option<&'a typecheck::Table> {
    context
        .tables
        .get(&crate::ext::string::decapitalize(&link.foreign.table))
}

fn to_zod_type(type_: &str) -> String {
    input_zod_type_for_column_type(&ast::ColumnType::from_str(type_))
}

fn input_zod_type_for_column_type(type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "z.string()".to_string(),
        ast::ColumnType::Int | ast::ColumnType::Float => "z.number()".to_string(),
        ast::ColumnType::Bool => "z.boolean()".to_string(),
        ast::ColumnType::DateTime => "z.union([z.date(), z.string(), z.number()])".to_string(),
        ast::ColumnType::Date => "z.string()".to_string(),
        ast::ColumnType::Json => "z.unknown()".to_string(),
        ast::ColumnType::JsonTyped(inner) => output_zod_type_for_column_type(inner),
        ast::ColumnType::List(inner) => {
            format!("z.array({})", input_zod_type_for_column_type(inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!("z.record({})", input_zod_type_for_column_type(inner))
        }
        ast::ColumnType::Nullable(inner) => {
            format!("{}.nullable()", input_zod_type_for_column_type(inner))
        }
        ast::ColumnType::IdInt { .. } | ast::ColumnType::ForeignKey { .. } => {
            "z.number()".to_string()
        }
        ast::ColumnType::IdUuid { .. } => "z.string()".to_string(),
        ast::ColumnType::Custom(name) => format!("Decode.{}", name),
    }
}

fn output_zod_type_for_column_type(type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "z.string()".to_string(),
        ast::ColumnType::Int | ast::ColumnType::Float => "z.number()".to_string(),
        ast::ColumnType::Bool => "CoercedBool".to_string(),
        ast::ColumnType::DateTime => "CoercedDate".to_string(),
        ast::ColumnType::Date => "z.string()".to_string(),
        ast::ColumnType::Json => "Decode.Json".to_string(),
        ast::ColumnType::JsonTyped(inner) => output_zod_type_for_column_type(inner),
        ast::ColumnType::List(inner) => {
            format!("z.array({})", output_zod_type_for_column_type(inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!("z.record({})", output_zod_type_for_column_type(inner))
        }
        ast::ColumnType::Nullable(inner) => {
            format!("{}.nullable()", output_zod_type_for_column_type(inner))
        }
        ast::ColumnType::IdInt { .. } | ast::ColumnType::ForeignKey { .. } => {
            "z.number()".to_string()
        }
        ast::ColumnType::IdUuid { .. } => "z.string()".to_string(),
        ast::ColumnType::Custom(name) => format!("Decode.{}", name),
    }
}

fn to_param_type_alias(args: &Vec<ast::QueryParamDefinition>) -> String {
    let mut result = "const RawInputValidator = z.object({".to_string();
    let mut is_first = true;
    for arg in args {
        let type_name = arg.type_.clone().unwrap_or("unknown".to_string());
        let mut type_string = to_zod_type(&type_name);
        if arg.nullable {
            type_string = format!("{}.nullable()", type_string);
        }
        if arg.omittable {
            type_string = format!("{}.optional()", type_string);
        }
        if is_first {
            result.push_str(&format!("\n  {}: {}", arg.name, type_string));
            is_first = false;
        } else {
            result.push_str(&format!(",\n  {}: {}", arg.name, type_string));
        }
    }
    result.push_str("\n});\n");
    result.push_str("const InputValidator = RawInputValidator;\n");

    result.push_str("export type Input = z.infer<typeof RawInputValidator>;");
    result
}
