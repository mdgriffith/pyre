use crate::{ast, typecheck};
use serde_json::{json, Value};
use std::collections::BTreeMap;

// Keep semantic AST distinctions rather than using the human-facing formatter.
// Maps are sorted; sequences retain their AST order. Source metadata is excluded.
pub(super) fn structural_schema(context: &typecheck::Context) -> BTreeMap<String, Value> {
    let mut schema = BTreeMap::new();
    for (name, (_, type_)) in &context.types {
        let value = match type_ {
            typecheck::Type::Integer => json!(["Integer"]),
            typecheck::Type::Float => json!(["Float"]),
            typecheck::Type::String => json!(["String"]),
            typecheck::Type::Record(record) => {
                json!(["Record", record.name, fields(&record.fields)])
            }
            typecheck::Type::OneOf { variants } => json!([
                "OneOf",
                variants
                    .iter()
                    .map(|variant| { json!([variant.name, variant.fields.as_deref().map(fields)]) })
                    .collect::<Vec<_>>()
            ]),
        };
        schema.insert(format!("type:{name}"), value);
    }
    if let Some(session) = &context.session {
        schema.insert("session".into(), json!(fields(&session.fields)));
    }
    for (name, table) in &context.tables {
        schema.insert(
            format!("table:{}:{name}", table.schema),
            json!({
                "namespace": table.schema,
                "name": table.record.name,
                "fields": fields(&table.record.fields),
            }),
        );
    }
    schema
}

fn fields(values: &[ast::Field]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|field| match field {
            ast::Field::Column(column) => Some(json!(["Column", {
                "name": column.name,
                "type": column_type(&column.type_),
                "nullable": column.nullable,
                "directives": column.directives.iter().map(column_directive).collect::<Vec<_>>(),
            }])),
            ast::Field::FieldDirective(directive) => Some(field_directive(directive)),
            ast::Field::ColumnLines { .. } | ast::Field::ColumnComment { .. } => None,
        })
        .collect()
}

fn column_type(value: &ast::ColumnType) -> Value {
    use ast::ColumnType::*;
    match value {
        String => json!(["String"]),
        Int => json!(["Int"]),
        Float => json!(["Float"]),
        Bool => json!(["Bool"]),
        DateTime => json!(["DateTime"]),
        Date => json!(["Date"]),
        Json => json!(["Json"]),
        JsonTyped(inner) => json!(["JsonTyped", column_type(inner)]),
        List(inner) => json!(["List", column_type(inner)]),
        Dict(inner) => json!(["Dict", column_type(inner)]),
        Nullable(inner) => json!(["Nullable", column_type(inner)]),
        IdInt { table } => json!(["IdInt", table]),
        IdUuid { table } => json!(["IdUuid", table]),
        ForeignKey {
            schema,
            table,
            field,
            serialization_type,
        } => json!([
            "ForeignKey",
            schema,
            table,
            field,
            serialization_type.as_ref().map(serialization)
        ]),
        Custom(name) => json!(["Custom", name]),
    }
}

fn serialization(value: &ast::ConcreteSerializationType) -> Value {
    use ast::ConcreteSerializationType::*;
    match value {
        Integer => json!(["Integer"]),
        Real => json!(["Real"]),
        Text => json!(["Text"]),
        Blob => json!(["Blob"]),
        Date => json!(["Date"]),
        DateTime => json!(["DateTime"]),
        JsonB => json!(["JsonB"]),
        IdInt => json!(["IdInt"]),
        IdUuid => json!(["IdUuid"]),
        VectorBlob {
            vector_type,
            dimensionality,
        } => json!([
            "VectorBlob",
            match vector_type {
                ast::VectorType::Float64 => "Float64",
                ast::VectorType::Float32 => "Float32",
                ast::VectorType::Float16 => "Float16",
                ast::VectorType::BFloat16 => "BFloat16",
                ast::VectorType::Float8 => "Float8",
                ast::VectorType::Float1 => "Float1",
            },
            dimensionality
        ]),
    }
}

fn column_directive(value: &ast::ColumnDirective) -> Value {
    use ast::ColumnDirective::*;
    match value {
        PrimaryKey => json!(["PrimaryKey"]),
        Unique => json!(["Unique"]),
        Index => json!(["Index"]),
        Immutable => json!(["Immutable"]),
        CreatedAt => json!(["CreatedAt"]),
        UpdatedAt => json!(["UpdatedAt"]),
        Default {
            id,
            value,
            start: _,
            end: _,
        } => json!([
            "Default",
            id,
            match value {
                ast::DefaultValue::Now => json!(["Now"]),
                ast::DefaultValue::Value(value) => json!(["Value", query_value(value)]),
            }
        ]),
    }
}

fn field_directive(value: &ast::FieldDirective) -> Value {
    use ast::FieldDirective::*;
    match value {
        Watched(details) => json!([
            "Watched",
            details.selects,
            details.inserts,
            details.updates,
            details.deletes
        ]),
        TableName((_, name)) => json!(["TableName", name]),
        Link(link) => json!(["Link", link.link_name, link.local_ids, {
            "schema": link.foreign.schema,
            "table": link.foreign.table,
            "fields": link.foreign.fields,
        }]),
        Index(details) => json!(["Index", index(details)]),
        Unique(details) => json!(["Unique", index(details)]),
        Permissions(details) => json!([
            "Permissions",
            match details {
                ast::PermissionDetails::Public => json!(["Public"]),
                ast::PermissionDetails::Star(value) => json!(["Star", predicate(value)]),
                ast::PermissionDetails::OnOperation(operations) => json!([
                    "OnOperation",
                    operations
                        .iter()
                        .map(|op| {
                            json!([
                                op.operations
                                    .iter()
                                    .map(ast::QueryOperation::as_str)
                                    .collect::<Vec<_>>(),
                                predicate(&op.where_)
                            ])
                        })
                        .collect::<Vec<_>>()
                ]),
            }
        ]),
        Singleton => json!(["Singleton"]),
        Timestamps => json!(["Timestamps"]),
    }
}

fn index(value: &ast::IndexDirective) -> Value {
    json!({
        "columns": value.columns.iter().map(|column| json!([column.name, match column.direction {
            ast::SortDirection::Asc => "Asc",
            ast::SortDirection::Desc => "Desc",
        }])).collect::<Vec<_>>(),
        "where": value.where_.as_ref().map(predicate),
    })
}

fn predicate(value: &ast::WhereArg) -> Value {
    match value {
        ast::WhereArg::Constant(value) => json!(["Constant", value]),
        ast::WhereArg::Column(session, path, operator, value, _) => json!([
            "Column",
            session,
            path.segments
                .iter()
                .map(|segment| match segment {
                    ast::PredicatePathSegment::Field(name) => json!(["Field", name]),
                    ast::PredicatePathSegment::Variant(name) => json!(["Variant", name]),
                })
                .collect::<Vec<_>>(),
            match operator {
                ast::Operator::Equal => "Equal",
                ast::Operator::NotEqual => "NotEqual",
                ast::Operator::GreaterThan => "GreaterThan",
                ast::Operator::LessThan => "LessThan",
                ast::Operator::GreaterThanOrEqual => "GreaterThanOrEqual",
                ast::Operator::LessThanOrEqual => "LessThanOrEqual",
                ast::Operator::In => "In",
                ast::Operator::NotIn => "NotIn",
                ast::Operator::Like => "Like",
                ast::Operator::NotLike => "NotLike",
            },
            query_value(value)
        ]),
        ast::WhereArg::Exists(path, value) => json!([
            "Exists",
            path.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            predicate(value)
        ]),
        ast::WhereArg::And(values) => {
            json!(["And", values.iter().map(predicate).collect::<Vec<_>>()])
        }
        ast::WhereArg::Or(values) => {
            json!(["Or", values.iter().map(predicate).collect::<Vec<_>>()])
        }
    }
}

fn query_value(value: &ast::QueryValue) -> Value {
    match value {
        ast::QueryValue::Fn(details) => json!([
            "Fn",
            details.name,
            details.args.iter().map(query_value).collect::<Vec<_>>()
        ]),
        ast::QueryValue::LiteralTypeValue((_, details)) => json!([
            "LiteralTypeValue",
            details.name,
            details.fields.as_ref().map(|fields| {
                fields
                    .iter()
                    .map(|(name, value)| json!([name, query_value(value)]))
                    .collect::<Vec<_>>()
            })
        ]),
        ast::QueryValue::Variable((_, details)) => {
            json!(["Variable", details.name, details.session_field])
        }
        ast::QueryValue::String((_, value)) => json!(["String", value]),
        ast::QueryValue::Int((_, value)) => json!(["Int", value]),
        // Preserve every f32 value, including non-finite values that JSON numbers cannot encode.
        ast::QueryValue::Float((_, value)) => json!(["Float", value.to_bits()]),
        ast::QueryValue::Bool((_, value)) => json!(["Bool", value]),
        ast::QueryValue::Null(_) => json!(["Null"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_key_storage_is_not_lost_inside_containers() {
        let type_ = |serialization_type| {
            ast::ColumnType::List(Box::new(ast::ColumnType::ForeignKey {
                schema: Some("Accounts".into()),
                table: "User".into(),
                field: "id".into(),
                serialization_type,
            }))
        };
        let int = type_(Some(ast::ConcreteSerializationType::IdInt));
        let uuid = type_(Some(ast::ConcreteSerializationType::IdUuid));
        assert_eq!(int.to_string(), uuid.to_string());
        assert_ne!(column_type(&int), column_type(&uuid));
        assert_ne!(column_type(&int), column_type(&type_(None)));
    }

    #[test]
    fn nested_defaults_preserve_values_but_not_locations() {
        let default = |number, range: ast::Range| ast::ColumnDirective::Default {
            id: "default".into(),
            value: ast::DefaultValue::Value(ast::QueryValue::LiteralTypeValue((
                range.clone(),
                ast::LiteralTypeValueDetails {
                    name: "Outer".into(),
                    fields: Some(vec![(
                        "inner".into(),
                        ast::QueryValue::LiteralTypeValue((
                            range.clone(),
                            ast::LiteralTypeValueDetails {
                                name: "Inner".into(),
                                fields: Some(vec![(
                                    "count".into(),
                                    ast::QueryValue::Int((range.clone(), number)),
                                )]),
                            },
                        )),
                    )]),
                },
            ))),
            start: Some(range.start),
            end: Some(range.end),
        };
        let original = column_directive(&default(1, ast::empty_range()));
        assert_ne!(original, column_directive(&default(2, ast::empty_range())));
        let mut moved = ast::empty_range();
        moved.start.line = 42;
        moved.end.offset = 1000;
        assert_eq!(original, column_directive(&default(1, moved)));
    }
}
