use crate::ast;
use crate::filesystem;
use crate::generate::sql;
use crate::typecheck;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Serialize)]
struct Manifest {
    version: u32,
    session_schema: BTreeMap<String, FieldSchema>,
    queries: BTreeMap<String, QueryManifest>,
}

#[derive(Serialize)]
struct QueryManifest {
    id: String,
    operation: String,
    primary_db: String,
    attached_dbs: Vec<String>,
    input_schema: BTreeMap<String, FieldSchema>,
    session_args: Vec<String>,
    optional_input_args: Vec<String>,
    json_input_args: Vec<String>,
    sql: Vec<SqlInfo>,
    #[serde(rename = "syncSql", skip_serializing_if = "Option::is_none")]
    sync_sql: Option<Vec<SqlInfo>>,
}

#[derive(Serialize)]
struct FieldSchema {
    #[serde(rename = "type")]
    type_: String,
    is_enum: bool,
    enum_variants: Vec<String>,
    nullable: bool,
    omittable: bool,
}

#[derive(Serialize)]
struct SqlInfo {
    include: bool,
    params: Vec<String>,
    sql: String,
}

pub fn generate_schema(
    context: &typecheck::Context,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    write_manifest(context, Vec::new(), files);
}

pub fn generate_queries(
    context: &typecheck::Context,
    query_list: &ast::QueryList,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    let queries = query_list
        .queries
        .iter()
        .filter_map(|query_def| match query_def {
            ast::QueryDef::Query(query) => all_query_info
                .get(&query.name)
                .map(|query_info| query_manifest(context, query, query_info)),
            _ => None,
        })
        .collect();

    write_manifest(context, queries, files);
}

fn write_manifest(
    context: &typecheck::Context,
    queries: Vec<QueryManifest>,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    let manifest = Manifest {
        version: 1,
        session_schema: session_schema(context),
        queries: queries
            .into_iter()
            .map(|query| (query.id.clone(), query))
            .collect(),
    };
    let content = serde_json::to_string_pretty(&manifest).expect("manifest should serialize");

    files.retain(|file| file.path != Path::new("manifest.json"));
    files.push(filesystem::generate_text_file("manifest.json", content));
}

fn query_manifest(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
) -> QueryManifest {
    QueryManifest {
        id: query.interface_hash.clone(),
        operation: operation_to_string(&query.operation),
        primary_db: query_info.primary_db.clone(),
        attached_dbs: sorted_strings(&query_info.attached_dbs),
        input_schema: input_schema(context, query),
        session_args: session_args(&query_info.variables),
        optional_input_args: query
            .args
            .iter()
            .filter(|arg| arg.omittable)
            .map(|arg| arg.name.clone())
            .collect(),
        json_input_args: query
            .args
            .iter()
            .filter(|arg| {
                arg.type_
                    .as_ref()
                    .map(|type_name| ast::ColumnType::from_str(type_name).is_json_like())
                    .unwrap_or(false)
            })
            .map(|arg| arg.name.clone())
            .collect(),
        sql: query_sql(context, query, query_info, false),
        sync_sql: if query.operation == ast::QueryOperation::Query {
            None
        } else {
            Some(query_sql(context, query, query_info, true))
        },
    }
}

fn sorted_strings(values: &std::collections::HashSet<String>) -> Vec<String> {
    let mut result: Vec<String> = values.iter().cloned().collect();
    result.sort();
    result
}

fn input_schema(context: &typecheck::Context, query: &ast::Query) -> BTreeMap<String, FieldSchema> {
    query
        .args
        .iter()
        .map(|arg| {
            let type_ = arg
                .type_
                .as_deref()
                .map(|type_| typecheck::resolve_query_param_type(context, type_))
                .unwrap_or_else(|| "Json".to_string());
            let enum_variants = enum_variants(context, &type_);
            (
                arg.name.clone(),
                FieldSchema {
                    is_enum: !enum_variants.is_empty(),
                    enum_variants,
                    type_,
                    nullable: arg.nullable,
                    omittable: arg.omittable,
                },
            )
        })
        .collect()
}

fn session_schema(context: &typecheck::Context) -> BTreeMap<String, FieldSchema> {
    context
        .session
        .as_ref()
        .map(|session| {
            session
                .fields
                .iter()
                .filter_map(|field| match field {
                    ast::Field::Column(column) => {
                        let type_ = column.type_.to_string();
                        let enum_variants = enum_variants(context, &type_);
                        Some((
                            column.name.clone(),
                            FieldSchema {
                                is_enum: !enum_variants.is_empty(),
                                enum_variants,
                                type_,
                                nullable: column.nullable,
                                omittable: false,
                            },
                        ))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn enum_variants(context: &typecheck::Context, type_: &str) -> Vec<String> {
    match context.types.get(type_) {
        Some((crate::error::DefInfo::Def(_), typecheck::Type::OneOf { variants }))
            if variants.iter().all(|variant| variant.fields.is_none()) =>
        {
            variants
                .iter()
                .map(|variant| variant.name.clone())
                .collect()
        }
        _ => Vec::new(),
    }
}

fn query_sql(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    sync_mode: bool,
) -> Vec<SqlInfo> {
    let mut result = Vec::new();

    for field in &query.fields {
        let ast::TopLevelQueryField::Field(query_field) = field else {
            continue;
        };
        let Some(table) = context.tables.get(&query_field.name) else {
            continue;
        };
        let params = used_params(
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

        for prepared in prepared {
            if sync_mode && prepared.include && !prepared.sql.contains("_affectedRows") {
                continue;
            }
            result.push(SqlInfo {
                include: prepared.include,
                params: params.clone(),
                sql: prepared.sql,
            });
        }
    }

    result
}

fn used_params(
    query: &ast::Query,
    top_level_field_alias: &str,
    query_params: &HashMap<String, typecheck::ParamInfo>,
) -> Vec<String> {
    let mut result = Vec::new();

    for info in query_params.values() {
        let typecheck::ParamInfo::Defined {
            used_by_top_level_field_alias,
            raw_variable_name,
            from_session,
            ..
        } = info
        else {
            continue;
        };

        if !*from_session || used_by_top_level_field_alias.contains(top_level_field_alias) {
            result.push(raw_variable_name.clone());
        }
    }

    for arg in &query.args {
        if arg.omittable {
            result.push(format!("{}__is_set", arg.name));
        }
    }

    result.sort_unstable();
    result.dedup();
    result
}

fn session_args(params: &HashMap<String, typecheck::ParamInfo>) -> Vec<String> {
    let mut result = Vec::new();

    for info in params.values() {
        let typecheck::ParamInfo::Defined {
            from_session,
            used,
            session_name,
            ..
        } = info
        else {
            continue;
        };

        if *from_session && *used {
            if let Some(session_name) = session_name {
                result.push(session_name.clone());
            }
        }
    }

    result.sort_unstable();
    result.dedup();
    result
}

fn operation_to_string(operation: &ast::QueryOperation) -> String {
    match operation {
        ast::QueryOperation::Query => "query",
        ast::QueryOperation::Insert => "insert",
        ast::QueryOperation::Update => "update",
        ast::QueryOperation::Delete => "delete",
    }
    .to_string()
}
