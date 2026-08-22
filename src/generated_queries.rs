use crate::ast;
use crate::error::{Error, ErrorType, Location};
use crate::hash;
use crate::typecheck;

use std::collections::HashSet;

pub fn validate_generated_crud_name_collisions(
    query_list: &ast::QueryList,
    context: &typecheck::Context,
) -> Vec<Error> {
    let reserved = reserved_generated_crud_names(context);
    let mut errors = Vec::new();

    for query in &query_list.queries {
        let ast::QueryDef::Query(q) = query else {
            continue;
        };

        if let Some((table, operation)) = reserved.iter().find_map(|(name, table, operation)| {
            if name == &q.name {
                Some((table, operation))
            } else {
                None
            }
        }) {
            errors.push(Error {
                filepath: context.current_filepath.clone(),
                error_type: ErrorType::GeneratedCrudNameCollision {
                    name: q.name.clone(),
                    table: table.clone(),
                    operation: operation.clone(),
                },
                locations: vec![Location {
                    contexts: vec![],
                    primary: maybe_range(&q.start, &q.end),
                }],
            });
        }
    }

    errors
}

fn maybe_range(
    start: &Option<ast::Location>,
    end: &Option<ast::Location>,
) -> Vec<crate::error::Range> {
    match (start, end) {
        (Some(start), Some(end)) => vec![crate::error::Range {
            start: start.clone(),
            end: end.clone(),
        }],
        _ => vec![],
    }
}

pub fn append_generated_crud_queries(
    query_list: &mut ast::QueryList,
    context: &typecheck::Context,
) {
    let existing_names: HashSet<String> = query_list
        .queries
        .iter()
        .filter_map(|query| match query {
            ast::QueryDef::Query(q) => Some(q.name.clone()),
            _ => None,
        })
        .collect();

    let mut generated = generated_crud_queries(context)
        .into_iter()
        .filter(|query| !existing_names.contains(&query.name))
        .map(ast::QueryDef::Query)
        .collect::<Vec<ast::QueryDef>>();

    query_list.queries.append(&mut generated);
}

fn reserved_generated_crud_names(
    context: &typecheck::Context,
) -> Vec<(String, String, ast::QueryOperation)> {
    let mut result = Vec::new();

    for table in sorted_tables(context) {
        result.push((
            format!("{}Create", table.record.name),
            table.record.name.clone(),
            ast::QueryOperation::Insert,
        ));
        result.push((
            format!("{}Update", table.record.name),
            table.record.name.clone(),
            ast::QueryOperation::Update,
        ));
        result.push((
            format!("{}Delete", table.record.name),
            table.record.name.clone(),
            ast::QueryOperation::Delete,
        ));
    }

    result
}

fn generated_crud_queries(context: &typecheck::Context) -> Vec<ast::Query> {
    let mut result = Vec::new();

    for table in sorted_tables(context) {
        result.push(build_create_query(table));
        result.push(build_update_query(table));
        result.push(build_delete_query(table));
    }

    result
}

fn sorted_tables(context: &typecheck::Context) -> Vec<&typecheck::Table> {
    let mut tables = context.tables.values().collect::<Vec<&typecheck::Table>>();
    tables.sort_by(|a, b| a.record.name.cmp(&b.record.name));
    tables
}

fn build_create_query(table: &typecheck::Table) -> ast::Query {
    let writable_columns = writable_create_columns(table);
    let return_columns = scalar_return_columns(table);

    let args = writable_columns
        .iter()
        .map(|column| ast::QueryParamDefinition {
            name: column.name.clone(),
            type_: Some(typecheck::query_param_type_for_column(
                &table.record,
                column,
            )),
            nullable: column.nullable,
            omittable: column.nullable || ast::has_default_value(column),
            start_name: None,
            end_name: None,
            start_type: None,
            end_type: None,
        })
        .collect::<Vec<_>>();

    let mut fields = writable_columns
        .iter()
        .map(|column| field_assignment(&column.name))
        .collect::<Vec<_>>();

    for column in return_columns {
        if !fields.iter().any(|field| match field {
            ast::ArgField::Field(query_field) => query_field.name == column.name,
            _ => false,
        }) {
            fields.push(ast::ArgField::Field(selection_field(&column.name)));
        }
    }

    build_query(
        ast::QueryOperation::Insert,
        format!("{}Create", table.record.name),
        args,
        table_root_field(table, fields),
    )
}

fn build_update_query(table: &typecheck::Table) -> ast::Query {
    let primary_key = primary_key_column(table).expect("generated CRUD requires primary key");
    let writable_columns = writable_update_columns(table);
    let return_columns = scalar_return_columns(table);

    let mut args = vec![ast::QueryParamDefinition {
        name: primary_key.name.clone(),
        type_: Some(typecheck::query_param_type_for_column(
            &table.record,
            primary_key,
        )),
        nullable: false,
        omittable: false,
        start_name: None,
        end_name: None,
        start_type: None,
        end_type: None,
    }];

    args.extend(
        writable_columns
            .iter()
            .map(|column| ast::QueryParamDefinition {
                name: column.name.clone(),
                type_: Some(typecheck::query_param_type_for_column(
                    &table.record,
                    column,
                )),
                nullable: column.nullable,
                omittable: true,
                start_name: None,
                end_name: None,
                start_type: None,
                end_type: None,
            }),
    );

    let mut fields = vec![where_equals_field(&primary_key.name)];
    fields.extend(writable_columns.iter().map(|column| {
        ast::ArgField::Field(query_field_with_set(&column.name, variable(&column.name)))
    }));

    for column in return_columns {
        if !fields.iter().any(|field| match field {
            ast::ArgField::Field(query_field) => query_field.name == column.name,
            _ => false,
        }) {
            fields.push(ast::ArgField::Field(selection_field(&column.name)));
        }
    }

    build_query(
        ast::QueryOperation::Update,
        format!("{}Update", table.record.name),
        args,
        table_root_field(table, fields),
    )
}

fn build_delete_query(table: &typecheck::Table) -> ast::Query {
    let primary_key = primary_key_column(table).expect("generated CRUD requires primary key");
    let args = vec![ast::QueryParamDefinition {
        name: primary_key.name.clone(),
        type_: Some(typecheck::query_param_type_for_column(
            &table.record,
            primary_key,
        )),
        nullable: false,
        omittable: false,
        start_name: None,
        end_name: None,
        start_type: None,
        end_type: None,
    }];

    let fields = vec![
        where_equals_field(&primary_key.name),
        ast::ArgField::Field(selection_field(&primary_key.name)),
    ];

    build_query(
        ast::QueryOperation::Delete,
        format!("{}Delete", table.record.name),
        args,
        table_root_field(table, fields),
    )
}

fn build_query(
    operation: ast::QueryOperation,
    name: String,
    args: Vec<ast::QueryParamDefinition>,
    table_field: ast::TopLevelQueryField,
) -> ast::Query {
    let mut query = ast::Query {
        interface_hash: String::new(),
        full_hash: String::new(),
        operation,
        name,
        args,
        fields: vec![table_field],
        start: None,
        end: None,
    };

    query.interface_hash = hash::hash_query_interface(&query);
    query.full_hash = hash::hash_query_full(&query);
    query
}

fn table_root_field(
    table: &typecheck::Table,
    fields: Vec<ast::ArgField>,
) -> ast::TopLevelQueryField {
    ast::TopLevelQueryField::Field(ast::QueryField {
        operation: None,
        name: crate::ext::string::decapitalize(&table.record.name),
        alias: None,
        set: None,
        directives: vec![],
        fields,
        start_fieldname: None,
        end_fieldname: None,
        start: None,
        end: None,
    })
}

fn field_assignment(name: &str) -> ast::ArgField {
    ast::ArgField::Field(query_field_with_set(name, variable(name)))
}

fn selection_field(name: &str) -> ast::QueryField {
    query_field_with_set(name, None)
}

fn query_field_with_set(name: &str, set: Option<ast::QueryValue>) -> ast::QueryField {
    ast::QueryField {
        operation: None,
        name: name.to_string(),
        alias: None,
        set,
        directives: vec![],
        fields: vec![],
        start_fieldname: None,
        end_fieldname: None,
        start: None,
        end: None,
    }
}

fn where_equals_field(name: &str) -> ast::ArgField {
    ast::ArgField::Arg(ast::LocatedArg {
        arg: ast::Arg::Where(ast::WhereArg::Column(
            false,
            ast::PredicatePath::field(name),
            ast::Operator::Equal,
            variable_value(name),
            ast::empty_range(),
        )),
        start: None,
        end: None,
    })
}

fn variable(name: &str) -> Option<ast::QueryValue> {
    Some(variable_value(name))
}

fn variable_value(name: &str) -> ast::QueryValue {
    ast::QueryValue::Variable((
        ast::empty_range(),
        ast::VariableDetails {
            name: name.to_string(),
            session_field: None,
        },
    ))
}

fn writable_create_columns(table: &typecheck::Table) -> Vec<&ast::Column> {
    scalar_columns(table)
        .into_iter()
        .filter(|column| !ast::is_integer_primary_key(column))
        .filter(|column| !ast::is_managed_timestamp(column))
        .collect()
}

fn writable_update_columns(table: &typecheck::Table) -> Vec<&ast::Column> {
    scalar_columns(table)
        .into_iter()
        .filter(|column| !ast::is_primary_key(column))
        .filter(|column| !ast::is_managed_timestamp(column))
        .collect()
}

fn scalar_return_columns(table: &typecheck::Table) -> Vec<&ast::Column> {
    scalar_columns(table)
}

fn scalar_columns(table: &typecheck::Table) -> Vec<&ast::Column> {
    table
        .record
        .fields
        .iter()
        .filter_map(|field| match field {
            ast::Field::Column(column) => Some(column),
            _ => None,
        })
        .collect()
}

fn primary_key_column(table: &typecheck::Table) -> Option<&ast::Column> {
    scalar_columns(table)
        .into_iter()
        .find(|column| ast::is_primary_key(column))
}
