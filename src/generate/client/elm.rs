use crate::ast;
use crate::ext::string;
use crate::filesystem::{generate_text_file, GeneratedFile};

use crate::generate::typealias;
use crate::typecheck;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const ELM_DELTA_MODULE: &str = include_str!("./static/elm/src/Db/Delta.elm");
const ELM_UPDATES_MODULE: &str = include_str!("./static/elm/src/Db/Updates.elm");

pub fn generate(
    base_path: &Path,
    database: &ast::Database,
    files: &mut Vec<GeneratedFile<String>>,
) {
    files.push(generate_text_file(
        base_path.join("Db.elm"),
        write_schema(database),
    ));
    files.push(generate_text_file(
        base_path.join("Db/Id.elm"),
        to_schema_ids(database),
    ));
    files.push(generate_text_file(
        base_path.join("Db/Database.elm"),
        to_database_ids(database),
    ));
    files.push(generate_text_file(
        base_path.join("Db/Decode.elm"),
        to_schema_decoders(database),
    ));
    files.push(generate_text_file(
        base_path.join("Db/Encode.elm"),
        to_schema_encoders(database),
    ));
    files.push(generate_text_file(
        base_path.join("Db/Delta.elm"),
        ELM_DELTA_MODULE,
    ));
    files.push(generate_text_file(
        base_path.join("Db/Updates.elm"),
        ELM_UPDATES_MODULE,
    ));
    for schema in &database.schemas {
        let records = collect_schema_entity_stream_records(schema);

        for record in &records {
            files.push(generate_text_file(
                base_path.join(entity_stream_table_module_path(schema, record)),
                to_entity_stream_table_module(database, schema, record),
            ));
        }

        files.push(generate_text_file(
            base_path.join(entity_stream_internal_module_path(schema)),
            to_entity_stream_internal_module(schema),
        ));

        files.push(generate_text_file(
            base_path.join(entity_stream_module_path(schema)),
            to_entity_stream_module(schema, &records),
        ));
    }
}

fn to_entity_stream_module(schema: &ast::Schema, records: &[EntityStreamRecord]) -> String {
    let mut result = String::new();

    result.push_str(&format!(
        "module {} exposing (DatabaseId, {}, EntityChange(..), EntityChangeBatch, EntityChangeBatchSource(..), EntitySubscription, StreamId, decodeIncomingBatch, register, unregister",
        entity_stream_module_name(schema),
        elm_database_namespace(&schema.namespace)
    ));
    for record in records {
        result.push_str(&format!(", {}", entity_stream_wrap_function_name(record)));
    }
    result.push_str(")\n\n\n");
    result.push_str("{-| Register and decode entity stream batches.\n\n    pyreStoreOut\n        (register databaseId\n            \"visible-posts\"\n            [ Posts.stream\n                |> Posts.idIn visiblePostIds\n                |> post\n            ]\n        )\n\n    decodeIncomingBatch value\n\n@docs DatabaseId, StreamId, EntitySubscription, EntityChange, EntityChangeBatch, EntityChangeBatchSource, register, unregister, decodeIncomingBatch\n\n-}\n\n");
    result.push_str("import Db.Database\n");
    result.push_str(&format!(
        "import {} as StreamInternal\n",
        entity_stream_internal_module_name(schema)
    ));
    for record in records {
        result.push_str(&format!(
            "import {} as {}\n",
            entity_stream_table_module_name(schema, record),
            record.table_module_segment
        ));
    }
    result.push_str("import Json.Decode as Decode\n");
    result.push_str("import Json.Encode as Encode\n");
    result.push_str("\n\n");

    result
        .push_str("type alias DatabaseId namespace =\n    Db.Database.DatabaseId namespace\n\n\n");
    result.push_str(&format!(
        "type alias {} =\n    Db.Database.{}\n\n\n",
        elm_database_namespace(&schema.namespace),
        elm_database_namespace(&schema.namespace)
    ));
    result.push_str("type alias StreamId =\n    String\n\n\n");

    result.push_str(
        "type EntitySubscription\n    = EntitySubscription StreamInternal.EntitySubscription\n\n\n",
    );

    for record in records {
        result.push_str(&format!(
            "{} : StreamInternal.TableSubscription {}.Stream -> EntitySubscription\n",
            entity_stream_wrap_function_name(record),
            record.table_module_segment
        ));
        result.push_str(&format!(
            "{} tableStream =\n    EntitySubscription (StreamInternal.toEntitySubscription tableStream)\n\n\n",
            entity_stream_wrap_function_name(record)
        ));
    }

    result.push_str("type EntityChange\n");
    if records.is_empty() {
        result.push_str("    = EntityDecodeFailed String Decode.Value\n\n\n");
    } else {
        for (index, record) in records.iter().enumerate() {
            let prefix = if index == 0 { "    = " } else { "    | " };
            result.push_str(&format!(
                "{}{}Row {}.Row\n",
                prefix, record.record_name, record.table_module_segment
            ));
        }
        result.push_str("    | EntityDecodeFailed String Decode.Value\n\n\n");
    }

    result.push_str("type EntityChangeBatchSource\n    = IndexedDbInitial\n    | Catchup\n    | Live\n    | UnknownSource String\n\n\n");
    result.push_str("type alias EntityChangeBatch =\n    { streamId : StreamId\n    , databaseId : Maybe String\n    , sequence : Int\n    , source : EntityChangeBatchSource\n    , changes : List EntityChange\n    }\n\n\n");

    result.push_str(&format!(
        "register : DatabaseId {} -> StreamId -> List EntitySubscription -> Encode.Value\n",
        elm_database_namespace(&schema.namespace)
    ));
    result.push_str("register databaseId streamId subscriptions =\n");
    result.push_str("    Encode.object\n");
    result.push_str("        [ ( \"type\", Encode.string \"register-entity-stream\" )\n");
    result.push_str("        , ( \"databaseId\", Db.Database.encode databaseId )\n");
    result.push_str("        , ( \"streamId\", Encode.string streamId )\n");
    result.push_str("        , ( \"tables\", Encode.list encodeSubscription subscriptions )\n");
    result.push_str("        ]\n\n\n");

    result.push_str("unregister : StreamId -> Encode.Value\n");
    result.push_str("unregister streamId =\n");
    result.push_str("    Encode.object\n");
    result.push_str("        [ ( \"type\", Encode.string \"unregister-entity-stream\" )\n");
    result.push_str("        , ( \"streamId\", Encode.string streamId )\n");
    result.push_str("        ]\n\n\n");

    result
        .push_str("decodeIncomingBatch : Decode.Value -> Result Decode.Error EntityChangeBatch\n");
    result.push_str("decodeIncomingBatch json =\n");
    result.push_str("    Decode.decodeValue entityChangeBatchDecoder json\n\n\n");

    result.push_str("encodeSubscription : EntitySubscription -> Encode.Value\n");
    result.push_str("encodeSubscription subscription =\n");
    result.push_str("    case subscription of\n");
    result.push_str("        EntitySubscription inner ->\n");
    result.push_str("            StreamInternal.encodeSubscription inner\n\n\n");

    result.push_str("entityChangeBatchDecoder : Decode.Decoder EntityChangeBatch\n");
    result.push_str("entityChangeBatchDecoder =\n");
    result.push_str("    Decode.map5 EntityChangeBatch\n");
    result.push_str("        (Decode.field \"streamId\" Decode.string)\n");
    result.push_str("        (Decode.maybe (Decode.field \"databaseId\" Decode.string))\n");
    result.push_str("        (Decode.field \"sequence\" Decode.int)\n");
    result.push_str("        (Decode.field \"source\" sourceDecoder)\n");
    result.push_str("        (Decode.field \"changes\" (Decode.list entityChangeDecoder))\n\n\n");

    result.push_str("sourceDecoder : Decode.Decoder EntityChangeBatchSource\n");
    result.push_str("sourceDecoder =\n");
    result.push_str("    Decode.string\n");
    result.push_str("        |> Decode.map\n");
    result.push_str("            (\\source ->\n");
    result.push_str("                case source of\n");
    result.push_str("                    \"indexeddb-initial\" ->\n");
    result.push_str("                        IndexedDbInitial\n\n");
    result.push_str("                    \"catchup\" ->\n");
    result.push_str("                        Catchup\n\n");
    result.push_str("                    \"live\" ->\n");
    result.push_str("                        Live\n\n");
    result.push_str("                    other ->\n");
    result.push_str("                        UnknownSource other\n");
    result.push_str("            )\n\n\n");

    result.push_str("entityChangeDecoder : Decode.Decoder EntityChange\n");
    result.push_str("entityChangeDecoder =\n");
    result.push_str("    Decode.map2 Tuple.pair\n");
    result.push_str("        (Decode.field \"tableName\" Decode.string)\n");
    result.push_str("        (Decode.field \"row\" Decode.value)\n");
    result.push_str("        |> Decode.map\n");
    result.push_str("            (\\( tableName, row ) ->\n");
    result.push_str("                case tableName of\n");
    for record in records {
        result.push_str(&format!(
            "                    \"{}\" ->\n",
            record.table_name
        ));
        result.push_str(&format!(
            "                        decodeRow {}Row {}.decodeRow row\n\n",
            record.record_name, record.table_module_segment
        ));
    }
    result.push_str("                    other ->\n");
    result.push_str("                        EntityDecodeFailed other row\n");
    result.push_str("            )\n");

    result.push_str("\n\n");
    result.push_str(
        "decodeRow : (row -> EntityChange) -> Decode.Decoder row -> Decode.Value -> EntityChange\n",
    );
    result.push_str("decodeRow toChange rowDecoder row =\n");
    result.push_str("    case Decode.decodeValue rowDecoder row of\n");
    result.push_str("        Ok decoded ->\n");
    result.push_str("            toChange decoded\n\n");
    result.push_str("        Err decodeErr ->\n");
    result.push_str("            EntityDecodeFailed (Decode.errorToString decodeErr) row\n");

    result
}

fn to_entity_stream_table_module(
    database: &ast::Database,
    schema: &ast::Schema,
    record: &EntityStreamRecord,
) -> String {
    let mut result = String::new();

    result.push_str(&format!(
        "module {} exposing (Row, Stream, decodeRow, stream",
        entity_stream_table_module_name(schema, record)
    ));
    for field in entity_stream_condition_fields(database, record) {
        result.push_str(&format!(
            ", {}",
            entity_stream_condition_function_name(&field.name)
        ));
    }
    result.push_str(")\n\n\n");
    result.push_str("import Db\n");
    result.push_str("import Db.Decode\n");
    result.push_str("import Db.Id\n");
    result.push_str(&format!(
        "import {} as StreamInternal\n",
        entity_stream_internal_module_name(schema)
    ));
    result.push_str("import Dict exposing (Dict)\n");
    result.push_str("import Json.Decode as Decode\n");
    result.push_str("import Json.Encode as Encode\n");
    result.push_str("import Time\n\n\n");
    result.push_str("type Stream\n    = Stream\n\n\n");
    result.push_str("stream : StreamInternal.TableSubscription Stream\n");
    result.push_str(&format!(
        "stream =\n    StreamInternal.table \"{}\"\n\n\n",
        record.table_name
    ));
    result.push_str(&entity_stream_condition_builders(database, record));
    result.push_str(&entity_stream_row_alias(database, record));
    result.push_str(&entity_stream_row_decoder(database, record));

    result
}

fn to_entity_stream_internal_module(schema: &ast::Schema) -> String {
    let mut result = String::new();

    result.push_str(&format!(
        "module {} exposing (EntitySubscription, TableSubscription, addCondition, encodeSubscription, table, toEntitySubscription)\n\n\n",
        entity_stream_internal_module_name(schema)
    ));
    result.push_str("import Dict exposing (Dict)\n");
    result.push_str("import Json.Encode as Encode\n\n\n");
    result.push_str("type TableSubscription table\n    = TableSubscription String (Dict String Encode.Value)\n\n\n");
    result.push_str(
        "type EntitySubscription\n    = EntitySubscription String (Dict String Encode.Value)\n\n\n",
    );
    result.push_str("table : String -> TableSubscription table\n");
    result.push_str("table tableName =\n    TableSubscription tableName Dict.empty\n\n\n");
    result.push_str("addCondition : String -> Encode.Value -> TableSubscription table -> TableSubscription table\n");
    result.push_str("addCondition field condition subscription =\n");
    result.push_str("    case subscription of\n");
    result.push_str("        TableSubscription tableName conditions ->\n");
    result.push_str(
        "            TableSubscription tableName (Dict.insert field condition conditions)\n\n\n",
    );
    result.push_str("toEntitySubscription : TableSubscription table -> EntitySubscription\n");
    result.push_str("toEntitySubscription subscription =\n");
    result.push_str("    case subscription of\n");
    result.push_str("        TableSubscription tableName conditions ->\n");
    result.push_str("            EntitySubscription tableName conditions\n\n\n");
    result.push_str("encodeSubscription : EntitySubscription -> Encode.Value\n");
    result.push_str("encodeSubscription subscription =\n");
    result.push_str("    case subscription of\n");
    result.push_str("        EntitySubscription tableName conditions ->\n");
    result.push_str("            Encode.object\n");
    result.push_str("                (( \"tableName\", Encode.string tableName ) :: encodeWhere conditions)\n\n\n");
    result.push_str("encodeWhere : Dict String Encode.Value -> List ( String, Encode.Value )\n");
    result.push_str("encodeWhere conditions =\n");
    result.push_str("    if Dict.isEmpty conditions then\n");
    result.push_str("        []\n\n");
    result.push_str("    else\n");
    result.push_str("        [ ( \"where\", Encode.object (Dict.toList conditions) ) ]\n");

    result
}

#[derive(Debug)]
struct EntityStreamRecord {
    record_name: String,
    table_name: String,
    table_module_segment: String,
    fields: Vec<ast::Field>,
}

fn entity_stream_module_name(schema: &ast::Schema) -> String {
    format!("{}.Stream", entity_stream_schema_module_prefix(schema))
}

fn entity_stream_module_path(schema: &ast::Schema) -> String {
    format!("{}/Stream.elm", entity_stream_schema_path_prefix(schema))
}

fn entity_stream_internal_module_name(schema: &ast::Schema) -> String {
    format!(
        "{}.Stream.Internal",
        entity_stream_schema_module_prefix(schema)
    )
}

fn entity_stream_internal_module_path(schema: &ast::Schema) -> String {
    format!(
        "{}/Stream/Internal.elm",
        entity_stream_schema_path_prefix(schema)
    )
}

fn entity_stream_table_module_name(schema: &ast::Schema, record: &EntityStreamRecord) -> String {
    format!(
        "{}.Table.{}",
        entity_stream_schema_module_prefix(schema),
        record.table_module_segment
    )
}

fn entity_stream_table_module_path(schema: &ast::Schema, record: &EntityStreamRecord) -> String {
    format!(
        "{}/Table/{}.elm",
        entity_stream_schema_path_prefix(schema),
        record.table_module_segment
    )
}

fn entity_stream_wrap_function_name(record: &EntityStreamRecord) -> String {
    string::decapitalize(&record.record_name)
}

fn entity_stream_condition_function_name(field_name: &str) -> String {
    format!(
        "{}In",
        string::decapitalize(&elm_module_segment(field_name))
    )
}

fn entity_stream_schema_module_prefix(schema: &ast::Schema) -> String {
    if schema.namespace == ast::DEFAULT_SCHEMANAME {
        "Db".to_string()
    } else {
        format!("Db.{}", elm_database_namespace(&schema.namespace))
    }
}

fn entity_stream_schema_path_prefix(schema: &ast::Schema) -> String {
    if schema.namespace == ast::DEFAULT_SCHEMANAME {
        "Db".to_string()
    } else {
        format!("Db/{}", elm_database_namespace(&schema.namespace))
    }
}

fn elm_module_segment(name: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if result.is_empty() && ch.is_ascii_digit() {
                result.push('D');
            }
            if capitalize_next {
                result.push(ch.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(ch);
            }
        } else {
            capitalize_next = true;
        }
    }

    if result.is_empty() {
        "Table".to_string()
    } else {
        result
    }
}

fn collect_schema_entity_stream_records(schema: &ast::Schema) -> Vec<EntityStreamRecord> {
    let mut records = Vec::new();

    for file in &schema.files {
        for definition in &file.definitions {
            if let ast::Definition::Record { name, fields, .. } = definition {
                if entity_stream_id_type(fields).is_some() {
                    let table_name = ast::get_tablename(name, fields);
                    records.push(EntityStreamRecord {
                        record_name: name.clone(),
                        table_module_segment: elm_module_segment(&table_name),
                        table_name,
                        fields: fields.clone(),
                    });
                }
            }
        }
    }

    records.sort_by(|a, b| a.record_name.cmp(&b.record_name));
    records
}

fn entity_stream_row_alias(database: &ast::Database, record: &EntityStreamRecord) -> String {
    let mut result = "type alias Row =\n".to_string();
    let mut is_first = true;

    for field in &record.fields {
        let ast::Field::Column(column) = field else {
            continue;
        };

        if is_first {
            result.push_str("    { ");
        } else {
            result.push_str("    , ");
        }

        let maybe = if column.nullable { "Maybe " } else { "" };
        result.push_str(&format!(
            "{} : {}{}\n",
            column.name,
            maybe,
            entity_stream_elm_type(database, &column.type_)
        ));
        is_first = false;
    }

    if is_first {
        result.push_str("    {}\n");
    } else {
        result.push_str("    }\n");
    }
    result.push_str("\n\n");
    result
}

fn entity_stream_row_decoder(database: &ast::Database, record: &EntityStreamRecord) -> String {
    let mut result =
        "decodeRow : Decode.Decoder Row\ndecodeRow =\n    Decode.succeed Row\n".to_string();

    for field in &record.fields {
        let ast::Field::Column(column) = field else {
            continue;
        };

        let decoder = entity_stream_decoder(database, &column.type_);
        let decoder = if column.nullable {
            format!("(Decode.nullable {})", decoder)
        } else {
            decoder
        };
        result.push_str(&format!(
            "        |> Db.Decode.andField \"{}\" {}\n",
            column.name, decoder
        ));
    }

    result.push_str("\n\n");
    result
}

fn entity_stream_condition_builders(
    database: &ast::Database,
    record: &EntityStreamRecord,
) -> String {
    let mut result = String::new();

    for column in entity_stream_condition_fields(database, record) {
        let Some(encoder) = entity_stream_encoder(database, &column.type_) else {
            continue;
        };

        result.push_str(&format!(
            "{} : List {} -> StreamInternal.TableSubscription Stream -> StreamInternal.TableSubscription Stream\n",
            entity_stream_condition_function_name(&column.name),
            entity_stream_elm_type(database, &column.type_)
        ));
        result.push_str(&format!(
            "{} values subscription =\n",
            entity_stream_condition_function_name(&column.name)
        ));
        result.push_str(&format!(
            "    StreamInternal.addCondition \"{}\" (Encode.object [ ( \"$in\", Encode.list {} values ) ]) subscription\n\n\n",
            column.name, encoder
        ));
    }

    result
}

fn entity_stream_condition_fields(
    database: &ast::Database,
    record: &EntityStreamRecord,
) -> Vec<ast::Column> {
    let mut columns = Vec::new();

    for field in &record.fields {
        let ast::Field::Column(column) = field else {
            continue;
        };

        if entity_stream_encoder(database, &column.type_).is_some()
            && (entity_stream_column_is_id_like(database, column) || ast::is_primary_key(column))
        {
            columns.push(column.clone());
        }
    }

    columns
}

fn entity_stream_column_is_id_like(database: &ast::Database, column: &ast::Column) -> bool {
    match &column.type_ {
        ast::ColumnType::IdInt { .. } | ast::ColumnType::IdUuid { .. } => true,
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if database_column_type(database, table, field).is_some() {
                true
            } else {
                field == "id"
            }
        }
        ast::ColumnType::JsonTyped(inner) | ast::ColumnType::Nullable(inner) => {
            entity_stream_column_is_id_like(
                database,
                &ast::Column {
                    type_: *inner.clone(),
                    ..column.clone()
                },
            )
        }
        _ => false,
    }
}

fn entity_stream_elm_type(database: &ast::Database, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::IdInt { table } => {
            if table.is_empty() {
                "Int".to_string()
            } else {
                format!("Db.Id.{}", table)
            }
        }
        ast::ColumnType::IdUuid { table } => {
            if table.is_empty() {
                "String".to_string()
            } else {
                format!("Db.Id.{}", table)
            }
        }
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if let Some(column_type) = database_column_type(database, table, field) {
                entity_stream_elm_type(database, &column_type)
            } else if field == "id" {
                format!("Db.Id.{}", table)
            } else {
                "String".to_string()
            }
        }
        ast::ColumnType::JsonTyped(inner) => entity_stream_elm_type(database, inner),
        ast::ColumnType::List(inner) => format!("List {}", entity_stream_elm_type(database, inner)),
        ast::ColumnType::Dict(inner) => {
            format!("Dict String {}", entity_stream_elm_type(database, inner))
        }
        ast::ColumnType::Nullable(inner) => {
            format!("Maybe {}", entity_stream_elm_type(database, inner))
        }
        _ => elm_type_from_column_type(type_, true),
    }
}

fn entity_stream_encoder(database: &ast::Database, type_: &ast::ColumnType) -> Option<String> {
    match type_ {
        ast::ColumnType::IdInt { table } => Some(if table.is_empty() {
            "Encode.int".to_string()
        } else {
            "Db.Id.encodeInt".to_string()
        }),
        ast::ColumnType::IdUuid { table } => Some(if table.is_empty() {
            "Encode.string".to_string()
        } else {
            "Db.Id.encodeUuid".to_string()
        }),
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if let Some(column_type) = database_column_type(database, table, field) {
                entity_stream_encoder(database, &column_type)
            } else if field == "id" {
                match id_kind_for_brand(database, table) {
                    Some(IdKind::Int) => Some("Db.Id.encodeInt".to_string()),
                    Some(IdKind::Uuid) => Some("Db.Id.encodeUuid".to_string()),
                    None => Some("Encode.string".to_string()),
                }
            } else {
                Some("Encode.string".to_string())
            }
        }
        ast::ColumnType::JsonTyped(inner) | ast::ColumnType::Nullable(inner) => {
            entity_stream_encoder(database, inner)
        }
        ast::ColumnType::Int => Some("Encode.int".to_string()),
        ast::ColumnType::String => Some("Encode.string".to_string()),
        _ => None,
    }
}

fn entity_stream_decoder(database: &ast::Database, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::IdInt { table } => {
            if table.is_empty() {
                "Decode.int".to_string()
            } else {
                "Db.Id.decodeInt".to_string()
            }
        }
        ast::ColumnType::IdUuid { table } => {
            if table.is_empty() {
                "Decode.string".to_string()
            } else {
                "Db.Id.decodeUuid".to_string()
            }
        }
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if let Some(column_type) = database_column_type(database, table, field) {
                entity_stream_decoder(database, &column_type)
            } else if field == "id" {
                match id_kind_for_brand(database, table) {
                    Some(IdKind::Int) => "Db.Id.decodeInt".to_string(),
                    Some(IdKind::Uuid) => "Db.Id.decodeUuid".to_string(),
                    None => "Decode.string".to_string(),
                }
            } else {
                "Decode.string".to_string()
            }
        }
        ast::ColumnType::JsonTyped(inner) => entity_stream_decoder(database, inner),
        ast::ColumnType::List(inner) => {
            format!("(Decode.list {})", entity_stream_decoder(database, inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!("(Decode.dict {})", entity_stream_decoder(database, inner))
        }
        ast::ColumnType::Nullable(inner) => format!(
            "(Decode.nullable {})",
            entity_stream_decoder(database, inner)
        ),
        _ => to_elm_decoder_from_column_type(&ElmLookup::default(), type_),
    }
}

fn database_column_type(
    database: &ast::Database,
    record_name: &str,
    field_name: &str,
) -> Option<ast::ColumnType> {
    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                if let ast::Definition::Record { name, fields, .. } = definition {
                    if !name.eq_ignore_ascii_case(record_name) {
                        continue;
                    }

                    for field in fields {
                        if let ast::Field::Column(column) = field {
                            if column.name == field_name {
                                return Some(column.type_.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn entity_stream_id_type(fields: &Vec<ast::Field>) -> Option<(String, &'static str)> {
    for field in fields {
        if let ast::Field::Column(column) = field {
            if column.name != "id" {
                continue;
            }

            return match &column.type_ {
                ast::ColumnType::IdInt { table } => Some((
                    if table.is_empty() {
                        "Int".to_string()
                    } else {
                        format!("Db.Id.{}", table)
                    },
                    if table.is_empty() {
                        "Encode.int"
                    } else {
                        "Db.Id.encodeInt"
                    },
                )),
                ast::ColumnType::IdUuid { table } => Some((
                    if table.is_empty() {
                        "String".to_string()
                    } else {
                        format!("Db.Id.{}", table)
                    },
                    if table.is_empty() {
                        "Encode.string"
                    } else {
                        "Db.Id.encodeUuid"
                    },
                )),
                ast::ColumnType::Int => Some(("Int".to_string(), "Encode.int")),
                ast::ColumnType::String => Some(("String".to_string(), "Encode.string")),
                _ => None,
            };
        }
    }

    None
}

fn to_database_ids(database: &ast::Database) -> String {
    let mut namespaces: Vec<String> = database
        .schemas
        .iter()
        .map(|schema| schema.namespace.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    namespaces.sort();

    let mut result = String::new();
    result.push_str("module Db.Database exposing (DatabaseId, fromString, toString, encode");
    for namespace in &namespaces {
        result.push_str(&format!(", {}", elm_database_namespace(namespace)));
    }
    result.push_str(")\n\n");
    result.push_str("import Json.Encode as Encode\n\n\n");

    result.push_str("type DatabaseId namespace\n");
    result.push_str("    = DatabaseId String\n\n\n");

    for namespace in &namespaces {
        let name = elm_database_namespace(namespace);
        result.push_str(&format!("type {}\n", name));
        result.push_str(&format!("    = {}\n\n\n", name));
    }

    result.push_str("fromString : String -> DatabaseId namespace\n");
    result.push_str("fromString =\n");
    result.push_str("    DatabaseId\n\n\n");

    result.push_str("toString : DatabaseId namespace -> String\n");
    result.push_str("toString (DatabaseId value) =\n");
    result.push_str("    value\n\n\n");

    result.push_str("encode : DatabaseId namespace -> Encode.Value\n");
    result.push_str("encode databaseId =\n");
    result.push_str("    Encode.string (toString databaseId)\n");

    result
}

fn elm_database_namespace(namespace: &str) -> String {
    if namespace == ast::DEFAULT_SCHEMANAME {
        return "Default".to_string();
    }

    let mut result = String::new();
    let mut capitalize_next = true;
    for ch in namespace.chars() {
        if ch.is_ascii_alphanumeric() {
            if result.is_empty() && ch.is_ascii_digit() {
                result.push('D');
            }
            if capitalize_next {
                result.push(ch.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(ch);
            }
        } else {
            capitalize_next = true;
        }
    }

    if result.is_empty() {
        "Default".to_string()
    } else {
        result
    }
}

fn elm_database_id_type(namespace: &str) -> String {
    format!("(DatabaseId {})", elm_database_namespace(namespace))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdKind {
    Int,
    Uuid,
}

#[derive(Clone, Debug, Default)]
struct ElmLookup {
    records_by_name: std::collections::HashMap<String, ast::RecordDetails>,
}

impl ElmLookup {
    fn from_context(context: &typecheck::Context) -> Self {
        let mut records_by_name = std::collections::HashMap::new();

        for table in context.tables.values() {
            records_by_name.insert(table.record.name.to_ascii_lowercase(), table.record.clone());
        }

        ElmLookup { records_by_name }
    }
}

fn to_schema_ids(database: &ast::Database) -> String {
    let mut result = String::new();

    result.push_str("module Db.Id exposing (..)");
    result.push_str("\n\n");
    result.push_str("import Json.Decode as Decode\n");
    result.push_str("import Json.Encode as Encode\n\n\n");

    result.push_str("type Integer guard\n");
    result.push_str("    = Integer Int\n\n\n");

    result.push_str("type Uuid guard\n");
    result.push_str("    = Uuid String\n\n\n");

    result.push_str("int : Int -> Integer guard\n");
    result.push_str("int =\n");
    result.push_str("    Integer\n\n\n");

    result.push_str("uuid : String -> Uuid guard\n");
    result.push_str("uuid =\n");
    result.push_str("    Uuid\n\n\n");

    result.push_str("encodeInt : Integer guard -> Encode.Value\n");
    result.push_str("encodeInt (Integer value) =\n");
    result.push_str("    Encode.int value\n\n\n");

    result.push_str("encodeUuid : Uuid guard -> Encode.Value\n");
    result.push_str("encodeUuid (Uuid value) =\n");
    result.push_str("    Encode.string value\n\n\n");

    result.push_str("decodeInt : Decode.Decoder (Integer guard)\n");
    result.push_str("decodeInt =\n");
    result.push_str("    Decode.map int Decode.int\n\n\n");

    result.push_str("decodeUuid : Decode.Decoder (Uuid guard)\n");
    result.push_str("decodeUuid =\n");
    result.push_str("    Decode.map uuid Decode.string\n\n\n");

    let brands = collect_id_brands(database);
    if !brands.is_empty() {
        result.push_str("-- Branded ID aliases\n\n");
        for (brand, kind) in brands {
            result.push_str(&format!("type {}Id\n", brand));
            result.push_str(&format!("    = {}Id\n\n", brand));

            let base_type = match kind {
                IdKind::Int => "Integer",
                IdKind::Uuid => "Uuid",
            };
            result.push_str(&format!(
                "type alias {} = {} {}Id\n\n\n",
                brand, base_type, brand
            ));
        }
    }

    result
}

fn collect_id_brands(database: &ast::Database) -> Vec<(String, IdKind)> {
    use std::collections::HashMap;

    let mut brands: HashMap<String, IdKind> = HashMap::new();

    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                if let ast::Definition::Record { fields, .. } = definition {
                    for field in fields {
                        if let ast::Field::Column(column) = field {
                            match &column.type_ {
                                ast::ColumnType::IdInt { table } if !table.is_empty() => {
                                    brands.entry(table.clone()).or_insert(IdKind::Int);
                                }
                                ast::ColumnType::IdUuid { table } if !table.is_empty() => {
                                    brands.entry(table.clone()).or_insert(IdKind::Uuid);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<(String, IdKind)> = brands.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn split_foreign_key_type(type_: &str) -> Option<(&str, &str)> {
    let mut parts = type_.split('.');
    let table = parts.next()?;
    let field = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some((table, field))
}

fn find_table<'a>(lookup: &'a ElmLookup, table_name: &str) -> Option<&'a ast::RecordDetails> {
    lookup.records_by_name.get(&table_name.to_ascii_lowercase())
}

fn get_id_kind_for_brand(lookup: &ElmLookup, brand: &str) -> Option<IdKind> {
    let table = find_table(lookup, brand)?;

    for field in &table.fields {
        if let ast::Field::Column(column) = field {
            if column.name == "id" {
                return match &column.type_ {
                    ast::ColumnType::IdInt { .. } => Some(IdKind::Int),
                    ast::ColumnType::IdUuid { .. } => Some(IdKind::Uuid),
                    _ => None,
                };
            }
        }
    }

    None
}

fn resolve_foreign_key_column_type(lookup: &ElmLookup, type_: &str) -> Option<ast::ColumnType> {
    let (table_name, field_name) = split_foreign_key_type(type_)?;
    let table = find_table(lookup, table_name)?;

    for field in &table.fields {
        if let ast::Field::Column(column) = field {
            if column.name == field_name {
                return Some(column.type_.clone());
            }
        }
    }

    None
}

fn id_encoder(kind: IdKind) -> &'static str {
    match kind {
        IdKind::Int => "Db.Id.encodeInt",
        IdKind::Uuid => "Db.Id.encodeUuid",
    }
}

fn id_decoder(kind: IdKind) -> &'static str {
    match kind {
        IdKind::Int => "Db.Id.decodeInt",
        IdKind::Uuid => "Db.Id.decodeUuid",
    }
}

pub fn write_schema(database: &ast::Database) -> String {
    let mut result = String::new();

    result
        .push_str("module Db exposing (..)\n\nimport Db.Id\nimport Dict exposing (Dict)\nimport Json.Encode\nimport Time\n\n\n");

    result.push_str("type alias DateTime =\n    Time.Posix\n\n\n");
    result.push_str("type alias Json =\n    Json.Encode.Value\n\n\n");

    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                if let ast::Definition::Tagged { .. } = definition {
                    result.push_str(&to_string_definition(definition));
                }
            }
        }
    }

    result
}

fn to_string_definition(definition: &ast::Definition) -> String {
    match definition {
        ast::Definition::Lines { count } => {
            if *count > 2 {
                "\n\n".to_string()
            } else {
                "\n".repeat(*count as usize)
            }
        }
        ast::Definition::Session(_) => "".to_string(),
        ast::Definition::Comment { .. } => "".to_string(),
        ast::Definition::SyncMode(_) => "".to_string(),
        ast::Definition::Tagged { name, variants, .. } => {
            let mut result = format!("type {}\n", name);
            let mut is_first = true;
            for variant in variants {
                result.push_str(&to_string_variant(is_first, 4, variant));
                is_first = false;
            }
            result.push('\n');
            result
        }
        ast::Definition::Record { name, fields, .. } => to_type_alias(name, fields),
    }
}

fn to_type_alias(name: &str, fields: &Vec<ast::Field>) -> String {
    let mut result = format!("type alias {} =\n", name);

    let mut is_first = true;
    for field in fields {
        if is_first & ast::is_column(field) {
            result.push_str(&format!("    {{ ",))
        }
        if ast::is_column_space(field) {
            continue;
        }

        result.push_str(&to_string_field(is_first, 4, &field));

        if is_first & ast::is_column(field) {
            is_first = false;
        }
    }
    result.push_str("    }\n\n");
    result
}

fn to_string_variant(is_first: bool, indent_size: usize, variant: &ast::Variant) -> String {
    let prefix = if is_first { " = " } else { " | " };

    match &variant.fields {
        Some(fields) => {
            let indent = " ".repeat(indent_size + 2);
            let mut result = format!("   {}{}\n{}{{ ", prefix, variant.name, indent);

            let mut is_first_field = true;
            for field in fields {
                result.push_str(&to_string_field(is_first_field, 6, &field));
                if ast::is_column(field) {
                    is_first_field = false;
                }
            }
            result.push_str("      }\n");
            result
        }
        None => format!("   {}{}\n", prefix, variant.name),
    }
}

fn to_string_field(is_first: bool, indent: usize, field: &ast::Field) -> String {
    match field {
        ast::Field::ColumnLines { count } => {
            if *count > 2 {
                "\n\n".to_string()
            } else {
                "\n".repeat(*count as usize)
            }
        }
        ast::Field::Column(column) => to_string_column(is_first, indent, column),
        ast::Field::ColumnComment { .. } => "".to_string(),
        ast::Field::FieldDirective(_) => "".to_string(),
    }
}

fn to_string_column(is_first: bool, indent: usize, column: &ast::Column) -> String {
    let maybe = if column.nullable { "Maybe " } else { "" };
    let type_str = column_to_elm_type(column);

    if is_first {
        return format!("{} : {}{}\n", column.name, maybe, type_str);
    } else {
        let spaces = " ".repeat(indent);
        return format!("{}, {} : {}{}\n", spaces, column.name, maybe, type_str);
    }
}

/// Convert a column to its Elm type representation
/// For ID types with brands, generates branded types like `UserId` or `Uuid User`
fn column_to_elm_type(column: &ast::Column) -> String {
    elm_type_from_column_type(&column.type_, false)
}

fn elm_type_from_column_type(type_: &ast::ColumnType, qualify_db_types: bool) -> String {
    match type_ {
        ast::ColumnType::String => "String".to_string(),
        ast::ColumnType::Int => "Int".to_string(),
        ast::ColumnType::Float => "Float".to_string(),
        ast::ColumnType::Bool => "Bool".to_string(),
        ast::ColumnType::DateTime => "Time.Posix".to_string(),
        ast::ColumnType::Date => "String".to_string(),
        ast::ColumnType::Json => {
            if qualify_db_types {
                "Db.Json".to_string()
            } else {
                "Json".to_string()
            }
        }
        ast::ColumnType::JsonTyped(inner) => elm_type_from_column_type(inner, qualify_db_types),
        ast::ColumnType::List(inner) => {
            format!(
                "List {}",
                elm_type_from_column_type(inner, qualify_db_types)
            )
        }
        ast::ColumnType::Dict(inner) => {
            format!(
                "Dict String {}",
                elm_type_from_column_type(inner, qualify_db_types)
            )
        }
        ast::ColumnType::Nullable(inner) => {
            format!(
                "Maybe {}",
                elm_type_from_column_type(inner, qualify_db_types)
            )
        }
        ast::ColumnType::IdInt { table } => {
            if !table.is_empty() {
                format!("Db.Id.{}", table)
            } else {
                "Int".to_string()
            }
        }
        ast::ColumnType::IdUuid { table } => {
            if !table.is_empty() {
                format!("Db.Id.{}", table)
            } else {
                "String".to_string()
            }
        }
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if field == "id" {
                format!("Db.Id.{}", table)
            } else {
                "String".to_string()
            }
        }
        ast::ColumnType::Custom(name) => {
            if qualify_db_types {
                format!("Db.{}", name)
            } else {
                name.clone()
            }
        }
    }
}

// DECODE

pub fn to_schema_decoders(database: &ast::Database) -> String {
    let mut result = String::new();

    result.push_str("module Db.Decode exposing (..)\n\n");

    result.push_str("import Db exposing (..)\n");
    result.push_str("import Dict exposing (Dict)\n");
    result.push_str("import Json.Decode as Decode\n");
    result.push_str("import Db.Id\n");
    result.push_str("import Time\n\n\n");

    result.push_str(
        r#"field : String -> Decode.Decoder a -> Decode.Decoder (a -> b) -> Decode.Decoder b
field fieldName_ fieldDecoder_ decoder_ =
    decoder_ |> Decode.andThen (\func -> Decode.field fieldName_ fieldDecoder_ |> Decode.map func)


{-| Chain field decoders together.
This allows you to build up a decoder by adding fields one at a time.

    decodeGame =
        Decode.succeed Game
            |> andField "id" Decode.int 
            |> andField "name" Decode.string

-}
andField : String -> Decode.Decoder a -> Decode.Decoder (a -> b) -> Decode.Decoder b
andField fieldName decoder partial =
    Decode.map2 (\f value -> f value)
        partial
        (Decode.field fieldName decoder)


bool : Decode.Decoder Bool
bool =
    Decode.oneOf
        [ Decode.bool
        , Decode.int
            |> Decode.andThen
                (\int ->
                    case int of
                        0 ->
                            Decode.succeed False

                        _ ->
                            Decode.succeed True
                )
                ]


dateTime : Decode.Decoder Time.Posix
dateTime =
    Decode.map (\seconds -> Time.millisToPosix (seconds * 1000)) Decode.int


json : Decode.Decoder Json
json =
    Decode.value

"#,
    );

    result.push_str("\n\n");

    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                to_decoder_definition(database, definition, &mut result);
            }
        }
    }
    result
}

fn to_decoder_definition(
    database: &ast::Database,
    definition: &ast::Definition,
    result: &mut String,
) {
    match definition {
        ast::Definition::Lines { .. } => (),
        ast::Definition::Session(_) => (),
        ast::Definition::Comment { .. } => (),
        ast::Definition::SyncMode(_) => (),
        ast::Definition::Tagged { name, variants, .. } => {
            for variant in variants {
                match &variant.fields {
                    Some(fields) => {
                        result.push_str(&to_type_alias(
                            &format!("{}_{}", name, variant.name),
                            fields,
                        ));
                    }
                    None => continue,
                }
            }

            result.push_str(&format!(
                "{} : Decode.Decoder Db.{}\n",
                crate::ext::string::decapitalize(name),
                name
            ));
            result.push_str(&format!("{} =\n", crate::ext::string::decapitalize(name)));
            result.push_str("    Decode.field \"_type\" Decode.string\n");
            result.push_str("        |> Decode.andThen\n");
            result.push_str("            (\\variant_name ->\n");
            result.push_str("               case variant_name of\n");

            for variant in variants {
                result.push_str(&to_decoder_variant(database, 18, name, variant));
            }
            result.push_str("                  _ ->\n");
            result.push_str("                      Decode.fail (\"Unknown variant for ");
            result.push_str(name);
            result.push_str(": \" ++ variant_name)\n");
            result.push_str("            )\n");
            result.push_str("\n\n");
        }
        ast::Definition::Record { .. } => (),
    }
}

fn to_decoder_variant(
    database: &ast::Database,
    indent_size: usize,
    typename: &str,
    variant: &ast::Variant,
) -> String {
    let outer_indent = " ".repeat(indent_size);
    let indent = " ".repeat(indent_size + 4);
    let inner_indent = " ".repeat(indent_size + 8);
    match &variant.fields {
        Some(fields) => {
            let mut result = format!(
                "{}\"{}\" ->\n{}Decode.map Db.{}\n{}(Decode.succeed {}_{}\n",
                outer_indent,
                variant.name,
                indent,
                variant.name,
                inner_indent,
                typename,
                variant.name
            );

            for field in fields {
                result.push_str(&to_variant_field_json_decoder(
                    database,
                    indent_size + 12,
                    field,
                ));
            }
            result.push_str(&format!("{})\n", inner_indent));

            result
        }
        None => format!(
            "{}\"{}\" ->\n{}Decode.succeed Db.{}\n",
            outer_indent, variant.name, indent, variant.name
        ),
    }
}

// Field directives(specifically @link) is not allowed within a type at the moment
fn to_variant_field_json_decoder(
    database: &ast::Database,
    indent: usize,
    field: &ast::Field,
) -> String {
    match field {
        ast::Field::Column(column) => {
            let spaces = " ".repeat(indent);
            let field_decoder = to_json_type_decoder(database, &column.type_);
            let field_decoder = if column.nullable {
                format!("(Decode.nullable {})", field_decoder)
            } else {
                field_decoder
            };
            return format!("{}|> field \"{}\" {}\n", spaces, column.name, field_decoder);
        }
        _ => "".to_string(),
    }
}

fn id_kind_for_brand(database: &ast::Database, brand: &str) -> Option<IdKind> {
    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                if let ast::Definition::Record { name, fields, .. } = definition {
                    if !name.eq_ignore_ascii_case(brand) {
                        continue;
                    }

                    for field in fields {
                        if let ast::Field::Column(column) = field {
                            if column.name == "id" {
                                return match &column.type_ {
                                    ast::ColumnType::IdInt { .. } => Some(IdKind::Int),
                                    ast::ColumnType::IdUuid { .. } => Some(IdKind::Uuid),
                                    _ => None,
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn to_json_type_decoder(database: &ast::Database, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "Decode.string".to_string(),
        ast::ColumnType::Int => "Decode.int".to_string(),
        ast::ColumnType::Float => "Decode.float".to_string(),
        ast::ColumnType::Bool => "bool".to_string(),
        ast::ColumnType::DateTime => "dateTime".to_string(),
        ast::ColumnType::Date => "Decode.string".to_string(),
        ast::ColumnType::Json => "json".to_string(),
        ast::ColumnType::JsonTyped(inner) => to_json_type_decoder(database, inner),
        ast::ColumnType::List(inner) => {
            format!("(Decode.list {})", to_json_type_decoder(database, inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!("(Decode.dict {})", to_json_type_decoder(database, inner))
        }
        ast::ColumnType::Nullable(inner) => {
            format!(
                "(Decode.nullable {})",
                to_json_type_decoder(database, inner)
            )
        }
        ast::ColumnType::IdInt { .. } => "Db.Id.decodeInt".to_string(),
        ast::ColumnType::IdUuid { .. } => "Db.Id.decodeUuid".to_string(),
        ast::ColumnType::ForeignKey { table, field, .. } if field == "id" => {
            match id_kind_for_brand(database, table) {
                Some(IdKind::Int) => "Db.Id.decodeInt".to_string(),
                Some(IdKind::Uuid) => "Db.Id.decodeUuid".to_string(),
                None => "Db.Id.decodeUuid".to_string(),
            }
        }
        _ => crate::ext::string::decapitalize(&type_.to_string()).to_string(),
    }
}

// Encoders!
//
pub fn to_schema_encoders(database: &ast::Database) -> String {
    let mut result = String::new();

    result.push_str(
        "module Db.Encode exposing (..)\n\nimport Db\nimport Db.Id\nimport Json.Encode as Encode\nimport Time\n\n\n",
    );
    result = result.replacen(
        "import Db.Id\n",
        "import Db.Id\nimport Dict exposing (Dict)\n",
        1,
    );

    result.push_str("dateTime : Time.Posix -> Encode.Value\n");
    result.push_str("dateTime time =\n");
    result.push_str("    Encode.int (Time.posixToMillis time // 1000)\n\n");

    result.push_str("json : Db.Json -> Encode.Value\n");
    result.push_str("json value =\n");
    result.push_str("    value\n\n");

    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                result.push_str(&to_encoder_definition(database, definition));
            }
        }
    }
    result
}

fn to_encoder_definition(database: &ast::Database, definition: &ast::Definition) -> String {
    match definition {
        ast::Definition::Lines { .. } => "".to_string(),
        ast::Definition::Comment { .. } => "".to_string(),
        ast::Definition::Session(_) => "".to_string(),
        ast::Definition::SyncMode(_) => "".to_string(),
        ast::Definition::Tagged { name, variants, .. } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} : Db.{} -> Encode.Value\n",
                string::decapitalize(name),
                name
            ));
            result.push_str(&format!("{} input_ =\n", string::decapitalize(name)));
            result.push_str("    case input_ of\n");

            for variant in variants {
                result.push_str(&to_encoder_variant(database, 8, name, variant));
            }
            result
        }
        ast::Definition::Record { .. } => "".to_string(),
    }
}

fn to_encoder_variant(
    database: &ast::Database,
    indent_size: usize,
    _typename: &str,
    variant: &ast::Variant,
) -> String {
    let outer_indent = " ".repeat(indent_size);
    let indent = " ".repeat(indent_size + 4);
    let inner_indent = " ".repeat(indent_size + 8);
    match &variant.fields {
        Some(fields) => {
            let mut result = format!(
                "{}Db.{} inner_details__ ->\n{}Encode.object\n{}[ ( \"_type\", Encode.string \"{}\" )\n",
                outer_indent, variant.name, indent, inner_indent, variant.name
            );

            for field in fields {
                result.push_str(&to_field_encoder(database, indent_size + 8, field));
            }
            result.push_str(&format!("{}]\n\n", inner_indent));

            result
        }
        None => format!(
            "{}Db.{} ->\n{}Encode.object [ ( \"_type\", Encode.string \"{}\" ) ]\n\n",
            outer_indent, variant.name, indent, variant.name
        ),
    }
}

fn to_field_encoder(database: &ast::Database, indent: usize, field: &ast::Field) -> String {
    match field {
        ast::Field::Column(column) => {
            let spaces = " ".repeat(indent);
            let encoder = to_type_encoder(database, &column.type_);
            let value_expr = if column.nullable {
                format!(
                    "(Maybe.map {} inner_details__.{} |> Maybe.withDefault Encode.null)",
                    encoder, column.name
                )
            } else {
                format!("{} inner_details__.{}", encoder, column.name)
            };
            return format!("{}, ( \"{}\", {})\n", spaces, column.name, value_expr);
        }

        _ => "".to_string(),
    }
}

fn to_type_encoder(database: &ast::Database, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "Encode.string".to_string(),
        ast::ColumnType::Int => "Encode.int".to_string(),
        ast::ColumnType::Float => "Encode.float".to_string(),
        ast::ColumnType::Bool => "Encode.bool".to_string(),
        ast::ColumnType::DateTime => "dateTime".to_string(),
        ast::ColumnType::Date => "Encode.string".to_string(),
        ast::ColumnType::Json => "json".to_string(),
        ast::ColumnType::JsonTyped(inner) => to_type_encoder(database, inner),
        ast::ColumnType::List(inner) => {
            format!("Encode.list {}", to_type_encoder(database, inner))
        }
        ast::ColumnType::Dict(inner) => format!(
            "(\\dict__ -> dict__ |> Dict.toList |> List.map (\\( key__, value__ ) -> ( key__, {} value__ )) |> Encode.object)",
            to_type_encoder(database, inner)
        ),
        ast::ColumnType::Nullable(inner) => format!(
            "(\\maybe__ -> Maybe.map ({} ) maybe__ |> Maybe.withDefault Encode.null)",
            to_type_encoder(database, inner)
        ),
        ast::ColumnType::IdInt { .. } => "Db.Id.encodeInt".to_string(),
        ast::ColumnType::IdUuid { .. } => "Db.Id.encodeUuid".to_string(),
        ast::ColumnType::ForeignKey { table, field, .. } if field == "id" => {
            match id_kind_for_brand(database, table) {
                Some(IdKind::Int) => "Db.Id.encodeInt".to_string(),
                Some(IdKind::Uuid) => "Db.Id.encodeUuid".to_string(),
                None => "Db.Id.encodeUuid".to_string(),
            }
        }
        _ => string::decapitalize(&type_.to_string()),
    }
}

fn to_type_encoder_str(lookup: &ElmLookup, type_: &str) -> String {
    to_elm_encoder(lookup, &ast::ColumnType::from_str(type_))
}

fn to_elm_encoder(lookup: &ElmLookup, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "Encode.string".to_string(),
        ast::ColumnType::Int => "Encode.int".to_string(),
        ast::ColumnType::Float => "Encode.float".to_string(),
        ast::ColumnType::Bool => "Encode.bool".to_string(),
        ast::ColumnType::DateTime => "Db.Encode.dateTime".to_string(),
        ast::ColumnType::Date => "Encode.string".to_string(),
        ast::ColumnType::Json => "Db.Encode.json".to_string(),
        ast::ColumnType::JsonTyped(inner) => to_elm_encoder(lookup, inner),
        ast::ColumnType::List(inner) => format!("Encode.list {}", to_elm_encoder(lookup, inner)),
        ast::ColumnType::Dict(inner) => format!(
            "(\\dict__ -> dict__ |> Dict.toList |> List.map (\\( key__, value__ ) -> ( key__, {} value__ )) |> Encode.object)",
            to_elm_encoder(lookup, inner)
        ),
        ast::ColumnType::Nullable(inner) => format!(
            "(\\maybe__ -> Maybe.map ({} ) maybe__ |> Maybe.withDefault Encode.null)",
            to_elm_encoder(lookup, inner)
        ),
        ast::ColumnType::IdInt { .. } => "Db.Id.encodeInt".to_string(),
        ast::ColumnType::IdUuid { .. } => "Db.Id.encodeUuid".to_string(),
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if field == "id" {
                if let Some(kind) = get_id_kind_for_brand(lookup, table) {
                    return id_encoder(kind).to_string();
                }
            }

            if let Some(col_type) = resolve_foreign_key_column_type(lookup, &type_.to_string()) {
                return to_elm_encoder(lookup, &col_type);
            }

            "Encode.string".to_string()
        }
        ast::ColumnType::Custom(name) => format!("Db.Encode.{}", string::decapitalize(name)),
    }
}

//  QUERIES
//

pub fn generate_queries(
    context: &typecheck::Context,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    query_list: &ast::QueryList,
    base_out_dir: &Path,
    files: &mut Vec<GeneratedFile<String>>,
) {
    let mut query_names: Vec<String> = Vec::new();

    for operation in &query_list.queries {
        match operation {
            ast::QueryDef::Query(q) => {
                // Only generate QueryClient for select queries, not mutations
                if q.operation == ast::QueryOperation::Query {
                    query_names.push(q.name.clone());
                }
                files.push(generate_text_file(
                    base_out_dir
                        .join("Query")
                        .join(format!("{}.elm", q.name.to_string())),
                    to_query_file(context, all_query_info.get(&q.name), q),
                ));
            }
            ast::QueryDef::QueryComment { .. } | ast::QueryDef::QueryLines { .. } => continue,
        }
    }

    // Generate the Pyre.elm module that ties all queries together
    if !query_names.is_empty() {
        files.push(generate_text_file(
            base_out_dir.join("Pyre.elm"),
            generate_pyre_module(context, all_query_info, query_list, &query_names),
        ));
    }
}

fn to_query_file(
    context: &typecheck::Context,
    query_info: Option<&typecheck::QueryInfo>,
    query: &ast::Query,
) -> String {
    // Collect type names as we generate them
    use std::cell::RefCell;
    use std::rc::Rc;

    let type_names: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let elm_lookup = ElmLookup::from_context(context);

    // Always include Input
    type_names.borrow_mut().push("Input".to_string());

    let mut result = String::new();

    result.push_str("import Db\n");
    result.push_str("import Db.Database\n");
    result.push_str("import Db.Decode\n");
    result.push_str("import Db.Delta\n");
    result.push_str("import Db.Encode\n");
    result.push_str("import Db.Id\n");
    result.push_str("import Dict exposing (Dict)\n");
    if query.operation == ast::QueryOperation::Update {
        result.push_str("import Db.Updates\n");
    }
    result.push_str("import Json.Decode as Decode\n");
    result.push_str("import Json.Encode as Encode\n");
    result.push_str("import Time\n");
    result.push_str("\n\n");

    if query.operation != ast::QueryOperation::Query {
        result.push_str(&format!(
            "id : String\nid =\n    \"{}\"\n\n\n",
            query.interface_hash
        ));
        result.push_str(&format!(
            "name : String\nname =\n    \"{}\"\n\n\n",
            query.name
        ));
    }

    result.push_str(&to_param_type_alias(
        &elm_lookup,
        &query.operation,
        &query.args,
    ));

    let type_names_clone = type_names.clone();
    let elm_lookup_for_types = elm_lookup.clone();
    let formatter = typealias::TypeFormatter {
        to_comment: Box::new(|s| format!("{{-| {} -}}\n", s)),
        to_type_def_start: Box::new(move |name| {
            type_names_clone.borrow_mut().push(name.to_string());
            format!("type alias {} =\n    {{ ", name)
        }),
        to_field: Box::new(
            move |name,
                  type_,
                  typealias::FieldMetadata {
                      is_link,
                      is_optional,
                      is_array_relationship: _,
                  }| {
                let base_type = to_elm_typename(&elm_lookup_for_types, type_, is_link);

                let type_str = if is_link {
                    if is_optional {
                        format!("Maybe {}", base_type)
                    } else {
                        format!("List {}", base_type)
                    }
                } else {
                    if is_optional {
                        format!("Maybe {}", base_type)
                    } else {
                        base_type.to_string()
                    }
                };
                format!("{} : {}\n", name, type_str)
            },
        ),
        to_type_def_end: Box::new(|| "    }\n".to_string()),
        to_field_separator: Box::new(|is_last| {
            if is_last {
                "".to_string()
            } else {
                "    , ".to_string()
            }
        }),
    };

    // Type Alisaes
    typealias::return_data_aliases(context, query, &mut result, &formatter);

    // Helpers

    let elm_lookup_for_decoders = elm_lookup.clone();
    let decoder_formatter = typealias::TypeFormatter {
        to_comment: Box::new(|s| format!("{{-| {} -}}\n", s)),
        to_type_def_start: Box::new(|name| {
            format!(
                "decode{} : Decode.Decoder {}\ndecode{} =\n    Decode.succeed {}\n        ",
                name, name, name, name
            )
        }),
        to_field: Box::new(
            move |name,
                  type_,
                  typealias::FieldMetadata {
                      is_link,
                      is_optional,
                      is_array_relationship: _,
                  }| {
                let decoder = to_elm_decoder(&elm_lookup_for_decoders, type_, is_link);

                let final_decoder: String = if is_optional {
                    format!("(Decode.nullable {})", decoder)
                } else {
                    if is_link {
                        format!("(Decode.oneOf [ Decode.list {}, Decode.null [] ])", decoder)
                    } else {
                        decoder.to_string()
                    }
                };

                format!("|> Db.Decode.andField \"{}\" {}\n", name, final_decoder)
            },
        ),
        to_type_def_end: Box::new(|| "\n".to_string()),
        to_field_separator: Box::new(|_| "        ".to_string()),
    };

    result.push_str(&to_param_type_encoder(
        &elm_lookup,
        &query.operation,
        &query.args,
    ));

    // Top level query decoder

    typealias::return_data_aliases(context, query, &mut result, &decoder_formatter);

    if query.operation != ast::QueryOperation::Query {
        let database_type = query_info
            .map(|info| elm_database_id_type(&info.primary_db))
            .unwrap_or_else(|| elm_database_id_type(ast::DEFAULT_SCHEMANAME));
        result.push_str(
            "type alias DatabaseId namespace =\n    Db.Database.DatabaseId namespace\n\n\ntype alias RequestId =\n    String\n\n\ntype alias MutationResult =\n    { requestId : RequestId\n    , mutationId : String\n    , mutationName : Maybe String\n    , result : Result String ReturnData\n    }\n\n\n",
        );
        if let Some(info) = query_info {
            let namespace = elm_database_namespace(&info.primary_db);
            result.push_str(&format!(
                "type alias {} =\n    Db.Database.{}\n\n\n",
                namespace, namespace
            ));
        }
        let optimistic_field = if optimistic_update_metadata(query).is_some() {
            "        , ( \"optimistic\", optimistic input )\n"
        } else {
            ""
        };
        result.push_str(&format!(
            "mutationRequest : {} -> RequestId -> Input -> Encode.Value\nmutationRequest databaseId requestId input =\n    Encode.object\n        [ ( \"type\", Encode.string \"mutate\" )\n        , ( \"databaseId\", Db.Database.encode databaseId )\n        , ( \"requestId\", Encode.string requestId )\n        , ( \"mutationId\", Encode.string id )\n        , ( \"mutationName\", Encode.string name )\n        , ( \"mutationInput\", encode input )\n{}        ]\n\n\n",
            database_type, optimistic_field
        ));
        if let Some(optimistic) = to_optimistic_update_elm(query) {
            result.push_str(&optimistic);
        }
        result.push_str(
            "decodeMutationResult : Decode.Decoder MutationResult\ndecodeMutationResult =\n    Decode.map4\n        (\\requestId mutationId mutationName mutationResult ->\n            { requestId = requestId\n            , mutationId = mutationId\n            , mutationName = mutationName\n            , result = mutationResult\n            }\n        )\n        (Decode.field \"requestId\" Decode.string)\n        (Decode.field \"mutationId\" Decode.string)\n        (Decode.oneOf\n            [ Decode.field \"mutationName\" (Decode.nullable Decode.string)\n            , Decode.succeed Nothing\n            ]\n        )\n        (Decode.field \"result\" (decodeBridgeMutationResult decodeReturnData))\n\n\n",
        );
        result.push_str(
            "decodeBridgeMutationResult : Decode.Decoder value -> Decode.Decoder (Result String value)\ndecodeBridgeMutationResult valueDecoder =\n    Decode.field \"ok\" Decode.bool\n        |> Decode.andThen\n            (\\ok ->\n                if ok then\n                    Decode.map Ok (Decode.field \"value\" valueDecoder)\n\n                else\n                    Decode.map Err (Decode.field \"error\" Decode.string)\n            )\n\n\n",
        );
    }

    // Generate queryShape as JSON encoder (only for queries)
    if query.operation == ast::QueryOperation::Query {
        result.push_str("\n\n");
        result.push_str(&to_query_shape_json(context, query));
    }

    // Generate QueryDelta types and application code (only for queries, not mutations)
    if query.operation == ast::QueryOperation::Query {
        result.push_str(&to_query_delta_types(context, query));
    }

    // Build exposing list: functions first, then types
    let mut exposing_items: Vec<String> = Vec::new();

    // Add encoder function
    exposing_items.push("encode".to_string());

    if query.operation != ast::QueryOperation::Query {
        exposing_items.push("DatabaseId".to_string());
        if let Some(info) = query_info {
            exposing_items.push(elm_database_namespace(&info.primary_db));
        }
        exposing_items.push("RequestId".to_string());
        exposing_items.push("id".to_string());
        exposing_items.push("name".to_string());
        exposing_items.push("mutationRequest".to_string());
        if optimistic_update_metadata(query).is_some() {
            exposing_items.push("optimistic".to_string());
        }
        exposing_items.push("decodeMutationResult".to_string());
        exposing_items.push("MutationResult".to_string());
    }

    // Add delta functions and types for queries (not mutations)
    if query.operation == ast::QueryOperation::Query {
        exposing_items.push("queryShape".to_string());
        exposing_items.push("applyDelta".to_string());
        exposing_items.push("decodeQueryDelta".to_string());
        exposing_items.push("QueryDelta(..)".to_string());
    }

    // Then add types (sorted)
    let mut type_names_sorted: Vec<String> = type_names.borrow().clone();
    type_names_sorted.sort();
    exposing_items.extend(type_names_sorted);

    // Build the module declaration with explicit exposing
    let exposing_list = exposing_items.join(", ");
    let module_decl = format!(
        "module Query.{} exposing ({})\n\n\n",
        query.name, exposing_list
    );

    // Replace the placeholder or prepend the module declaration
    // Since we started with an empty result, we need to prepend
    format!("{}{}", module_decl, result)
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

fn to_optimistic_update_elm(query: &ast::Query) -> Option<String> {
    let metadata = optimistic_update_metadata(query)?;
    let set_fields = metadata
        .set_fields
        .iter()
        .map(|(field, input)| {
            format!(
                "Encode.object [ ( \"field\", Encode.string {} ), ( \"input\", Encode.string {} ) ]",
                string::quote(field),
                string::quote(input)
            )
        })
        .collect::<Vec<String>>()
        .join("\n                , ");

    Some(format!(
        "optimistic : Input -> Encode.Value\noptimistic _ =\n    Encode.object\n        [ ( \"queryField\", Encode.string {} )\n        , ( \"where\"\n          , Encode.object\n                [ ( \"field\", Encode.string {} )\n                , ( \"input\", Encode.string {} )\n                ]\n          )\n        , ( \"set\"\n          , Encode.list identity\n                [ {}\n                ]\n          )\n        ]\n\n\n",
        string::quote(&metadata.query_field),
        string::quote(&metadata.where_field),
        string::quote(&metadata.where_input),
        set_fields
    ))
}

/// Generate an empty ReturnData constructor with the right number of empty lists
fn generate_empty_return_data(query: &ast::Query) -> String {
    let field_count = query
        .fields
        .iter()
        .filter(|f| matches!(f, ast::TopLevelQueryField::Field(_)))
        .count();

    if field_count == 0 {
        "ReturnData".to_string()
    } else {
        let empty_lists = vec!["[]"; field_count].join(" ");
        format!("ReturnData {}", empty_lists)
    }
}

fn to_param_type_alias(
    lookup: &ElmLookup,
    operation: &ast::QueryOperation,
    args: &Vec<ast::QueryParamDefinition>,
) -> String {
    let mut result = "type alias Input =\n".to_string();
    result.push_str("    {");
    let mut is_first = true;
    for arg in args {
        let type_string = &arg.type_.clone().unwrap_or("unknown".to_string());
        let base_type = to_elm_typename(lookup, type_string, false);
        let elm_type = if *operation == ast::QueryOperation::Update && arg.omittable {
            format!("Db.Updates.Update {}", base_type)
        } else {
            base_type
        };
        if is_first {
            result.push_str(&format!(" {} : {}\n", arg.name, elm_type));
            is_first = false;
        } else {
            result.push_str(&format!("    , {} : {}\n", arg.name, elm_type));
        }
    }
    result.push_str("    }\n\n\n");
    result
}

fn to_param_type_encoder(
    lookup: &ElmLookup,
    operation: &ast::QueryOperation,
    args: &Vec<ast::QueryParamDefinition>,
) -> String {
    let mut result = "encode : Input -> Encode.Value\n".to_string();
    result.push_str("encode input =\n");
    if *operation == ast::QueryOperation::Update {
        result.push_str("    Db.Updates.object");
    } else {
        result.push_str("    Encode.object");
    }

    if args.len() == 0 {
        result.push_str(" []\n\n\n");
        return result;
    } else {
        result.push_str("\n");
    }
    let mut is_first = true;
    for arg in args {
        let type_string = &arg.type_.clone().unwrap_or("unknown".to_string());
        let encoded_value = if *operation == ast::QueryOperation::Update && arg.omittable {
            to_update_arg_encoder_str(lookup, type_string, &arg.name)
        } else if *operation == ast::QueryOperation::Update {
            format!(
                "Db.Updates.set ({} input.{})",
                to_type_encoder_str(lookup, &type_string),
                &arg.name
            )
        } else {
            format!(
                "{} input.{}",
                to_type_encoder_str(lookup, &type_string),
                &arg.name
            )
        };
        if is_first {
            result.push_str(&format!(
                "        [ ( {}, {} )\n",
                string::quote(&arg.name),
                encoded_value
            ));
            is_first = false;
        } else {
            result.push_str(&format!(
                "        , ( {}, {} )\n",
                string::quote(&arg.name),
                encoded_value
            ));
        }
    }
    result.push_str("        ]\n\n\n");
    result
}

fn to_update_arg_encoder_str(lookup: &ElmLookup, type_: &str, field_name: &str) -> String {
    let encoder = to_type_encoder_str(lookup, type_);
    format!(
        "case input.{field_name} of\n            Db.Updates.Set value ->\n                Db.Updates.Set ({encoder} value)\n\n            Db.Updates.Unchanged ->\n                Db.Updates.Unchanged\n\n            Db.Updates.SetToNull ->\n                Db.Updates.SetToNull",
        field_name = field_name,
        encoder = encoder
    )
}

fn to_elm_typename(lookup: &ElmLookup, type_: &str, is_link: bool) -> String {
    if is_link {
        type_.to_string()
    } else {
        to_elm_type_from_column_type(lookup, &ast::ColumnType::from_str(type_))
    }
}

fn to_elm_type_from_column_type(lookup: &ElmLookup, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "String".to_string(),
        ast::ColumnType::Int => "Int".to_string(),
        ast::ColumnType::Float => "Float".to_string(),
        ast::ColumnType::Bool => "Bool".to_string(),
        ast::ColumnType::DateTime => "Time.Posix".to_string(),
        ast::ColumnType::Date => "String".to_string(),
        ast::ColumnType::Json => "Db.Json".to_string(),
        ast::ColumnType::JsonTyped(inner) => to_elm_type_from_column_type(lookup, inner),
        ast::ColumnType::List(inner) => {
            format!("List {}", to_elm_type_from_column_type(lookup, inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!(
                "Dict String {}",
                to_elm_type_from_column_type(lookup, inner)
            )
        }
        ast::ColumnType::Nullable(inner) => {
            format!("Maybe {}", to_elm_type_from_column_type(lookup, inner))
        }
        ast::ColumnType::IdInt { table } => {
            if !table.is_empty() {
                format!("Db.Id.{}", table)
            } else {
                "Int".to_string()
            }
        }
        ast::ColumnType::IdUuid { table } => {
            if !table.is_empty() {
                format!("Db.Id.{}", table)
            } else {
                "String".to_string()
            }
        }
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if field == "id" {
                if let Some(table_def) = find_table(lookup, table) {
                    return format!("Db.Id.{}", table_def.name);
                }
                return format!("Db.Id.{}", table);
            }

            if let Some(col_type) = resolve_foreign_key_column_type(lookup, &type_.to_string()) {
                return to_elm_type_from_column_type(lookup, &col_type);
            }

            "String".to_string()
        }
        ast::ColumnType::Custom(name) => format!("Db.{}", name),
    }
}

fn to_elm_decoder(lookup: &ElmLookup, type_: &str, is_link: bool) -> String {
    if is_link {
        format!("decode{}", type_)
    } else {
        to_elm_decoder_from_column_type(lookup, &ast::ColumnType::from_str(type_))
    }
}

fn to_elm_decoder_from_column_type(lookup: &ElmLookup, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "Decode.string".to_string(),
        ast::ColumnType::Int => "Decode.int".to_string(),
        ast::ColumnType::Float => "Decode.float".to_string(),
        ast::ColumnType::DateTime => "Db.Decode.dateTime".to_string(),
        ast::ColumnType::Date => "Decode.string".to_string(),
        ast::ColumnType::Bool => "Db.Decode.bool".to_string(),
        ast::ColumnType::Json => "Db.Decode.json".to_string(),
        ast::ColumnType::JsonTyped(inner) => to_elm_decoder_from_column_type(lookup, inner),
        ast::ColumnType::List(inner) => {
            format!(
                "(Decode.list {})",
                to_elm_decoder_from_column_type(lookup, inner)
            )
        }
        ast::ColumnType::Dict(inner) => {
            format!(
                "(Decode.dict {})",
                to_elm_decoder_from_column_type(lookup, inner)
            )
        }
        ast::ColumnType::Nullable(inner) => {
            format!(
                "Decode.nullable ({})",
                to_elm_decoder_from_column_type(lookup, inner)
            )
        }
        ast::ColumnType::IdInt { .. } => "Db.Id.decodeInt".to_string(),
        ast::ColumnType::IdUuid { .. } => "Db.Id.decodeUuid".to_string(),
        ast::ColumnType::ForeignKey { table, field, .. } => {
            if field == "id" {
                if let Some(kind) = get_id_kind_for_brand(lookup, table) {
                    return id_decoder(kind).to_string();
                }
            }

            if let Some(col_type) = resolve_foreign_key_column_type(lookup, &type_.to_string()) {
                return to_elm_decoder_from_column_type(lookup, &col_type);
            }

            "Decode.string".to_string()
        }
        ast::ColumnType::Custom(name) => {
            format!("Db.Decode.{}", crate::ext::string::decapitalize(name))
        }
    }
}

fn to_query_shape_json(context: &typecheck::Context, query: &ast::Query) -> String {
    let mut result = "queryShape : Encode.Value\n".to_string();
    result.push_str("queryShape =\n");
    result.push_str("    Encode.object\n");
    result.push_str("        [ ");

    let mut is_first_table = true;
    for field in &query.fields {
        match field {
            ast::TopLevelQueryField::Field(query_field) => {
                if !is_first_table {
                    result.push_str("\n        , ");
                }
                is_first_table = false;

                let field_name = ast::get_aliased_name(query_field);
                result.push_str(&format!(
                    "({}, {})",
                    string::quote(&field_name),
                    to_query_field_spec_json(
                        context,
                        query_field,
                        context.tables.get(&query_field.name),
                        3,
                    )
                ));
            }
            _ => {}
        }
    }

    result.push_str("\n        ]\n");
    result
}

fn to_query_field_spec_json(
    context: &typecheck::Context,
    query_field: &ast::QueryField,
    table: Option<&typecheck::Table>,
    indent_level: usize,
) -> String {
    let indent = "    ".repeat(indent_level);
    let mut result = format!("Encode.object\n{}[ ", indent);
    let mut is_first = true;

    // Get table info for relationship detection
    let table = table.or_else(|| context.tables.get(&query_field.name));

    // Extract special directives (@where, @sort, @limit)
    let mut where_clause: Option<String> = None;
    let mut sort_clauses: Vec<String> = Vec::new();
    let mut limit: Option<i32> = None;

    // Collect all field selections and args
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
                    where_clause = Some(to_where_clause_elm(where_arg, indent_level + 1));
                }
                ast::Arg::OrderBy(direction, field_name) => {
                    let dir_str = match direction {
                        ast::Direction::Asc => "asc",
                        ast::Direction::Desc => "desc",
                    };
                    sort_clauses.push(format!(
                            "Encode.object [ (\"field\", Encode.string {}) , (\"direction\", Encode.string {}) ]",
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
        result.push_str(&format!("({}, {})", string::quote("@where"), where_clause));
        is_first = false;
    }

    // Generate field selections
    for (field_name, aliased_name, is_relationship, has_nested_fields) in field_selections {
        if !is_first {
            result.push_str(&format!("\n{}, ", indent));
        }
        is_first = false;

        if is_relationship && has_nested_fields {
            // Relationship field with nested selections - recurse
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
                result.push_str(&format!(
                    "({}, {})",
                    string::quote(&aliased_name),
                    to_query_field_spec_json_with_source(
                        context,
                        nested_field,
                        nested_table,
                        indent_level + 1,
                        if aliased_name == field_name {
                            None
                        } else {
                            Some(field_name.as_str())
                        },
                    )
                ));
            }
        } else {
            // Regular field or relationship without nested - just true
            result.push_str(&to_query_shape_leaf_elm(
                &field_name,
                &aliased_name,
                indent_level,
            ));
        }
    }

    // Add special directives if present
    if !sort_clauses.is_empty() || limit.is_some() {
        if !is_first {
            result.push_str(&format!("\n{}, ", indent));
        }

        if !sort_clauses.is_empty() {
            if sort_clauses.len() == 1 {
                result.push_str(&format!("(\"@sort\", {})", sort_clauses[0]));
            } else {
                // For multiple sort clauses, create a list
                result.push_str("(\"@sort\", Encode.list identity [");
                for (i, clause) in sort_clauses.iter().enumerate() {
                    if i > 0 {
                        result.push_str(", ");
                    }
                    result.push_str(clause);
                }
                result.push_str("])");
            }
        }

        if let Some(limit_val) = limit {
            if !sort_clauses.is_empty() {
                result.push_str(&format!("\n{}, ", indent));
            }
            result.push_str(&format!("(\"@limit\", Encode.int {})", limit_val));
        }
    }

    result.push_str(&format!("\n{}]", indent));
    result
}

fn to_where_clause_elm(where_arg: &ast::WhereArg, indent_level: usize) -> String {
    let indent = "    ".repeat(indent_level);

    match where_arg {
        ast::WhereArg::Column(is_session_field, field_name, operator, value, _) => {
            let key = if *is_session_field {
                format!("Session.{}", field_name)
            } else {
                field_name.clone()
            };

            match operator {
                ast::Operator::Equal => format!(
                    "Encode.object\n{}[ ({}, {})\n{}]",
                    indent,
                    string::quote(&key),
                    to_query_value_elm(value, indent_level + 1),
                    indent
                ),
                _ => format!(
                    "Encode.object\n{}[ ({}, Encode.object\n{}    [ ({}, {})\n{}    ]\n{})\n{}]",
                    indent,
                    string::quote(&key),
                    indent,
                    string::quote(to_filter_operator_key(operator)),
                    to_query_value_elm(value, indent_level + 2),
                    indent,
                    indent,
                    indent
                ),
            }
        }
        ast::WhereArg::And(items) => format!(
            "Encode.object\n{}[ (\"$and\", Encode.list identity\n{}    [ {}\n{}    ])\n{}]",
            indent,
            indent,
            items
                .iter()
                .map(|item| to_where_clause_elm(item, indent_level + 2))
                .collect::<Vec<_>>()
                .join(&format!("\n{}    , ", indent)),
            indent,
            indent
        ),
        ast::WhereArg::Or(items) => format!(
            "Encode.object\n{}[ (\"$or\", Encode.list identity\n{}    [ {}\n{}    ])\n{}]",
            indent,
            indent,
            items
                .iter()
                .map(|item| to_where_clause_elm(item, indent_level + 2))
                .collect::<Vec<_>>()
                .join(&format!("\n{}    , ", indent)),
            indent,
            indent
        ),
    }
}

fn to_query_value_elm(value: &ast::QueryValue, indent_level: usize) -> String {
    let indent = "    ".repeat(indent_level);

    match value {
        ast::QueryValue::Variable((_, details)) => match &details.session_field {
            Some(field) => format!(
                "Encode.object\n{}[ (\"$session\", Encode.string {})\n{}]",
                indent,
                string::quote(field),
                indent
            ),
            None => format!(
                "Encode.object\n{}[ (\"$var\", Encode.string {})\n{}]",
                indent,
                string::quote(&details.name),
                indent
            ),
        },
        ast::QueryValue::String((_, value)) => format!("Encode.string {}", string::quote(value)),
        ast::QueryValue::Int((_, value)) => format!("Encode.int {}", value),
        ast::QueryValue::Float((_, value)) => format!("Encode.float {}", value),
        ast::QueryValue::Bool((_, value)) => format!("Encode.bool {}", value),
        ast::QueryValue::Null(_) => "Encode.null".to_string(),
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            let mut fields = vec![format!(
                "(\"type\", Encode.string {})",
                string::quote(&details.name)
            )];
            if let Some(assignments) = &details.fields {
                for (name, value) in assignments {
                    fields.push(format!(
                        "({}, {})",
                        string::quote(name),
                        to_query_value_elm(value, indent_level + 1)
                    ));
                }
            }
            format!(
                "Encode.object\n{}[ {}\n{}]",
                indent,
                fields.join(&format!("\n{}{}, ", indent, "")),
                indent
            )
        }
        ast::QueryValue::Fn(func) => format!(
            "Encode.object\n{}[ (\"$fn\", Encode.string {})\n{}]",
            indent,
            string::quote(&func.name),
            indent
        ),
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

// QUERY DELTA TYPES AND APPLICATION CODE
//
// Generates types and functions for applying deltas to query results using lenses

fn to_query_delta_types(context: &typecheck::Context, query: &ast::Query) -> String {
    let mut result = String::new();

    result.push_str("\n\n-- QueryDelta Types\n\n\n");

    // QueryDelta envelope type (uses DeltaOp from Db.Delta)
    result.push_str("type QueryDelta\n");
    result.push_str("    = Full Int ReturnData\n");
    result.push_str("    | Delta Int (List Db.Delta.DeltaOp)\n\n\n");

    // QueryDelta decoder (uses Db.Delta.decodeDeltaOp)
    result.push_str("decodeQueryDelta : Decode.Decoder QueryDelta\n");
    result.push_str("decodeQueryDelta =\n");
    result.push_str("    Decode.field \"type\" Decode.string\n");
    result.push_str("        |> Decode.andThen\n");
    result.push_str("            (\\type_ ->\n");
    result.push_str("                case type_ of\n");
    result.push_str("                    \"full\" ->\n");
    result.push_str("                        Decode.map2 Full\n");
    result.push_str("                            (Decode.field \"revision\" Decode.int)\n");
    result.push_str("                            (Decode.field \"result\" decodeReturnData)\n\n");
    result.push_str("                    \"delta\" ->\n");
    result.push_str("                        Decode.map2 Delta\n");
    result.push_str("                            (Decode.field \"revision\" Decode.int)\n");
    result.push_str("                            (Decode.at [ \"delta\", \"ops\" ] (Decode.list Db.Delta.decodeDeltaOp))\n\n");
    result.push_str("                    _ ->\n");
    result
        .push_str("                        Decode.fail (\"Unknown QueryDelta type: \" ++ type_)\n");
    result.push_str("            )\n\n\n");

    // Generate applyDelta function with lens-based approach
    result.push_str(&to_apply_delta_function_with_lenses(context, query));

    result
}

fn to_query_field_spec_json_with_source(
    context: &typecheck::Context,
    query_field: &ast::QueryField,
    table: Option<&typecheck::Table>,
    indent_level: usize,
    source: Option<&str>,
) -> String {
    let mut result = to_query_field_spec_json(context, query_field, table, indent_level);
    if let Some(source_name) = source {
        let indent = "    ".repeat(indent_level);
        let marker = format!("Encode.object\n{}[ ", indent);
        let replacement = format!(
            "Encode.object\n{}[ ({}, Encode.string {})\n{}, ",
            indent,
            string::quote("@source"),
            string::quote(source_name),
            indent,
        );
        result = result.replacen(&marker, &replacement, 1);
    }
    result
}

fn to_query_shape_leaf_elm(field_name: &str, aliased_name: &str, indent_level: usize) -> String {
    if aliased_name == field_name {
        format!("({}, Encode.bool True)", string::quote(aliased_name))
    } else {
        let indent = "    ".repeat(indent_level + 1);
        format!(
            "({}, Encode.object\n{}[ ({}, Encode.string {})\n{}, ({}, Encode.bool True)\n{}])",
            string::quote(aliased_name),
            indent,
            string::quote("@source"),
            string::quote(field_name),
            indent,
            string::quote("@select"),
            indent,
        )
    }
}

/// Generate the applyDelta function using the lens-based approach
fn to_apply_delta_function_with_lenses(context: &typecheck::Context, query: &ast::Query) -> String {
    let mut result = String::new();

    // applyDelta function
    result.push_str("applyDelta : QueryDelta -> ReturnData -> Result String ReturnData\n");
    result.push_str("applyDelta delta data =\n");
    result.push_str("    case delta of\n");
    result.push_str("        Full _ newData ->\n");
    result.push_str("            Ok newData\n\n");
    result.push_str("        Delta _ ops ->\n");
    result.push_str("            Db.Delta.applyOps fields ops data\n\n\n");

    // Generate field lenses section header
    result.push_str("-- Field Lenses (generated)\n\n\n");

    // Generate the fields list for ReturnData
    result.push_str("fields : List ( String, Db.Delta.FieldHandler ReturnData )\n");
    result.push_str("fields =\n");

    let mut field_entries: Vec<String> = Vec::new();
    for field in &query.fields {
        if let ast::TopLevelQueryField::Field(query_field) = field {
            let field_name = ast::get_aliased_name(query_field);
            // Top-level query fields are always List, so use listField
            field_entries.push(format!(
                "( \"{}\", Db.Delta.listField {}Lens )",
                field_name, field_name
            ));
        }
    }

    if field_entries.is_empty() {
        result.push_str("    []\n\n\n");
    } else {
        result.push_str("    [ ");
        result.push_str(&field_entries.join("\n    , "));
        result.push_str("\n    ]\n\n\n");
    }

    // Generate lens definitions for each top-level field
    for field in &query.fields {
        if let ast::TopLevelQueryField::Field(query_field) = field {
            let field_name = ast::get_aliased_name(query_field);
            result.push_str(&generate_field_lens(
                context,
                query_field,
                "ReturnData",
                "",
                &field_name,
            ));
        }
    }

    result
}

/// Generate a lens definition for a field and its nested fields recursively
fn generate_field_lens(
    context: &typecheck::Context,
    query_field: &ast::QueryField,
    parent_type: &str,
    alias_stack: &str,
    lens_base: &str,
) -> String {
    let mut result = String::new();
    let field_name = ast::get_aliased_name(query_field);
    let capitalized = string::capitalize(&field_name);

    // Build the type name for this field
    let type_name = if alias_stack.is_empty() {
        capitalized.clone()
    } else {
        format!("{}_{}", alias_stack, capitalized)
    };

    // Top-level fields are always lists
    result.push_str(&format!(
        "{}Lens : Db.Delta.ListLens {} {}\n",
        lens_base, parent_type, type_name
    ));
    result.push_str(&format!("{}Lens =\n", lens_base));
    result.push_str(&format!("    {{ get = .{}\n", field_name));
    result.push_str(&format!(
        "    , set = \\list data -> {{ data | {} = list }}\n",
        field_name
    ));
    result.push_str(&format!("    , decode = decode{}\n", type_name));

    // Generate nested field lookup function
    let current_table = find_table_for_query_field(context, &query_field.name);
    let nested_fields =
        collect_nested_fields_with_type(context, current_table, query_field, &type_name);

    if nested_fields.is_empty() {
        result.push_str("    , nested = Db.Delta.noNested\n");
    } else {
        result.push_str(&format!("    , nested = {}NestedFields\n", lens_base));
    }
    result.push_str("    }\n\n\n");

    // Generate nested fields lookup function if there are nested fields
    if !nested_fields.is_empty() {
        result.push_str(&format!(
            "{}NestedFields : String -> Maybe (Db.Delta.FieldHandler {})\n",
            lens_base, type_name
        ));
        result.push_str(&format!("{}NestedFields name =\n", lens_base));
        result.push_str("    case name of\n");

        for (nested_name, _nested_type, is_optional) in &nested_fields {
            let lens_constructor = if *is_optional {
                "Db.Delta.maybeField"
            } else {
                "Db.Delta.listField"
            };
            let nested_lens_base = format!("{}{}", lens_base, string::capitalize(nested_name));
            result.push_str(&format!(
                "        \"{}\" ->\n            Just ({} {}Lens)\n\n",
                nested_name, lens_constructor, nested_lens_base
            ));
        }
        result.push_str("        _ ->\n            Nothing\n\n\n");

        // Recursively generate lenses for nested fields
        for (nested_name, nested_type, is_optional) in &nested_fields {
            if let Some(nested_query_field) = find_nested_query_field(query_field, nested_name) {
                let nested_lens_base = format!("{}{}", lens_base, string::capitalize(nested_name));
                let nested_table =
                    relationship_info_for_field(context, current_table, &nested_query_field.name).1;
                result.push_str(&generate_nested_field_lens(
                    context,
                    nested_query_field,
                    &type_name,
                    nested_type,
                    *is_optional,
                    &nested_lens_base,
                    nested_table,
                ));
            }
        }
    }

    result
}

/// Generate a lens for a nested field
fn generate_nested_field_lens(
    context: &typecheck::Context,
    query_field: &ast::QueryField,
    parent_type: &str,
    type_name: &str,
    is_optional: bool,
    lens_base: &str,
    current_table: Option<&typecheck::Table>,
) -> String {
    let mut result = String::new();
    let field_name = ast::get_aliased_name(query_field);

    if is_optional {
        // Maybe lens
        result.push_str(&format!(
            "{}Lens : Db.Delta.MaybeLens {} {}\n",
            lens_base, parent_type, type_name
        ));
        result.push_str(&format!("{}Lens =\n", lens_base));
        result.push_str(&format!("    {{ get = .{}\n", field_name));
        result.push_str(&format!(
            "    , set = \\val item -> {{ item | {} = val }}\n",
            field_name
        ));
    } else {
        // List lens
        result.push_str(&format!(
            "{}Lens : Db.Delta.ListLens {} {}\n",
            lens_base, parent_type, type_name
        ));
        result.push_str(&format!("{}Lens =\n", lens_base));
        result.push_str(&format!("    {{ get = .{}\n", field_name));
        result.push_str(&format!(
            "    , set = \\list item -> {{ item | {} = list }}\n",
            field_name
        ));
    }
    result.push_str(&format!("    , decode = decode{}\n", type_name));

    // Check for further nested fields
    let nested_fields =
        collect_nested_fields_with_type(context, current_table, query_field, type_name);

    if nested_fields.is_empty() {
        result.push_str("    , nested = Db.Delta.noNested\n");
    } else {
        result.push_str(&format!("    , nested = {}NestedFields\n", lens_base));
    }
    result.push_str("    }\n\n\n");

    // Generate nested fields lookup if needed
    if !nested_fields.is_empty() {
        result.push_str(&format!(
            "{}NestedFields : String -> Maybe (Db.Delta.FieldHandler {})\n",
            lens_base, type_name
        ));
        result.push_str(&format!("{}NestedFields name =\n", lens_base));
        result.push_str("    case name of\n");

        for (nested_name, _nested_nested_type, nested_is_optional) in &nested_fields {
            let lens_constructor = if *nested_is_optional {
                "Db.Delta.maybeField"
            } else {
                "Db.Delta.listField"
            };
            let nested_lens_base = format!("{}{}", lens_base, string::capitalize(nested_name));
            result.push_str(&format!(
                "        \"{}\" ->\n            Just ({} {}Lens)\n\n",
                nested_name, lens_constructor, nested_lens_base
            ));
        }
        result.push_str("        _ ->\n            Nothing\n\n\n");

        // Recursively generate lenses for deeply nested fields
        for (nested_name, nested_nested_type, nested_is_optional) in &nested_fields {
            if let Some(nested_query_field) = find_nested_query_field(query_field, nested_name) {
                let nested_lens_base = format!("{}{}", lens_base, string::capitalize(nested_name));
                let nested_table =
                    relationship_info_for_field(context, current_table, &nested_query_field.name).1;
                result.push_str(&generate_nested_field_lens(
                    context,
                    nested_query_field,
                    type_name,
                    nested_nested_type,
                    *nested_is_optional,
                    &nested_lens_base,
                    nested_table,
                ));
            }
        }
    }

    result
}

/// Collect nested fields with their type information (name, type_name, is_optional)
fn collect_nested_fields_with_type(
    context: &typecheck::Context,
    table: Option<&typecheck::Table>,
    query_field: &ast::QueryField,
    parent_type_name: &str,
) -> Vec<(String, String, bool)> {
    let mut nested = Vec::new();

    for arg_field in &query_field.fields {
        if let ast::ArgField::Field(nested_field) = arg_field {
            // Check if this nested field has its own fields (making it a relationship)
            let has_fields = nested_field
                .fields
                .iter()
                .any(|f| matches!(f, ast::ArgField::Field(_)));
            if has_fields {
                let nested_name = ast::get_aliased_name(nested_field);
                let nested_type = if parent_type_name.is_empty() {
                    string::capitalize(&nested_name)
                } else {
                    format!("{}_{}", parent_type_name, string::capitalize(&nested_name))
                };

                // Determine if this relationship is optional (Maybe) or an array (List)
                let is_optional = relationship_info_for_field(context, table, &nested_field.name).0;

                nested.push((nested_name, nested_type, is_optional));
            }
        }
    }
    nested
}

fn relationship_info_for_field<'a>(
    context: &'a typecheck::Context,
    table: Option<&'a typecheck::Table>,
    field_name: &str,
) -> (bool, Option<&'a typecheck::Table>) {
    let Some(table) = table else {
        return (false, None);
    };

    for f in &table.record.fields {
        if let ast::Field::FieldDirective(ast::FieldDirective::Link(link)) = f {
            if link.link_name == field_name {
                let primary_key_name = ast::get_primary_id_field_name(&table.record.fields);
                let is_one_to_many = link.local_ids.iter().all(|id| {
                    primary_key_name
                        .as_ref()
                        .map(|pk| id == pk)
                        .unwrap_or(false)
                });

                let linked_table = typecheck::get_linked_table(context, link);
                let linked_to_unique = if let Some(linked_table) = linked_table {
                    ast::linked_to_unique_field_with_record(link, &linked_table.record)
                } else {
                    ast::linked_to_unique_field(link)
                };

                return (!is_one_to_many && linked_to_unique, linked_table);
            }
        }
    }

    (false, None)
}

fn find_table_for_query_field<'a>(
    context: &'a typecheck::Context,
    query_field_name: &str,
) -> Option<&'a typecheck::Table> {
    if let Some(table) = context.tables.get(query_field_name) {
        return Some(table);
    }

    let decapitalized = string::decapitalize(query_field_name);
    if let Some(table) = context.tables.get(&decapitalized) {
        return Some(table);
    }

    if let Some(singular) = decapitalized.strip_suffix('s') {
        if let Some(table) = context.tables.get(singular) {
            return Some(table);
        }
    }

    None
}

/// Find a nested query field by name
fn find_nested_query_field<'a>(
    query_field: &'a ast::QueryField,
    name: &str,
) -> Option<&'a ast::QueryField> {
    for arg_field in &query_field.fields {
        if let ast::ArgField::Field(nested_field) = arg_field {
            if nested_field.name == name || ast::get_aliased_name(nested_field) == name {
                return Some(nested_field);
            }
        }
    }
    None
}

// PYRE.ELM MODULE GENERATION
//
// Generates the main QueryClient module that ties all queries together

fn generate_pyre_module(
    _context: &typecheck::Context,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    query_list: &ast::QueryList,
    query_names: &[String],
) -> String {
    let mut result = String::new();

    // Module declaration
    let mut database_namespaces: Vec<String> = query_names
        .iter()
        .filter_map(|name| all_query_info.get(name).map(|info| info.primary_db.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    database_namespaces.sort();

    result.push_str("module Pyre exposing (DatabaseId");
    for namespace in &database_namespaces {
        result.push_str(&format!(", {}", elm_database_namespace(namespace)));
    }
    result.push_str(", QueryId, Model, QueryModel, Query(..), Msg(..), Effect(..), init, update, decodeIncomingDelta, getResult)\n\n\n");

    // Imports
    result.push_str("import Dict exposing (Dict)\n");
    result.push_str("import Db.Database\n");
    result.push_str("import Json.Decode as Decode\n");
    result.push_str("import Json.Encode as Encode\n");

    // Import query modules
    for name in query_names {
        result.push_str(&format!("import Query.{}\n", name));
    }
    result.push_str("\n\n");

    // Model type
    result.push_str("-- Model\n\n\n");
    result.push_str("type alias Model =\n");
    result.push_str("    {");

    for (i, name) in query_names.iter().enumerate() {
        let field_name = string::decapitalize(name);
        if i == 0 {
            result.push_str(&format!(
                " {} : Dict String (QueryModel Query.{}.Input Query.{}.ReturnData)\n",
                field_name, name, name
            ));
        } else {
            result.push_str(&format!(
                "    , {} : Dict String (QueryModel Query.{}.Input Query.{}.ReturnData)\n",
                field_name, name, name
            ));
        }
    }

    result.push_str("    }\n\n\n");

    // QueryModel type
    result.push_str("type alias QueryModel input result =\n");
    result.push_str("    { input : input\n");
    result.push_str("    , result : result\n");
    result.push_str("    , revision : Int\n");
    result.push_str("    }\n\n\n");

    // Query type
    result.push_str("-- Query\n\n\n");
    result
        .push_str("type alias DatabaseId namespace =\n    Db.Database.DatabaseId namespace\n\n\n");
    for namespace in &database_namespaces {
        let name = elm_database_namespace(namespace);
        result.push_str(&format!(
            "type alias {} =\n    Db.Database.{}\n\n\n",
            name, name
        ));
    }
    result.push_str("type alias QueryId =\n    String\n\n\n");
    result.push_str("type Query\n");
    for (i, name) in query_names.iter().enumerate() {
        let prefix = if i == 0 { "    = " } else { "    | " };
        let database_type = all_query_info
            .get(name)
            .map(|info| elm_database_id_type(&info.primary_db))
            .unwrap_or_else(|| elm_database_id_type(ast::DEFAULT_SCHEMANAME));
        result.push_str(&format!(
            "{}{} {} QueryId Query.{}.Input\n",
            prefix, name, database_type, name
        ));
    }
    result.push_str("\n\n");

    // Msg type
    result.push_str("-- Msg\n\n\n");
    result.push_str("type Msg\n");
    result.push_str("    = QueryUpdate Query\n");
    for name in query_names {
        result.push_str(&format!(
            "    | {}_DataReceived QueryId Query.{}.QueryDelta\n",
            name, name
        ));
        let database_type = all_query_info
            .get(name)
            .map(|info| elm_database_id_type(&info.primary_db))
            .unwrap_or_else(|| elm_database_id_type(ast::DEFAULT_SCHEMANAME));
        result.push_str(&format!(
            "    | {}_Unregistered {} QueryId\n",
            name, database_type
        ));
    }
    result.push_str("\n\n");

    result.push_str("    | IncomingMsgDecodeFailed Decode.Error Decode.Value\n\n\n");

    // Effect type
    result.push_str("type Effect\n");
    result.push_str("    = NoEffect\n");
    result.push_str("    | Send Encode.Value\n");
    result.push_str("    | QueryUpdated QueryId\n");
    result.push_str("    | LogError Encode.Value\n\n\n");

    // Error type
    result.push_str("type Error\n");
    result.push_str("    = QueryDeltaApplyFailed String String\n");
    result.push_str("    | IncomingDeltaDecodeFailed Decode.Error Decode.Value\n\n\n");

    // Init
    result.push_str("-- Init\n\n\n");
    result.push_str("init : Model\n");
    result.push_str("init =\n");
    result.push_str("    {");

    for (i, name) in query_names.iter().enumerate() {
        let field_name = string::decapitalize(name);
        if i == 0 {
            result.push_str(&format!(" {} = Dict.empty\n", field_name));
        } else {
            result.push_str(&format!("    , {} = Dict.empty\n", field_name));
        }
    }

    result.push_str("    }\n\n\n");

    // Update
    result.push_str("-- Update\n\n\n");
    result.push_str("update : Msg -> Model -> ( Model, Effect )\n");
    result.push_str("update msg model =\n");
    result.push_str("    case msg of\n");
    result.push_str("        QueryUpdate query ->\n");
    result.push_str("            updateQuery query model\n\n");

    for name in query_names {
        let field_name = string::decapitalize(name);

        // DataReceived
        result.push_str(&format!("        {}_DataReceived queryId delta ->\n", name));
        result.push_str(&format!(
            "            case Dict.get queryId model.{} of\n",
            field_name
        ));
        result.push_str("                Just queryModel ->\n");
        result.push_str(&format!(
            "                    case Query.{}.applyDelta delta queryModel.result of\n",
            name
        ));
        result.push_str("                        Ok newResult ->\n");
        result.push_str("                            let\n");
        result.push_str("                                newRevision =\n");
        result.push_str("                                    case delta of\n");
        result.push_str(&format!(
            "                                        Query.{}.Full rev _ ->\n",
            name
        ));
        result.push_str("                                            rev\n\n");
        result.push_str(&format!(
            "                                        Query.{}.Delta rev _ ->\n",
            name
        ));
        result.push_str("                                            rev\n");
        result.push_str("                            in\n");
        result.push_str(&format!(
            "                            ( {{ model | {} = Dict.insert queryId {{ queryModel | result = newResult, revision = newRevision }} model.{} }}\n",
            field_name, field_name
        ));
        result.push_str("                            , QueryUpdated queryId\n");
        result.push_str("                            )\n\n");
        result.push_str("                        Err errMsg ->\n");
        result.push_str("                            ( model\n");
        result.push_str("                            , LogError (encodeError (QueryDeltaApplyFailed queryId errMsg))\n");
        result.push_str("                            )\n\n");
        result.push_str("                Nothing ->\n");
        result.push_str("                    ( model, NoEffect )\n\n");

        // Unregistered
        result.push_str(&format!(
            "        {}_Unregistered databaseId queryId ->\n",
            name
        ));
        result.push_str(&format!(
            "            ( {{ model | {} = Dict.remove queryId model.{} }}\n",
            field_name, field_name
        ));
        result.push_str("            , Send (encodeUnregister databaseId queryId)\n");
        result.push_str("            )\n\n");
    }

    result.push_str("        IncomingMsgDecodeFailed decodeErr json ->\n");
    result.push_str("            ( model\n");
    result.push_str(
        "            , LogError (encodeError (IncomingDeltaDecodeFailed decodeErr json))\n",
    );
    result.push_str("            )\n\n");

    result.push_str("\n");

    result.push_str("decodeIncomingDelta : Decode.Value -> Msg\n");
    result.push_str("decodeIncomingDelta json =\n");
    result.push_str("    case Decode.decodeValue incomingDeltaDecoder json of\n");
    result.push_str("        Ok msg ->\n");
    result.push_str("            msg\n\n");
    result.push_str("        Err decodeErr ->\n");
    result.push_str("            IncomingMsgDecodeFailed decodeErr json\n\n\n");

    result.push_str("incomingDeltaDecoder : Decode.Decoder Msg\n");
    result.push_str("incomingDeltaDecoder =\n");
    result.push_str("    Decode.map2 Tuple.pair\n");
    result.push_str("        (Decode.field \"queryName\" Decode.string)\n");
    result.push_str("        (Decode.field \"queryId\" Decode.string)\n");
    result.push_str("        |> Decode.andThen\n");
    result.push_str("            (\\( source, queryId ) ->\n");
    result.push_str("                case source of\n");

    for name in query_names {
        result.push_str(&format!("                    \"{}\" ->\n", name));
        result.push_str(&format!(
            "                        Decode.map ({}_DataReceived queryId) Query.{}.decodeQueryDelta\n\n",
            name, name
        ));
    }

    result.push_str("                    _ ->\n");
    result.push_str("                        Decode.fail (\"Unknown query source: \" ++ source)\n");
    result.push_str("            )\n\n\n");

    // getResult helper
    result.push_str("-- Helpers\n\n\n");
    result.push_str("updateQuery : Query -> Model -> ( Model, Effect )\n");
    result.push_str("updateQuery query model =\n");
    result.push_str("    case query of\n");

    for name in query_names {
        let field_name = string::decapitalize(name);
        let empty_return_data = query_list
            .queries
            .iter()
            .find_map(|q| match q {
                ast::QueryDef::Query(query) if query.name == *name => {
                    Some(generate_empty_return_data(query))
                }
                _ => None,
            })
            .unwrap_or_else(|| "ReturnData".to_string());

        result.push_str(&format!("        {} databaseId queryId input ->\n", name));
        result.push_str(&format!(
            "            case Dict.get queryId model.{} of\n",
            field_name
        ));
        result.push_str("                Just queryModel ->\n");
        result.push_str(&format!(
            "                    ( {{ model | {} = Dict.insert queryId {{ queryModel | input = input }} model.{} }}\n",
            field_name, field_name
        ));
        result.push_str(&format!(
            "                    , Send (encodeUpdateInput databaseId queryId Query.{}.queryShape (Query.{}.encode input))\n",
            name, name
        ));
        result.push_str("                    )\n\n");
        result.push_str("                Nothing ->\n");
        result.push_str("                    let\n");
        result.push_str("                        queryModel =\n");
        result.push_str(&format!(
            "                            {{ input = input, result = Query.{}.{}, revision = 0 }}\n",
            name, empty_return_data
        ));
        result.push_str("                    in\n");
        result.push_str(&format!(
            "                    ( {{ model | {} = Dict.insert queryId queryModel model.{} }}\n",
            field_name, field_name
        ));
        result.push_str(&format!(
            "                    , Send (encodeRegister databaseId \"{}\" Query.{}.queryShape queryId (Query.{}.encode input))\n",
            name, name, name
        ));
        result.push_str("                    )\n\n");
    }

    result.push_str(
        "getResult : QueryId -> Dict QueryId (QueryModel input result) -> Maybe result\n",
    );
    result.push_str("getResult queryId queries =\n");
    result.push_str("    Dict.get queryId queries\n");
    result.push_str("        |> Maybe.map .result\n\n\n");

    // Encoders
    result.push_str("-- Encoders\n\n\n");

    result.push_str(
        "encodeRegister : DatabaseId namespace -> String -> Encode.Value -> QueryId -> Encode.Value -> Encode.Value\n",
    );
    result.push_str("encodeRegister databaseId queryName queryShape queryId input =\n");
    result.push_str("    Encode.object\n");
    result.push_str("        [ ( \"type\", Encode.string \"register\" )\n");
    result.push_str("        , ( \"databaseId\", Db.Database.encode databaseId )\n");
    result.push_str("        , ( \"queryName\", Encode.string queryName )\n");
    result.push_str("        , ( \"querySource\", queryShape )\n");
    result.push_str("        , ( \"queryId\", Encode.string queryId )\n");
    result.push_str("        , ( \"queryInput\", input )\n");
    result.push_str("        ]\n\n\n");

    result.push_str(
        "encodeUpdateInput : DatabaseId namespace -> QueryId -> Encode.Value -> Encode.Value -> Encode.Value\n",
    );
    result.push_str("encodeUpdateInput databaseId queryId queryShape input =\n");
    result.push_str("    Encode.object\n");
    result.push_str("        [ ( \"type\", Encode.string \"update-input\" )\n");
    result.push_str("        , ( \"databaseId\", Db.Database.encode databaseId )\n");
    result.push_str("        , ( \"queryId\", Encode.string queryId )\n");
    result.push_str("        , ( \"querySource\", queryShape )\n");
    result.push_str("        , ( \"queryInput\", input )\n");
    result.push_str("        ]\n\n\n");

    result.push_str("encodeUnregister : DatabaseId namespace -> QueryId -> Encode.Value\n");
    result.push_str("encodeUnregister databaseId queryId =\n");
    result.push_str("    Encode.object\n");
    result.push_str("        [ ( \"type\", Encode.string \"unregister\" )\n");
    result.push_str("        , ( \"databaseId\", Db.Database.encode databaseId )\n");
    result.push_str("        , ( \"queryId\", Encode.string queryId )\n");
    result.push_str("        ]\n\n\n");

    result.push_str("encodeError : Error -> Encode.Value\n");
    result.push_str("encodeError error =\n");
    result.push_str("    case error of\n");
    result.push_str("        QueryDeltaApplyFailed queryId message ->\n");
    result.push_str("            Encode.object\n");
    result.push_str("                [ ( \"tag\", Encode.string \"query_delta_apply_failed\" )\n");
    result.push_str("                , ( \"queryId\", Encode.string queryId )\n");
    result.push_str("                , ( \"message\", Encode.string message )\n");
    result.push_str("                ]\n\n");
    result.push_str("        IncomingDeltaDecodeFailed decodeErr json ->\n");
    result.push_str("            Encode.object\n");
    result
        .push_str("                [ ( \"tag\", Encode.string \"incoming_msg_decode_failed\" )\n");
    result.push_str(
        "                , ( \"message\", Encode.string (Decode.errorToString decodeErr) )\n",
    );
    result.push_str("                , ( \"value\", json )\n");
    result.push_str("                ]\n");

    result
}
