use super::{elm_database_namespace, to_elm_typename, to_param_encoder_str, ElmLookup};
use crate::ast::{self, ColumnType, QueryOperation};
use crate::ext::string;
use crate::filesystem::{generate_text_file, GeneratedFile};
use crate::typecheck;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;
use std::path::Path;

struct Field<'a> {
    arg: &'a ast::QueryParamDefinition,
    variant: String,
    automatic_id: bool,
}

impl Field<'_> {
    fn required(&self) -> bool {
        !self.automatic_id && !self.arg.omittable
    }

    fn read(&self) -> String {
        format!("read{}", self.variant)
    }
}

struct Record<'a> {
    name: &'a str,
    create: Option<Vec<Field<'a>>>,
    update: Option<Vec<Field<'a>>>,
}

pub(super) fn generate(
    context: &typecheck::Context,
    info: &HashMap<String, typecheck::QueryInfo>,
    base: &Path,
    files: &mut Vec<GeneratedFile<String>>,
    records: &[(String, Vec<&ast::Query>)],
) {
    let lookup = ElmLookup::from_context(context);
    let mut by_namespace = BTreeMap::<_, Vec<Record>>::new();
    let mut used_names = BTreeMap::<_, BTreeSet<String>>::new();
    for (name, queries) in records {
        let Some(first) = queries.first() else {
            continue;
        };
        let table = crate::generated_queries::generated_crud_table(context, first).unwrap();
        let namespace = &info[&first.name].primary_db;
        let key = ast::collect_columns(&table.record.fields)
            .into_iter()
            .find(|column| ast::is_primary_key(column))
            .unwrap();
        let mut names = BTreeMap::new();
        let mut collect = |operation| {
            let query = queries.iter().find(|query| query.operation == operation)?;
            let mut fields = Vec::new();
            for arg in &query.args {
                let is_key = arg.name == key.name;
                if operation == QueryOperation::Update && is_key {
                    continue;
                }
                let variant = names
                    .entry(arg.name.as_str())
                    .or_insert_with(|| {
                        let used = used_names.entry(namespace).or_default();
                        let mut name = format!("{name}_{}", string::capitalize(&arg.name));
                        while !used.insert(name.clone()) {
                            name.push('_');
                        }
                        name
                    })
                    .clone();
                fields.push(Field {
                    arg,
                    variant,
                    automatic_id: is_key && matches!(key.type_, ColumnType::IdUuid { .. }),
                });
            }
            Some(fields)
        };
        let create = collect(QueryOperation::Insert);
        let update = collect(QueryOperation::Update);
        if create.is_some() || update.is_some() {
            by_namespace.entry(namespace).or_default().push(Record {
                name,
                create,
                update,
            });
        }
    }

    for (path, source) in [
        (
            "Db/Editor.elm",
            include_str!("../static/elm/src/Db/Editor.elm"),
        ),
        (
            "Db/Editor/Field.elm",
            include_str!("../static/elm/src/Db/Editor/Field.elm"),
        ),
        (
            "Db/Internal/Editor.elm",
            include_str!("../static/elm/src/Db/Internal/Editor.elm"),
        ),
    ] {
        files.push(generate_text_file(base.join(path), source));
    }

    for (namespace, records) in by_namespace {
        let namespace = elm_database_namespace(namespace);
        let variants = records
            .iter()
            .flat_map(|record| record.create.iter().chain(record.update.iter()).flatten())
            .map(|field| (&field.variant, field.arg))
            .collect::<BTreeMap<_, _>>();
        let constructors = variants
            .into_iter()
            .map(|(variant, arg)| {
                let type_ = to_elm_typename(&lookup, arg.type_.as_deref().unwrap(), false);
                format!("{variant} ({type_})")
            })
            .collect::<Vec<_>>();
        let imports = "import Db\nimport Db.Id\nimport Dict exposing (Dict)\nimport Time\n";
        let input = if constructors.is_empty() {
            // No writable values exist; `Never` keeps inputs unconstructable.
            format!("module Db.Editor.Input.{namespace} exposing (Input)\n\n\ntype alias Input =\n    Never\n")
        } else {
            format!("module Db.Editor.Input.{namespace} exposing (Input(..))\n\n{imports}\n\ntype Input\n    = {}\n", constructors.join("\n    | "))
        };
        files.push(generate_text_file(
            base.join(format!("Db/Editor/Input/{namespace}.elm")),
            input,
        ));
        for record in records {
            files.push(generate_text_file(
                base.join(format!("Db/Edit/{}/Editor.elm", record.name)),
                render_record(context, &lookup, &namespace, &record, constructors.len()),
            ));
        }
    }
}

fn render_record(
    context: &typecheck::Context,
    lookup: &ElmLookup,
    namespace: &str,
    record: &Record,
    variants: usize,
) -> String {
    let name = record.name;
    let mut exposed = Vec::new();
    let mut body = format!("import Db\nimport Db.Edit.{name}.Draft as Records\nimport Db.Editor as Editor\nimport Db.Editor.Field as Field\nimport Db.Editor.Input.{namespace} as Input\nimport Db.Encode\nimport Db.Id\nimport Db.Internal.Editor as Internal\nimport Dict exposing (Dict)\nimport Json.Encode as Encode\nimport Time\n");
    if let Some(fields) = &record.create {
        exposed.push("createForm");
        render_form(context, lookup, &mut body, fields, true);
    }
    if let Some(fields) = &record.update {
        exposed.push("editForm");
        render_form(context, lookup, &mut body, fields, false);
    }
    let fields = record
        .create
        .iter()
        .chain(record.update.iter())
        .flatten()
        .map(|field| (&field.variant, field))
        .collect::<BTreeMap<_, _>>();
    for field in fields.values() {
        let read = field.read();
        let variant = &field.variant;
        let type_ = to_elm_typename(lookup, field.arg.type_.as_deref().unwrap(), false);
        write!(body, "\n\n{read} : Input.Input -> Maybe ({type_})\n{read} input =\n    case input of\n        Input.{variant} value ->\n            Just value\n").unwrap();
        if variants > 1 {
            body.push_str("\n        _ ->\n            Nothing\n");
        }
    }
    format!(
        "module Db.Edit.{name}.Editor exposing ({})\n\n{body}",
        exposed.join(", ")
    )
}

fn render_form(
    context: &typecheck::Context,
    lookup: &ElmLookup,
    body: &mut String,
    fields: &[Field],
    create: bool,
) {
    let result = if create { "Draft" } else { "Patch" };
    let signature = format!("Editor.Form Input.Input Records.{result}");
    if create {
        write!(body, "\n\ncreateForm : {signature}\ncreateForm =\n").unwrap();
    } else {
        let baseline = fields
            .iter()
            .map(|field| {
                let arg = field.arg;
                let type_ = to_elm_typename(lookup, arg.type_.as_deref().unwrap(), false);
                if arg.nullable {
                    format!("{} : Maybe ({type_})", arg.name)
                } else {
                    format!("{} : {type_}", arg.name)
                }
            })
            .collect::<Vec<_>>();
        let baseline = if baseline.is_empty() {
            "row".into()
        } else {
            format!("{{ row | {} }}", baseline.join(", "))
        };
        let row = if fields.is_empty() { "_" } else { "row" };
        write!(
            body,
            "\n\neditForm : {baseline} -> {signature}\neditForm {row} =\n"
        )
        .unwrap();
    }
    let mut entries = Vec::new();
    let mut output = Vec::new();
    let mut required = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let arg = field.arg;
        let key = string::quote(&arg.name);
        let variant = format!("Input.{}", field.variant);
        let read = field.read();
        let original = if create {
            "Nothing".to_string()
        } else if arg.nullable {
            format!("(Just (Maybe.map (Field.Value << {variant}) row.{} |> Maybe.withDefault Field.Null))", arg.name)
        } else {
            format!("(Just (Field.Value ({variant} row.{})))", arg.name)
        };
        let nullable = elm_bool(arg.nullable);
        let mandatory = elm_bool(create && field.required());
        let type_name = arg.type_.as_deref().unwrap();
        let type_ = ColumnType::from_str(type_name);
        let shape = shape(context, &type_, &mut BTreeSet::new());
        let equality = if !create && contains_raw_json(context, &type_, &mut BTreeSet::new()) {
            let encode = to_param_encoder_str(lookup, type_name, false);
            format!("(Internal.jsonEquality ({read} >> Maybe.map {encode}))")
        } else {
            "(==)".into()
        };
        entries.push(format!("Internal.entry {{ key = {key}, required = {mandatory}, nullable = {nullable}, shape = {shape} }} {original} ({read} >> Maybe.map (always True) >> Maybe.withDefault False) {equality}"));
        let value = if create && field.required() {
            let value = format!("required{index}");
            required.push((value.clone(), format!("Internal.value {read} (get {key})")));
            value
        } else if arg.nullable {
            format!("Internal.nullable {read} (get {key})")
        } else {
            format!("Internal.value {read} (get {key})")
        };
        output.push(format!("{} = {value}", arg.name));
    }
    let output = format!("{{ {} }}", output.join("\n                , "));
    let builder = if fields.is_empty() { "_" } else { "get" };
    writeln!(body, "    Internal.init\n        (\\{builder} ->").unwrap();
    if required.is_empty() {
        writeln!(body, "            Just {output}").unwrap();
    } else {
        let args = required
            .iter()
            .map(|(value, _)| value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(body, "            Just (\\{args} -> {output})").unwrap();
        for (_, expression) in required {
            writeln!(body, "                |> Internal.andMap ({expression})").unwrap();
        }
    }
    write!(
        body,
        "        )\n        [ {}\n        ]\n",
        entries.join("\n        , ")
    )
    .unwrap();
}

fn elm_bool(value: bool) -> &'static str {
    if value {
        "True"
    } else {
        "False"
    }
}

fn contains_raw_json(
    context: &typecheck::Context,
    type_: &ColumnType,
    seen: &mut BTreeSet<String>,
) -> bool {
    match type_ {
        ColumnType::Json => true,
        ColumnType::JsonTyped(inner)
        | ColumnType::List(inner)
        | ColumnType::Dict(inner)
        | ColumnType::Nullable(inner) => contains_raw_json(context, inner, seen),
        ColumnType::Custom(name) if seen.insert(name.clone()) => {
            if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(name) {
                variants.iter().any(|variant| {
                    variant.fields.iter().flatten().any(|field| match field {
                        ast::Field::Column(column) => {
                            contains_raw_json(context, &column.type_, seen)
                        }
                        _ => false,
                    })
                })
            } else {
                false
            }
        }
        _ => false,
    }
}

fn shape(context: &typecheck::Context, type_: &ColumnType, seen: &mut BTreeSet<String>) -> String {
    match type_ {
        ColumnType::Custom(name) => {
            if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(name) {
                if seen.insert(name.clone()) {
                    let choices = variants
                        .iter()
                        .map(|variant| {
                            let fields = variant
                                .fields
                                .iter()
                                .flatten()
                                .filter_map(|field| match field {
                                    ast::Field::Column(column) => {
                                        let value = shape(context, &column.type_, seen);
                                        let value = if column.nullable {
                                            format!("Field.Optional ({value})")
                                        } else {
                                            value
                                        };
                                        Some(format!(
                                            "( {}, {value} )",
                                            string::quote(&column.name)
                                        ))
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("( {}, [ {fields} ] )", string::quote(&variant.name))
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    seen.remove(name);
                    return format!("Field.Choices {} [ {choices} ]", string::quote(name));
                }
            }
            format!("Field.Named {}", string::quote(name))
        }
        ColumnType::JsonTyped(inner) => shape(context, inner, seen),
        ColumnType::List(inner) => format!("Field.Sequence ({})", shape(context, inner, seen)),
        ColumnType::Dict(inner) => format!("Field.Dictionary ({})", shape(context, inner, seen)),
        ColumnType::Nullable(inner) => format!("Field.Optional ({})", shape(context, inner, seen)),
        _ => format!("Field.Scalar {}", string::quote(&type_.to_string())),
    }
}
