use crate::{
    ast::*,
    typecheck::{Context, Type},
};
use serde::{Deserialize, Serialize};
#[cfg(feature = "json")]
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Non-fatal diagnostics for authored declarations, emitted by check and generate.
pub fn deprecation_diagnostics(database: &Database) -> Vec<String> {
    let mut diagnostics = Vec::new();
    for file in database.schemas.iter().flat_map(|schema| &schema.files) {
        let mut emit =
            |name: String| diagnostics.push(format!("{}: {name} is deprecated", file.path));
        for definition in &file.definitions {
            match definition {
                Definition::Record { name, fields, .. } => {
                    if record_lifecycle(fields) == Lifecycle::Deprecated {
                        emit(name.clone());
                    }
                    for column in collect_columns(fields) {
                        if column_lifecycle(&column) == Lifecycle::Deprecated {
                            emit(format!("{name}.{}", column.name));
                        }
                    }
                }
                Definition::Tagged { name, variants, .. } => {
                    for variant in variants {
                        if variant.lifecycle == Lifecycle::Deprecated {
                            emit(format!("{name}.{}", variant.name));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    diagnostics.sort();
    diagnostics
}

fn variants<'a>(context: &'a Context, name: &str) -> Option<&'a [Variant]> {
    match context.types.get(name) {
        Some((_, Type::OneOf { variants })) => Some(variants),
        _ => None,
    }
}

pub fn has_no_writable_variant(context: &Context, type_: &ColumnType) -> bool {
    match type_ {
        ColumnType::JsonTyped(inner) => has_no_writable_variant(context, inner),
        ColumnType::Custom(name) => variants(context, name)
            .is_some_and(|vs| vs.iter().all(|v| v.lifecycle == Lifecycle::Retired)),
        _ => false,
    }
}

pub fn has_retired_variant(context: &Context, type_: &ColumnType) -> bool {
    fn visit(context: &Context, type_: &ColumnType, seen: &mut HashSet<String>) -> bool {
        match type_ {
            ColumnType::JsonTyped(t)
            | ColumnType::List(t)
            | ColumnType::Dict(t)
            | ColumnType::Nullable(t) => visit(context, t, seen),
            ColumnType::Custom(name) if seen.insert(name.clone()) => variants(context, name)
                .is_some_and(|vs| {
                    vs.iter().any(|v| {
                        v.lifecycle == Lifecycle::Retired
                            || v.fields.iter().flatten().any(|field| match field {
                                Field::Column(c) => visit(context, &c.type_, seen),
                                _ => false,
                            })
                    })
                }),
            _ => false,
        }
    }
    visit(context, type_, &mut HashSet::new())
}

pub fn forbidden_assignment(
    context: &Context,
    type_: &ColumnType,
    value: &QueryValue,
) -> Option<String> {
    match value {
        QueryValue::Variable((_, variable)) if variable.session_field.is_some() && has_retired_variant(context, type_) => {
            Some("Use a validated write parameter to assign a type with retired variants, rather than copying a Session value.".into())
        }
        QueryValue::Fn(_) if has_retired_variant(context, type_) => Some("Use a validated write parameter or constructor to assign a type with retired variants.".into()),
        QueryValue::LiteralTypeValue((_, details)) => {
            let name = type_.get_custom_type_name()?;
            let variant = variants(context, name)?.iter().find(|v| v.name == details.name)?;
            if variant.lifecycle == Lifecycle::Retired {
                return Some(format!("Cannot construct retired variant {name}.{}.", variant.name));
            }
            let columns = collect_columns(variant.fields.as_deref().unwrap_or_default());
            details.fields.iter().flatten().find_map(|(name, value)| {
                forbidden_assignment(context, &columns.iter().find(|c| c.name == *name)?.type_, value)
            })
        }
        _ => None,
    }
}

pub const WRITE_TYPE_PREFIX: &str = "_PyreWrite_";

pub fn write_type(context: &Context, type_: &ColumnType) -> ColumnType {
    match type_ {
        ColumnType::Custom(name) if has_retired_variant(context, type_) => {
            ColumnType::Custom(format!("{WRITE_TYPE_PREFIX}{name}"))
        }
        ColumnType::JsonTyped(inner) => ColumnType::JsonTyped(Box::new(write_type(context, inner))),
        ColumnType::Nullable(inner) => ColumnType::Nullable(Box::new(write_type(context, inner))),
        ColumnType::List(inner) => ColumnType::List(Box::new(write_type(context, inner))),
        ColumnType::Dict(inner) => ColumnType::Dict(Box::new(write_type(context, inner))),
        _ => type_.clone(),
    }
}

pub fn write_parameters(context: &Context, query: &Query) -> BTreeSet<String> {
    fn value(
        context: &Context,
        type_: &ColumnType,
        assignment: &QueryValue,
        names: &mut BTreeSet<String>,
    ) {
        match assignment {
            QueryValue::Variable((_, v))
                if v.session_field.is_none() && has_retired_variant(context, type_) =>
            {
                names.insert(v.name.clone());
            }
            QueryValue::LiteralTypeValue((_, literal)) => {
                let Some(v) = type_
                    .get_custom_type_name()
                    .and_then(|name| variants(context, name))
                    .and_then(|vs| vs.iter().find(|v| v.name == literal.name))
                else {
                    return;
                };
                let columns = collect_columns(v.fields.as_deref().unwrap_or_default());
                for (name, assignment) in literal.fields.iter().flatten() {
                    if let Some(column) = columns.iter().find(|c| c.name == *name) {
                        value(context, &column.type_, assignment, names);
                    }
                }
            }
            _ => {}
        }
    }
    fn visit_record(
        context: &Context,
        record: &RecordDetails,
        selection: &QueryField,
        names: &mut BTreeSet<String>,
    ) {
        for field in &selection.fields {
            let ArgField::Field(field) = field else {
                continue;
            };
            for schema_field in &record.fields {
                match schema_field {
                    Field::Column(column) if column.name == field.name => {
                        if let Some(set) = &field.set {
                            value(context, &column.type_, set, names);
                            if let QueryValue::LiteralTypeValue((_, literal)) = set {
                                let columns = column
                                    .type_
                                    .get_custom_type_name()
                                    .and_then(|name| variants(context, name))
                                    .and_then(|vs| vs.iter().find(|v| v.name == literal.name))
                                    .and_then(|v| v.fields.as_ref());
                                if let Some(columns) = columns {
                                    for arg in &field.fields {
                                        if let ArgField::Field(field) = arg {
                                            if let (Some(c), Some(set)) = (
                                                collect_columns(columns)
                                                    .into_iter()
                                                    .find(|c| c.name == field.name),
                                                &field.set,
                                            ) {
                                                value(context, &c.type_, set, names);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Field::FieldDirective(FieldDirective::Link(link))
                        if link.link_name == field.name =>
                    {
                        if let Some(table) = crate::typecheck::get_linked_table(context, link) {
                            visit_record(context, &table.record, field, names);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut names = BTreeSet::new();
    for field in &query.fields {
        let TopLevelQueryField::Field(field) = field else {
            continue;
        };
        if matches!(
            field.operation.as_ref().unwrap_or(&query.operation),
            QueryOperation::Insert | QueryOperation::Update
        ) {
            if let Some(table) = context.tables.get(&field.name) {
                visit_record(context, &table.record, field, &mut names);
            }
        }
    }
    names
}

#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct WriteTypes {
    parameters: BTreeMap<String, String>,
    unions: BTreeMap<String, BTreeMap<String, Option<BTreeMap<String, String>>>>,
}

impl WriteTypes {
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    pub fn for_query(context: &Context, query: &Query) -> Self {
        let parameters = write_parameters(context, query);
        let mut types = Self::default();
        for parameter in &query.args {
            if let Some(type_) = parameter
                .type_
                .as_ref()
                .filter(|_| parameters.contains(&parameter.name))
            {
                types
                    .parameters
                    .insert(parameter.name.clone(), type_.clone());
            }
        }
        if types.is_empty() {
            return types;
        }
        for name in context.types.keys() {
            if !has_retired_variant(context, &ColumnType::Custom(name.clone())) {
                continue;
            }
            let Some(variants) = variants(context, name) else {
                continue;
            };
            types.unions.insert(
                name.clone(),
                variants
                    .iter()
                    .map(|v| {
                        (
                            v.name.clone(),
                            (v.lifecycle != Lifecycle::Retired).then(|| {
                                collect_columns(v.fields.as_deref().unwrap_or_default())
                                    .into_iter()
                                    .filter(|c| has_retired_variant(context, &c.type_))
                                    .map(|c| (c.name.clone(), c.type_.to_string()))
                                    .collect()
                            }),
                        )
                    })
                    .collect(),
            );
        }
        types
    }

    #[cfg(feature = "json")]
    pub fn validate(&self, name: &str, value: &Value) -> Result<(), String> {
        if let Some(type_) = self.parameters.get(name) {
            self.validate_type(&ColumnType::from_str(type_), value)?;
        }
        Ok(())
    }

    #[cfg(feature = "json")]
    fn validate_type(&self, type_: &ColumnType, value: &Value) -> Result<(), String> {
        match type_ {
            ColumnType::JsonTyped(inner) | ColumnType::Nullable(inner) => {
                self.validate_type(inner, value)?
            }
            ColumnType::List(inner) => {
                for entry in value.as_array().into_iter().flatten() {
                    self.validate_type(inner, entry)?;
                }
            }
            ColumnType::Dict(inner) => {
                for entry in value.as_object().into_iter().flat_map(|map| map.values()) {
                    self.validate_type(inner, entry)?;
                }
            }
            ColumnType::Custom(name) => {
                let Some(variants) = self.unions.get(name) else {
                    return Ok(());
                };
                let tag = value
                    .as_str()
                    .or_else(|| value.get("_type").and_then(Value::as_str));
                if let Some((tag, fields)) = tag.and_then(|tag| variants.get_key_value(tag)) {
                    let fields = fields
                        .as_ref()
                        .ok_or_else(|| format!("Cannot construct retired variant {name}.{tag}."))?;
                    for (field, type_) in fields {
                        if let Some(value) = value.get(field) {
                            self.validate_type(&ColumnType::from_str(type_), value)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
