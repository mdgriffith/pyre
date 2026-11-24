use super::{
    edit_key, elm_database_namespace, elm_union_type, elm_union_variant, to_elm_typename, ElmLookup,
};
use crate::ast::{self, ColumnType, QueryOperation};
use crate::ext::string;
use crate::filesystem::{generate_text_file, GeneratedFile};
use crate::typecheck;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;
use std::path::Path;

struct Field<'a> {
    arg: &'a ast::QueryParamDefinition,
    id: String,
    payload: String,
    required: bool,
}

struct Record<'a> {
    name: &'a str,
    create: Option<Vec<Field<'a>>>,
    update: Option<Vec<Field<'a>>>,
}

impl<'a> Record<'a> {
    fn fields(&self) -> impl Iterator<Item = &Field<'a>> {
        self.create.iter().chain(&self.update).flatten()
    }
}

pub(super) fn generate(
    context: &typecheck::Context,
    base: &Path,
    files: &mut Vec<GeneratedFile<String>>,
    records: &[(String, Vec<&ast::Query>)],
) {
    let lookup = ElmLookup::from_context(context);
    let mut by_namespace = BTreeMap::<_, Vec<Record>>::new();
    for schema in &context.database.schemas {
        by_namespace.entry(schema.namespace.as_str()).or_default();
    }
    for (name, queries) in records {
        let table = crate::generated_queries::generated_crud_table(context, queries[0]).unwrap();
        let key = edit_key(table);
        let mut names = BTreeMap::new();
        let mut used = HashSet::new();
        let mut collect = |operation| {
            let query = queries.iter().find(|query| query.operation == operation)?;
            Some(
                query
                    .args
                    .iter()
                    .filter(|arg| operation != QueryOperation::Update || arg.name != key.name)
                    .map(|arg| {
                        let id = names
                            .entry(arg.name.as_str())
                            .or_insert_with(|| unique(&mut used, string::capitalize(&arg.name)))
                            .clone();
                        let automatic_id =
                            arg.name == key.name && matches!(key.type_, ColumnType::IdUuid { .. });
                        Field {
                            arg,
                            id,
                            payload: to_elm_typename(&lookup, arg.type_.as_deref().unwrap(), false),
                            required: !automatic_id && !arg.omittable,
                        }
                    })
                    .collect(),
            )
        };
        let create = collect(QueryOperation::Insert);
        let update = collect(QueryOperation::Update);
        if create.is_some() || update.is_some() {
            by_namespace.entry(&table.schema).or_default().push(Record {
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
        let namespace = elm_database_namespace(&lookup.namespaces, namespace);
        // The rendered Elm type is the identity: Date and String, for example,
        // have the same payload, while two branded UUIDs remain distinct.
        let mut payloads: Vec<_> = records
            .iter()
            .flat_map(Record::fields)
            .map(|field| field.payload.as_str())
            .collect();
        // Reserve standard payload names before naming custom unions.
        payloads.sort_unstable_by_key(|&payload| {
            (
                payload.starts_with("Db.")
                    && !payload.starts_with("Db.Id.")
                    && payload != "Db.Json",
                payload,
            )
        });
        payloads.dedup();
        let mut used = HashSet::new();
        let constructors: BTreeMap<_, _> = payloads
            .into_iter()
            .map(|payload| (payload, unique(&mut used, value_name(payload))))
            .collect();
        files.push(generate_text_file(
            base.join(format!("Db/Editor/Input/{namespace}.elm")),
            render_input(namespace, &constructors),
        ));
        for record in &records {
            files.push(generate_text_file(
                base.join(format!("Db/Edit/{}/Editor.elm", record.name)),
                render_record(context, &lookup, namespace, record, &constructors),
            ));
        }
    }
}

fn unique(used: &mut HashSet<String>, mut name: String) -> String {
    while !used.insert(name.clone()) {
        name.push('_');
    }
    name
}

fn value_name(payload: &str) -> String {
    let payload = payload
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(payload);
    for (prefix, suffix) in [
        ("List ", "List"),
        ("Dict String ", "Dict"),
        ("Maybe ", "Maybe"),
        ("Db.Id.", "Id"),
    ] {
        if let Some(inner) = payload.strip_prefix(prefix) {
            return format!("{}{suffix}", value_name(inner));
        }
    }
    match payload {
        "Time.Posix" => "DateTime".into(),
        _ => payload.strip_prefix("Db.").unwrap_or(payload).into(),
    }
}

fn render_input(namespace: &str, constructors: &BTreeMap<&str, String>) -> String {
    if constructors.is_empty() {
        return format!("module Db.Editor.Input.{namespace} exposing (Kind, Value, kind)\n\n\ntype alias Kind =\n    Never\n\n\ntype alias Value =\n    Never\n\n\nkind : Value -> Kind\nkind =\n    never\n");
    }
    let kinds = constructors
        .values()
        .map(|name| format!("{name}Kind"))
        .collect::<Vec<_>>();
    let values = constructors
        .iter()
        .map(|(payload, name)| format!("{name}Value ({payload})"))
        .collect::<Vec<_>>();
    let cases = constructors
        .values()
        .map(|name| format!("        {name}Value _ ->\n            {name}Kind"))
        .collect::<Vec<_>>();
    format!("module Db.Editor.Input.{namespace} exposing (Kind(..), Value(..), kind)\n\nimport Db\nimport Db.Id\nimport Dict exposing (Dict)\nimport Time\n\n\ntype Kind\n    = {}\n\n\ntype Value\n    = {}\n\n\nkind : Value -> Kind\nkind input =\n    case input of\n{}\n", kinds.join("\n    | "), values.join("\n    | "), cases.join("\n\n"))
}

fn render_record(
    context: &typecheck::Context,
    lookup: &ElmLookup,
    namespace: &str,
    record: &Record,
    constructors: &BTreeMap<&str, String>,
) -> String {
    let name = record.name;
    let fields: BTreeSet<_> = record.fields().map(|field| field.id.as_str()).collect();
    let (expose_id, field_id) = if fields.is_empty() {
        ("FieldId", "type FieldId\n    = NoField Never".to_string())
    } else {
        (
            "FieldId(..)",
            format!(
                "type FieldId\n    = {}",
                fields.into_iter().collect::<Vec<_>>().join("\n    | ")
            ),
        )
    };
    let mut exposed = vec![expose_id];
    let mut body = format!("import Db\nimport Db.Edit.{name}.Draft as Records\nimport Db.Editor as Editor\nimport Db.Editor.Input.{namespace} as Input\nimport Db.Id\nimport Db.Internal.Editor as Internal\nimport Dict exposing (Dict)\nimport Time\n\n\n{field_id}\n");
    let mut equality = Equality::default();
    if let Some(fields) = &record.create {
        exposed.push("createForm");
        render_form(
            context,
            lookup,
            &mut body,
            fields,
            constructors,
            true,
            &mut equality,
        );
    }
    if let Some(fields) = &record.update {
        exposed.push("editForm");
        render_form(
            context,
            lookup,
            &mut body,
            fields,
            constructors,
            false,
            &mut equality,
        );
    }
    let payloads: BTreeSet<_> = record
        .fields()
        .map(|field| field.payload.as_str())
        .collect();
    for payload in payloads {
        let name = &constructors[payload];
        write!(body, "\n\nread{name} : Input.Value -> Maybe ({payload})\nread{name} input =\n    case input of\n        Input.{name}Value value ->\n            Just value\n").unwrap();
        if constructors.len() > 1 {
            body.push_str("\n        _ ->\n            Nothing\n");
        }
    }
    for definition in equality.unions.into_values().flatten() {
        body.push_str(&definition);
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
    constructors: &BTreeMap<&str, String>,
    create: bool,
    equality: &mut Equality,
) {
    let result = if create { "Draft" } else { "Patch" };
    let signature = format!("Editor.Form FieldId Input.Kind Input.Value Records.{result}");
    if create {
        write!(body, "\n\ncreateForm : {signature}\ncreateForm =\n").unwrap();
    } else {
        let baseline = fields
            .iter()
            .map(|field| {
                let payload = &field.payload;
                let type_ = if field.arg.nullable {
                    format!("Maybe ({payload})")
                } else {
                    payload.clone()
                };
                format!("{} : {type_}", field.arg.name)
            })
            .collect::<Vec<_>>();
        let (baseline, row) = if baseline.is_empty() {
            ("row".into(), "_")
        } else {
            (format!("{{ row | {} }}", baseline.join(", ")), "row")
        };
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
        let id = &field.id;
        let name = &constructors[field.payload.as_str()];
        let original = if create {
            "Db.Absent".into()
        } else if arg.nullable {
            format!(
                "(Maybe.map (Db.Present << Input.{name}Value) row.{} |> Maybe.withDefault Db.Null)",
                arg.name
            )
        } else {
            format!("(Db.Present (Input.{name}Value row.{}))", arg.name)
        };
        let type_name = arg.type_.as_deref().unwrap();
        let type_ = ColumnType::from_str(type_name);
        let choices = union_choices(context, lookup, &type_).join(", ");
        let equal = if !create && contains_raw_json(context, lookup, &type_, &mut HashSet::new()) {
            let compare = equality.compare(context, lookup, &type_);
            format!("(\\left right -> Maybe.map2 {compare} (read{name} left) (read{name} right) |> Maybe.withDefault False)")
        } else {
            "(==)".into()
        };
        let nullable = elm_bool(arg.nullable);
        let mandatory = elm_bool(field.required);
        entries.push(format!("Internal.entry {{ id = {id}, kind = Input.{name}Kind, required = {mandatory}, nullable = {nullable}, choices = [ {choices} ] }} {original} {equal}"));
        let read = format!("Internal.value read{name} (get {id})");
        let value = if field.required {
            let binding = format!("required{index}");
            required.push((binding.clone(), read));
            binding
        } else if arg.nullable {
            format!("Internal.nullable read{name} (get {id})")
        } else {
            read
        };
        output.push(format!("{} = {value}", arg.name));
    }
    let output = output.join("\n                , ");
    let builder = if fields.is_empty() { "_" } else { "get" };
    writeln!(
        body,
        "    Internal.init Input.kind\n        (\\{builder} ->"
    )
    .unwrap();
    if required.is_empty() {
        writeln!(body, "            Just {{ {output} }}").unwrap();
    } else {
        let args = required
            .iter()
            .map(|(value, _)| value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(body, "            Just (\\{args} -> {{ {output} }})").unwrap();
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

// Only opaque JSON needs special equality. Compare its containing Elm values
// structurally rather than going through wire encoders (which can be lossy).
#[derive(Default)]
struct Equality {
    unions: BTreeMap<String, Option<String>>,
}

impl Equality {
    fn compare(
        &mut self,
        context: &typecheck::Context,
        lookup: &ElmLookup,
        ty: &ColumnType,
    ) -> String {
        if !contains_raw_json(context, lookup, ty, &mut HashSet::new()) {
            return "(==)".into();
        }
        match ty {
            ColumnType::Json => "Internal.jsonEqual".into(),
            ColumnType::JsonTyped(inner) => self.compare(context, lookup, inner),
            ColumnType::List(inner) => format!(
                "(Internal.listEqual {})",
                self.compare(context, lookup, inner)
            ),
            ColumnType::Dict(inner) => format!(
                "(Internal.dictEqual {})",
                self.compare(context, lookup, inner)
            ),
            ColumnType::Nullable(inner) => format!(
                "(Internal.maybeEqual {})",
                self.compare(context, lookup, inner)
            ),
            ColumnType::ForeignKey { .. } => self.compare(
                context,
                lookup,
                &super::resolve_foreign_key_type(lookup, ty).unwrap(),
            ),
            ColumnType::Custom(name) => {
                let elm_name = elm_union_type(name);
                let equal = format!("equal{elm_name}");
                if self.unions.contains_key(name) {
                    return equal;
                }
                self.unions.insert(name.clone(), None);
                let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(name) else {
                    unreachable!("only unions can contain opaque JSON");
                };
                let mut body = format!("\n\n{equal} : Db.{elm_name} -> Db.{elm_name} -> Bool\n{equal} left right =\n    case ( left, right ) of\n");
                for variant in variants {
                    let tag = elm_union_variant(&variant.name);
                    if let Some(fields) = &variant.fields {
                        let comparisons = fields
                            .iter()
                            .filter_map(|field| match field {
                                ast::Field::Column(column) => {
                                    let mut equal = self.compare(context, lookup, &column.type_);
                                    if column.nullable {
                                        equal = format!("(Internal.maybeEqual {equal})");
                                    }
                                    let key = &column.name;
                                    Some(format!("{equal} a.{key} b.{key}"))
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        let (a, b, test) = if comparisons.is_empty() {
                            ("_", "_", "True".into())
                        } else {
                            ("a", "b", comparisons.join("\n                && "))
                        };
                        writeln!(
                            body,
                            "        ( Db.{tag} {a}, Db.{tag} {b} ) ->\n            {test}\n"
                        )
                        .unwrap();
                    } else {
                        writeln!(
                            body,
                            "        ( Db.{tag}, Db.{tag} ) ->\n            True\n"
                        )
                        .unwrap();
                    }
                }
                if variants.len() > 1 {
                    body.push_str("        _ ->\n            False\n");
                }
                self.unions.insert(name.clone(), Some(body));
                equal
            }
            _ => unreachable!("only structured values can contain opaque JSON"),
        }
    }
}

fn elm_bool(value: bool) -> &'static str {
    if value {
        "True"
    } else {
        "False"
    }
}

fn union_choices(
    context: &typecheck::Context,
    lookup: &ElmLookup,
    type_: &ColumnType,
) -> Vec<String> {
    match type_ {
        ColumnType::Custom(name) => match context.types.get(name) {
            Some((_, typecheck::Type::OneOf { variants })) => variants
                .iter()
                .map(|variant| string::quote(&variant.name))
                .collect(),
            _ => Vec::new(),
        },
        ColumnType::JsonTyped(inner) => union_choices(context, lookup, inner),
        ColumnType::ForeignKey { .. } => super::resolve_foreign_key_type(lookup, type_)
            .map(|inner| union_choices(context, lookup, &inner))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn contains_raw_json(
    context: &typecheck::Context,
    lookup: &ElmLookup,
    type_: &ColumnType,
    seen: &mut HashSet<String>,
) -> bool {
    match type_ {
        ColumnType::Json => true,
        ColumnType::JsonTyped(inner)
        | ColumnType::List(inner)
        | ColumnType::Dict(inner)
        | ColumnType::Nullable(inner) => contains_raw_json(context, lookup, inner, seen),
        ColumnType::ForeignKey { .. } => super::resolve_foreign_key_type(lookup, type_)
            .is_some_and(|inner| contains_raw_json(context, lookup, &inner, seen)),
        ColumnType::Custom(name) if seen.insert(name.clone()) => match context.types.get(name) {
            Some((_, typecheck::Type::OneOf { variants })) => variants.iter().any(|variant| {
                variant.fields.iter().flatten().any(|field| match field {
                    ast::Field::Column(column) => {
                        contains_raw_json(context, lookup, &column.type_, seen)
                    }
                    _ => false,
                })
            }),
            _ => false,
        },
        _ => false,
    }
}
