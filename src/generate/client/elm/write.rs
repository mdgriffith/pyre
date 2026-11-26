use super::{
    elm_type_apply, elm_union_type, elm_union_variant, to_elm_decoder_from_column_type,
    to_elm_encoder, to_elm_type_from_column_type, ElmLookup,
};
use crate::{ast::*, ext::string, schema_lifecycle, typecheck};
use std::fmt::Write;

pub(super) fn original_name(name: &str) -> Option<&str> {
    name.strip_prefix(schema_lifecycle::WRITE_TYPE_PREFIX)
}

pub(super) fn module(context: &typecheck::Context) -> Option<String> {
    let lookup = ElmLookup::from_context(context);
    let mut unions = context
        .types
        .iter()
        .filter_map(|(name, (_, type_))| {
            let typecheck::Type::OneOf { variants } = type_ else {
                return None;
            };
            schema_lifecycle::has_retired_variant(context, &ColumnType::Custom(name.clone()))
                .then_some((name, variants))
        })
        .collect::<Vec<_>>();
    if unions.is_empty() {
        return None;
    }
    unions.sort_unstable_by_key(|(name, _)| *name);
    let mut body = String::from("module Db.Write exposing (..)\n\n{-| Writable versions of unions affected by retirement. Historical rows use `Db` types. -}\n\nimport Db\nimport Db.Decode\nimport Db.Encode\nimport Db.Id\nimport Dict exposing (Dict)\nimport Json.Decode as Decode\nimport Json.Encode as Encode\nimport Time\n");
    for (name, variants) in unions {
        union(context, &lookup, &mut body, name, variants);
    }
    // Elm has no empty custom type. Never also lets lists/dicts of a retired-only
    // union be empty and nullable positions be Nothing, without admitting a tag.
    body.push_str("\n\ntraverseMaybe : (a -> Maybe b) -> Maybe a -> Maybe (Maybe b)\ntraverseMaybe convert value =\n    case value of\n        Nothing ->\n            Just Nothing\n\n        Just inner ->\n            Maybe.map Just (convert inner)\n\n\ntraverseList : (a -> Maybe b) -> List a -> Maybe (List b)\ntraverseList convert =\n    List.foldr (\\value acc -> Maybe.map2 (::) (convert value) acc) (Just [])\n\n\ntraverseDict : (a -> Maybe b) -> Dict String a -> Maybe (Dict String b)\ntraverseDict convert =\n    Dict.foldl (\\key value acc -> Maybe.map2 (Dict.insert key) (convert value) acc) (Just Dict.empty)\n\n\nandMap : Maybe a -> Maybe (a -> b) -> Maybe b\nandMap =\n    Maybe.map2 (|>)\n");
    Some(body)
}

fn payload_type(column: &Column) -> ColumnType {
    if column.nullable {
        ColumnType::Nullable(Box::new(column.type_.clone()))
    } else {
        column.type_.clone()
    }
}

fn union(
    context: &typecheck::Context,
    lookup: &ElmLookup,
    body: &mut String,
    name: &str,
    variants: &[Variant],
) {
    let type_name = elm_union_type(name);
    let writable: Vec<_> = variants
        .iter()
        .filter(|v| v.lifecycle != Lifecycle::Retired)
        .collect();
    let mut encoders = String::new();
    let mut decoders = String::new();
    let mut conversions = String::new();
    if writable.is_empty() {
        write!(body, "\n\ntype alias {type_name} =\n    Basics.Never\n\n\nencode{name} : {type_name} -> Encode.Value\nencode{name} =\n    Basics.never\n\n\ndecode{name} : Decode.Decoder {type_name}\ndecode{name} =\n    Decode.fail \"{name} has no writable variants\"\n\n\nfrom{name} : Db.{type_name} -> Maybe {type_name}\nfrom{name} _ =\n    Nothing\n").unwrap();
        return;
    }
    write!(body, "\n\ntype {type_name}\n").unwrap();
    for (index, variant) in writable.iter().enumerate() {
        let tag = elm_union_variant(&variant.name);
        let wire_tag = string::quote(&variant.name);
        let lead = if index == 0 { '=' } else { '|' };
        let Some(fields) = &variant.fields else {
            writeln!(body, "    {lead} {tag}").unwrap();
            writeln!(encoders, "        {tag} ->\n            Encode.object [ ( \"_type\", Encode.string {wire_tag} ) ]\n").unwrap();
            writeln!(
                decoders,
                "                    {wire_tag} ->\n                        Decode.succeed {tag}\n"
            )
            .unwrap();
            writeln!(conversions, "        Db.{tag} ->\n            Just {tag}\n").unwrap();
            continue;
        };
        let columns: Vec<_> = collect_columns(fields)
            .into_iter()
            .map(|column| {
                let type_ = payload_type(&column);
                let write_type = schema_lifecycle::write_type(context, &type_);
                (column.name, type_, write_type)
            })
            .collect();
        let types = columns
            .iter()
            .map(|(field, _, ty)| {
                format!(
                    "{field} : {}",
                    to_elm_type_from_column_type(lookup, ty).replace("Db.Write.", "")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(body, "    {lead} {tag} {{ {types} }}").unwrap();

        let payload = if columns.is_empty() { "_" } else { "payload" };
        writeln!(encoders, "        {tag} {payload} ->\n            Encode.object\n                [ ( \"_type\", Encode.string {wire_tag} )").unwrap();
        for (field, _, ty) in &columns {
            let encoder = to_elm_encoder(lookup, ty).replace("Db.Write.", "");
            writeln!(
                encoders,
                "                , ( \"{field}\", {encoder} payload.{field} )"
            )
            .unwrap();
        }
        encoders.push_str("                ]\n\n");

        let make = if columns.is_empty() {
            format!("({tag} {{}})")
        } else {
            let args = (0..columns.len())
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(" ");
            let record = columns
                .iter()
                .enumerate()
                .map(|(i, (field, _, _))| format!("{field} = v{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("(\\{args} -> {tag} {{ {record} }})")
        };
        writeln!(
            decoders,
            "                    {wire_tag} ->\n                        Decode.succeed {make}"
        )
        .unwrap();
        writeln!(
            conversions,
            "        Db.{tag} {payload} ->\n            Just {make}"
        )
        .unwrap();
        for (field, original, ty) in &columns {
            let decoder = to_elm_decoder_from_column_type(lookup, ty).replace("Db.Write.", "");
            writeln!(
                decoders,
                "                            |> Db.Decode.andField \"{field}\" {decoder}"
            )
            .unwrap();
            let convert = conversion(context, original).replace("Db.Write.", "");
            writeln!(
                conversions,
                "                |> andMap ({convert} payload.{field})"
            )
            .unwrap();
        }
        decoders.push('\n');
        conversions.push('\n');
    }
    if writable.len() != variants.len() {
        conversions.push_str("        _ ->\n            Nothing\n");
    }
    write!(body, "\n\nencode{name} : {type_name} -> Encode.Value\nencode{name} input =\n    case input of\n{encoders}\n\ndecode{name} : Decode.Decoder {type_name}\ndecode{name} =\n    Decode.field \"_type\" Decode.string\n        |> Decode.andThen\n            (\\tag ->\n                case tag of\n{decoders}                    _ ->\n                        Decode.fail (\"Not a writable variant of {name}: \" ++ tag)\n            )\n\n\nfrom{name} : Db.{type_name} -> Maybe {type_name}\nfrom{name} input =\n    case input of\n{conversions}").unwrap();
}

pub(super) fn conversion(context: &typecheck::Context, type_: &ColumnType) -> String {
    if !schema_lifecycle::has_retired_variant(context, type_) {
        return "Just".into();
    }
    match type_ {
        ColumnType::Custom(name) => format!("Db.Write.from{name}"),
        ColumnType::JsonTyped(inner) => conversion(context, inner),
        ColumnType::Nullable(inner) => {
            elm_type_apply("Db.Write.traverseMaybe", conversion(context, inner))
        }
        ColumnType::List(inner) => {
            elm_type_apply("Db.Write.traverseList", conversion(context, inner))
        }
        ColumnType::Dict(inner) => {
            elm_type_apply("Db.Write.traverseDict", conversion(context, inner))
        }
        _ => "Just".into(),
    }
}
