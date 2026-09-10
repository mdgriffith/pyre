use crate::ast;
use crate::filesystem;
use crate::generate::sql;
use crate::typecheck;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

#[derive(Serialize)]
struct Manifest {
    #[serde(rename = "compiledContract")]
    compiled_contract: String,
    version: u32,
    session_schema: BTreeMap<String, FieldSchema>,
    queries: BTreeMap<String, QueryManifest>,
}

#[derive(Serialize)]
struct QueryManifest {
    #[serde(rename = "compiledContract")]
    compiled_contract: String,
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
    #[serde(rename = "generatedEdit", skip_serializing_if = "Option::is_none")]
    generated_edit: Option<crate::server::manifest::GeneratedEdit>,
}

#[derive(Serialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    type_: String,
    is_enum: bool,
    enum_variants: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tagged_union_variants: BTreeMap<String, BTreeMap<String, FieldSchema>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tagged_union_types: BTreeMap<String, BTreeMap<String, BTreeMap<String, FieldSchema>>>,
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
        compiled_contract: compiled_contract(context, None),
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
        compiled_contract: compiled_contract(context, Some(query)),
        id: query.interface_hash.clone(),
        generated_edit: generated_edit_metadata(context, query, query_info),
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
                    .map(|type_name| {
                        typecheck::query_param_requires_json_serialization(context, type_name)
                    })
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

/// Compile the same artifact used by Rust before exporting its identity to other runtimes.
pub fn fingerprint(
    context: &typecheck::Context,
    queries: &ast::QueryList,
    info: &HashMap<String, typecheck::QueryInfo>,
) -> String {
    let mut files = Vec::new();
    generate_queries(context, queries, info, &mut files);
    let manifest: crate::server::manifest::Manifest =
        serde_json::from_str(&files[0].contents).expect("compiled manifest");
    manifest.fingerprint()
}

/// Schema/session identity used by replacement readers, independent of query projections.
pub fn compiled_schema_contract(context: &typecheck::Context) -> String {
    compiled_contract(context, None)
}

fn compiled_contract(context: &typecheck::Context, query: Option<&ast::Query>) -> String {
    use sha2::{Digest, Sha256};
    let mut definitions = BTreeMap::new();
    for (name, (_, type_)) in &context.types {
        if let typecheck::Type::OneOf { variants } = type_ {
            definitions.insert(
                name.clone(),
                ast::Definition::Tagged {
                    name: name.clone(),
                    variants: variants.clone(),
                    start: None,
                    end: None,
                },
            );
        }
    }
    let codec_database = ast::Database {
        schemas: vec![ast::Schema {
            files: vec![ast::SchemaFile {
                path: String::new(),
                definitions: definitions.into_values().collect(),
            }],
            session: context.session.clone(),
            ..ast::Schema::default()
        }],
    };
    let mut schemas = BTreeMap::new();
    for (name, table) in &context.tables {
        let file = ast::SchemaFile {
            path: String::new(),
            definitions: vec![ast::Definition::Record {
                name: table.record.name.clone(),
                fields: table.record.fields.clone(),
                start: None,
                end: None,
                start_name: None,
                end_name: None,
            }],
        };
        schemas.insert(
            format!("{}/{}", table.schema, name),
            crate::generate::to_string::schemafile_to_string(&table.schema, &file),
        );
    }
    let sync_modes = context
        .namespace_sync_modes
        .iter()
        .map(|(name, mode)| (name, mode.as_str()))
        .collect::<BTreeMap<_, _>>();
    let contract = serde_json::json!({
        "schema": schemas,
        "syncModes": sync_modes,
        "codecs": crate::generate::typescript::core::compiled_codec_contract(context, query, &codec_database),
        "runtimeContract": 1
    });
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&contract).expect("compiled contract"))
    )
}

pub fn generated_edit_metadata(
    context: &typecheck::Context,
    query: &ast::Query,
    info: &typecheck::QueryInfo,
) -> Option<crate::server::manifest::GeneratedEdit> {
    let table = query.fields.iter().find_map(|field| match field {
        ast::TopLevelQueryField::Field(field) => context.tables.get(&field.name),
        _ => None,
    })?;
    let (kind, _, writable_inputs) = crate::generated_queries::generated_edit(query, table)?;
    let sql = query_sql(context, query, info, false);
    let write_statement_indices = sql
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| {
            let sql = statement.sql.trim_start().to_ascii_lowercase();
            (sql.starts_with("insert ") || sql.starts_with("update ") || sql.starts_with("delete "))
                .then_some(index)
        })
        .collect();
    Some(crate::server::manifest::GeneratedEdit {
        kind: kind.to_string(),
        write_statement_indices,
        writable_inputs,
    })
}

fn sorted_strings(values: &std::collections::HashSet<String>) -> Vec<String> {
    let mut result: Vec<String> = values.iter().cloned().collect();
    result.sort();
    result
}

pub fn input_schema(
    context: &typecheck::Context,
    query: &ast::Query,
) -> BTreeMap<String, FieldSchema> {
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
            let mut definitions = BTreeMap::new();
            collect_session_tagged_union_types(
                context,
                &ast::ColumnType::from_str(&type_),
                &mut HashSet::new(),
                &mut definitions,
            );
            (
                arg.name.clone(),
                FieldSchema {
                    is_enum: !enum_variants.is_empty(),
                    enum_variants,
                    tagged_union_variants: BTreeMap::new(),
                    tagged_union_types: definitions,
                    type_,
                    nullable: arg.nullable,
                    omittable: arg.omittable,
                },
            )
        })
        .collect()
}

pub fn session_schema(context: &typecheck::Context) -> BTreeMap<String, FieldSchema> {
    context
        .session
        .as_ref()
        .map(|session| {
            session
                .fields
                .iter()
                .filter_map(|field| match field {
                    ast::Field::Column(column) => {
                        Some((column.name.clone(), session_field_schema(context, column)))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn session_field_schema(
    context: &typecheck::Context,
    column: &ast::Column,
) -> FieldSchema {
    let mut schema = session_field_schema_inner(context, column, &mut HashSet::new());
    collect_session_tagged_union_types(
        context,
        &column.type_,
        &mut HashSet::new(),
        &mut schema.tagged_union_types,
    );
    schema
}

fn session_field_schema_inner(
    context: &typecheck::Context,
    column: &ast::Column,
    visiting: &mut HashSet<String>,
) -> FieldSchema {
    let type_ = column.type_.query_type_string();
    let enum_variants = enum_variants(context, &type_);
    let tagged_union_variants = column
        .type_
        .get_custom_type_name()
        .and_then(|type_name| {
            if !visiting.insert(type_name.to_string()) {
                return None;
            }
            let result = context
                .types
                .get(type_name)
                .and_then(|(_, type_)| match type_ {
                    typecheck::Type::OneOf { variants }
                        if variants.iter().any(|variant| variant.fields.is_some()) =>
                    {
                        Some(
                            variants
                                .iter()
                                .map(|variant| {
                                    let fields = variant
                                        .fields
                                        .as_ref()
                                        .map(|fields| {
                                            fields
                                                .iter()
                                                .filter_map(|field| match field {
                                                    ast::Field::Column(column) => Some((
                                                        column.name.clone(),
                                                        session_field_schema_inner(
                                                            context, column, visiting,
                                                        ),
                                                    )),
                                                    _ => None,
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    (variant.name.clone(), fields)
                                })
                                .collect(),
                        )
                    }
                    _ => None,
                });
            visiting.remove(type_name);
            result
        })
        .unwrap_or_default();

    FieldSchema {
        is_enum: !enum_variants.is_empty(),
        enum_variants,
        tagged_union_variants,
        tagged_union_types: BTreeMap::new(),
        type_,
        nullable: column.nullable,
        omittable: false,
    }
}

fn collect_session_tagged_union_types(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    visited: &mut HashSet<String>,
    definitions: &mut BTreeMap<String, BTreeMap<String, BTreeMap<String, FieldSchema>>>,
) {
    match type_ {
        ast::ColumnType::List(inner)
        | ast::ColumnType::Dict(inner)
        | ast::ColumnType::JsonTyped(inner)
        | ast::ColumnType::Nullable(inner) => {
            collect_session_tagged_union_types(context, inner, visited, definitions);
            return;
        }
        _ => {}
    }
    let Some(type_name) = type_.get_custom_type_name() else {
        return;
    };
    if !visited.insert(type_name.to_string()) {
        return;
    }
    let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(type_name) else {
        return;
    };
    let variant_schemas = variants
        .iter()
        .map(|variant| {
            let fields = variant
                .fields
                .as_ref()
                .map(|fields| {
                    fields
                        .iter()
                        .filter_map(|field| match field {
                            ast::Field::Column(column) => Some((
                                column.name.clone(),
                                session_field_schema_reference(context, column),
                            )),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            (variant.name.clone(), fields)
        })
        .collect();
    definitions.insert(type_name.to_string(), variant_schemas);

    for variant in variants {
        if let Some(fields) = &variant.fields {
            for field in fields {
                if let ast::Field::Column(column) = field {
                    collect_session_tagged_union_types(
                        context,
                        &column.type_,
                        visited,
                        definitions,
                    );
                }
            }
        }
    }
}

fn session_field_schema_reference(
    context: &typecheck::Context,
    column: &ast::Column,
) -> FieldSchema {
    let type_ = column.type_.query_type_string();
    let enum_variants = enum_variants(context, &type_);
    FieldSchema {
        is_enum: !enum_variants.is_empty(),
        enum_variants,
        tagged_union_variants: BTreeMap::new(),
        tagged_union_types: BTreeMap::new(),
        type_,
        nullable: column.nullable,
        omittable: false,
    }
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

        let prepared =
            if *ast::query_field_operation(query, query_field) == ast::QueryOperation::Query {
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
        ast::QueryOperation::Transaction => "transaction",
    }
    .to_string()
}
