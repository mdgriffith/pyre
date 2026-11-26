use pyre::ast;
use pyre::filesystem::GeneratedFile;
use pyre::generate::client::elm;
use pyre::parser;
use pyre::typecheck;
use std::path::Path;

fn path_ends_with(path: &Path, suffix: &str) -> bool {
    path.ends_with(Path::new(suffix))
}

#[test]
fn db_elm_tagged_record_variant_ignores_comments_when_formatting_first_field() {
    let schema_source = r#"
type TileFormat
   = Png
   | Webp

type Tiling = Tiling {
    // Storage prefix for tiles; full tile key is {tileRootKey}/{z}/{x}/{y}.{format}
    tileRootKey       String
    tileWidth         Int
    tileHeight        Int
    highestTileLevel  Int
    format            TileFormat
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let generated = elm::write_schema(&database);

    assert!(
        generated.contains("type Tiling\n    = Tiling\n      { tileRootKey : String"),
        "Expected first field in variant record without leading comma. Generated:\n{}",
        generated
    );
    assert!(
        !generated.contains("{       , tileRootKey"),
        "Generated Db.elm should not contain malformed leading comma. Generated:\n{}",
        generated
    );
}

#[test]
fn db_elm_json_alias_and_json_codecs_are_generated() {
    let schema_source = r#"
record GameAsset {
    @public
    id    Id.Int @id
    attrs Json
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let db_module = elm::write_schema(&database);
    let decode_module = elm::to_schema_decoders(&database);
    let encode_module = elm::to_schema_encoders(&database);

    assert!(db_module.contains("import Json.Encode"));
    assert!(db_module.contains("type alias Json =\n    Json.Encode.Value"));

    assert!(decode_module.contains("json : Decode.Decoder Json"));
    assert!(decode_module.contains("json =\n    Decode.value"));

    assert!(encode_module.contains("json : Db.Json -> Encode.Value"));
    assert!(encode_module.contains("json value =\n    value"));
}

#[test]
fn db_elm_uses_unqualified_local_types_within_db_module() {
    let schema_source = r#"
type GridType
   = Square

type TileFormat
   = Png

type Grid = Grid {
    gridType GridType
}

type Tiling = Tiling {
    format TileFormat
}

type MembershipRole
   = Player {
        controlled Json
     }
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let generated = elm::write_schema(&database);

    assert!(generated.contains("controlled : Json"));
    assert!(generated.contains("gridType : GridType"));
    assert!(generated.contains("format : TileFormat"));
    assert!(!generated.contains("Db.Json"));
    assert!(!generated.contains("Db.GridType"));
    assert!(!generated.contains("Db.TileFormat"));
}

#[test]
fn elm_generates_rich_types_for_typed_json_fields_and_inputs() {
    let schema_source = r#"
@syncable(false)
type Lifecycle
   = Running
   | Finished {
        reason String
     }

record Event {
    @public
    id       Id.Int @id
    payload  Json<Lifecycle>
    tags     Json<List<String>>
    counts   Json<Dict<Int>>
}
"#;

    let query_source = r#"
insert CreateEvent($payload: Json<Lifecycle>, $tags: Json<List<String>>, $counts: Json<Dict<Int>>) {
    event {
        payload = $payload
        tags = $tags
        counts = $counts
        id
    }
}

query GetEvents {
    event {
        id
        payload
        tags
        counts
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");

    let db_module = elm::write_schema(&database);
    let decode_module = elm::to_schema_decoders(&database);
    let encode_module = elm::to_schema_encoders(&database);

    assert!(
        db_module.contains("import Dict exposing (Dict)")
            && db_module.contains("type Lifecycle")
            && db_module.contains("Finished\n      { reason : String"),
        "Expected Db.elm to expose the typed Json payload union. Generated:\n{}",
        db_module
    );

    assert!(
        decode_module.contains("Decode.field \"_type\" Decode.string")
            && !decode_module.contains("Decode.field \"type_\"")
            && !decode_module.contains("Decode.field \"type\" Decode.string"),
        "Expected Db.Decode.elm to decode only _type unions. Generated:\n{}",
        decode_module
    );

    assert!(
        encode_module.contains("( \"_type\", Encode.string \"Finished\" )")
            && !encode_module.contains("( \"type_\", Encode.string")
            && !encode_module.contains("( \"type\", Encode.string"),
        "Expected Db.Encode.elm to encode typed Json containers and _type unions. Generated:\n{}",
        encode_module
    );

    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let query_info = typecheck::check_queries(&query_list, &context).expect("query typechecks");

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("client/elm"),
        &mut files,
    );

    let query_module = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Query/CreateEvent.elm"))
        .expect("generated CreateEvent.elm file");

    let content = &query_module.contents;

    let get_events_module = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Query/GetEvents.elm"))
        .expect("generated GetEvents.elm file");

    let get_events_content = &get_events_module.contents;

    assert!(
        content.contains("import Dict exposing (Dict)")
            && content.contains("type alias Input =\n    { payload : Db.Lifecycle\n    , tags : List String\n    , counts : Dict String Int")
            && content.contains("Db.Encode.lifecycle input.payload")
            && content.contains("(Encode.list Encode.string) input.tags")
            && content.contains("(Encode.dict identity Encode.int) input.counts"),
        "Expected CreateEvent.elm to use rich typed Json inputs and encoders. Generated:\n{}",
        content
    );

    assert!(
        get_events_content.contains("(Decode.list Decode.string)")
            && get_events_content.contains("(Decode.dict Decode.int)"),
        "Expected GetEvents.elm to decode typed Json containers. Generated:\n{}",
        get_events_content
    );
}

#[test]
fn elm_wraps_dict_decoders_for_typed_json_fields() {
    let schema_source = r#"
@syncable(false)
type Attribute
   = AttributeInt {
        value Int
     }

record Entity {
    @public
    id    Id.Int @id
    attrs Json<Dict<Attribute>>
}
"#;

    let query_source = r#"
query GetEntities {
    entity {
        id
        attrs
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let query_info = typecheck::check_queries(&query_list, &context).expect("query typechecks");

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("client/elm"),
        &mut files,
    );

    let query_module = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Query/GetEntities.elm"))
        .expect("generated GetEntities.elm file");

    let content = &query_module.contents;

    assert!(
        content.contains("|> Db.Decode.andField \"attrs\" (Decode.dict Db.Decode.attribute)"),
        "Expected generated dict decoder to be fully applied before andField. Generated:\n{}",
        content
    );
    assert!(
        !content.contains("|> Db.Decode.andField \"attrs\" Decode.dict (Db.Decode.attribute)"),
        "Generated malformed dict decoder application:\n{}",
        content
    );
}

#[test]
fn elm_keeps_recursive_historical_reads_and_uses_writable_shapes_everywhere_else() {
    let source = r#"
@syncable(false)
type Tree = Leaf { json Json } @deprecated | Branch { next List<Forest?> } | Stump @retired
type Forest = Grove { trees Dict<Tree> }
type Former = Gone @retired
record Event {
    @public
    id Id.Int @id
    tree Json<Tree>
    forest Json<Forest>?
    former Json<Former>?
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let mut queries = parser::parse_query(
        "queries.pyre",
        r#"
update ReplaceTree($tree: Json<Tree>) { event { tree = $tree } }
query ReadEvents { event { id tree forest former } }
"#,
    )
    .unwrap();
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files = Vec::new();
    let root = Path::new("client/elm");
    elm::generate(root, &database, &mut files);
    elm::generate_queries(&context, &info, &queries, root, &mut files);
    let source = |path| {
        files
            .iter()
            .find(|f| f.path == root.join(path))
            .unwrap()
            .contents
            .as_str()
    };

    let writable = source("Db/Write.elm");
    assert!(
        writable.contains("Branch { next : List (Maybe Forest) }"),
        "{writable}"
    );
    assert!(writable.contains("Grove { trees : Dict String Tree }"));
    assert!(writable.contains("type alias Former =\n    Basics.Never"));
    assert!(writable.contains("Leaf { json : Db.Json }"));
    assert!(writable.contains("(Decode.dict decodeTree)"));
    assert!(writable.contains("(traverseDict fromTree payload.trees)"));
    assert!(!writable.contains("Stump") && !writable.contains("Gone"));
    assert!(source("Db/Decode.elm").contains("Decode.succeed Db.Stump"));
    assert!(source("Query/ReadEvents.elm").contains("tree : Db.Tree"));
    let query = source("Query/ReplaceTree.elm");
    assert!(query.contains("{ tree : Db.Write.Tree"));
    assert!(query.contains("Db.Write.encodeTree input.tree"));
    assert!(source("Db/Edit/Event.elm").contains("tree : Db.Write.Tree"));
    assert!(source("Db/Edit/Event/Draft.elm").contains("forest : Db.Nullable (Db.Write.Forest)"));
    let input = source("Db/Editor/Input/Default.elm");
    assert!(input.contains("TreeValue (Db.Write.Tree)"));
    assert!(input.contains("Decode.map TreeValue Db.Write.decodeTree"));
    assert!(input.contains("Editor.ListKind (Editor.MaybeKind (Editor.UnionKind ForestUnion))"));
    assert!(input.contains("name = \"Leaf\""));
    assert!(!input.contains("Stump"));
    let editor = source("Db/Edit/Event/Editor.elm");
    assert!(editor.contains("choices = [ \"Leaf\", \"Branch\" ]"));
    assert!(editor.contains(
        "Internal.baseline (Db.Write.fromTree >> Maybe.map Input.TreeValue) Db.Encode.tree"
    ));
}
