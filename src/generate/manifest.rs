use crate::ast;
use crate::filesystem;
use crate::generate::sql;
use crate::server::manifest::{FieldSchema, GeneratedEdit, Manifest, QueryManifest, SqlInfo};
use crate::typecheck;
use std::collections::{HashMap, HashSet};
use std::path::Path;

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
        ephemeral: if context.states.is_empty() {
            None
        } else {
            Some(
                crate::ephemeral::Contract::from_context(context)
                    .expect("typechecked ephemeral state should resolve"),
            )
        },
    };
    // Serializing through Value keeps generated output stable even though runtime maps are HashMaps.
    let content = serde_json::to_string_pretty(
        &serde_json::to_value(manifest).expect("manifest should serialize"),
    )
    .expect("manifest should serialize");

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
                    .map(|type_name| {
                        typecheck::query_param_requires_json_serialization(context, type_name)
                    })
                    .unwrap_or(false)
            })
            .map(|arg| arg.name.clone())
            .collect(),
        sql: query_sql(context, query, query_info, false),
        generated_edit: crate::generated_queries::generated_crud_table(context, query).map(
            |table| GeneratedEdit {
                write_statement: sql::to_sql::format_attach(query_info).len(),
                sync_write_statement: sql::to_sql::format_attach(query_info).len()
                    + usize::from(query.operation == ast::QueryOperation::Update),
                create_id: ast::collect_columns(&table.record.fields)
                    .into_iter()
                    .find(|column| ast::is_primary_key(column))
                    .filter(|column| {
                        query.operation == ast::QueryOperation::Insert
                            && matches!(column.type_, ast::ColumnType::IdUuid { .. })
                    })
                    .map(|column| column.name.clone()),
            },
        ),
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

fn input_schema(context: &typecheck::Context, query: &ast::Query) -> HashMap<String, FieldSchema> {
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
                    tagged_union_variants: HashMap::new(),
                    tagged_union_types: HashMap::new(),
                    type_,
                    nullable: arg.nullable,
                    omittable: arg.omittable,
                },
            )
        })
        .collect()
}

fn session_schema(context: &typecheck::Context) -> HashMap<String, FieldSchema> {
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

fn session_field_schema(context: &typecheck::Context, column: &ast::Column) -> FieldSchema {
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
        tagged_union_types: HashMap::new(),
        type_,
        nullable: column.nullable,
        omittable: false,
    }
}

fn collect_session_tagged_union_types(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    visited: &mut HashSet<String>,
    definitions: &mut HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
) {
    let Some(type_name) = type_.get_custom_type_name() else {
        return;
    };
    if !visited.insert(type_name.to_string()) {
        return;
    }
    let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(type_name) else {
        return;
    };
    if variants.iter().all(|variant| variant.fields.is_none()) {
        return;
    }

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
        tagged_union_variants: HashMap::new(),
        tagged_union_types: HashMap::new(),
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
