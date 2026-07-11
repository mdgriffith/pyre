use pyre::ast;
use pyre::filesystem::GeneratedFile;
use pyre::generate::server::rust;
use pyre::generate::typescript::core;
use pyre::generated_queries;
use pyre::parser;
use pyre::typecheck;
use std::path::Path;

#[test]
fn generated_rust_server_file_exposes_query_ids_and_typed_boundaries() {
    let schema_source = r#"
record Game {
    @public

    id Id.Int @id
    name String
    description String?
}

"#;

    let query_source = r#"
query GetGame($id: Int, $name: String?, $description: String?) {
    game {
        @where { id == $id }

        id
        name
        description
    }
}

"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let mut query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let ast::QueryDef::Query(query) = &mut query_list.queries[0] else {
        panic!("expected parsed query");
    };
    query.args[2].omittable = true;

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    rust::generate_queries(&context, &query_list, Path::new("rust"), &mut files);

    let generated = files
        .iter()
        .find(|file| file.path == Path::new("rust/server.rs"))
        .expect("generated Rust server file");
    let content = &generated.contents;

    assert!(
        content.contains("pub mod query_ids") && content.contains("pub const GET_GAME: &str ="),
        "Expected generated query id constant. Generated:\n{}",
        content
    );
    assert!(
        content.contains("pub type GetGameInput = get_game::Input;")
            && content.contains("pub type GetGameOutput = get_game::Output;"),
        "Expected stable typed aliases. Generated:\n{}",
        content
    );
    assert!(
        content.contains("pub id: i64,")
            && content.contains("pub name: Option<String>,")
            && content.contains("pub description: OptionalField<String>,"),
        "Expected required, nullable, and omittable input fields. Generated:\n{}",
        content
    );
    assert!(
        content.contains("impl TryFrom<serde_json::Value> for Output")
            && content.contains("pub game: Vec<Game>,")
            && content.contains("pub description: Option<String>,"),
        "Expected typed output decoder shape. Generated:\n{}",
        content
    );
}

#[test]
fn generated_crud_uses_uuid_types_for_foreign_keys() {
    let schema_source = r#"
record ClocktowerGame {
    @public
    id Id.Uuid @id
}

record ClocktowerParticipant {
    @public
    id Id.Int @id
    gameId ClocktowerGame.id
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let mut query_list = ast::QueryList { queries: vec![] };
    generated_queries::append_generated_crud_queries(&mut query_list, &context);

    let mut rust_files: Vec<GeneratedFile<String>> = Vec::new();
    rust::generate_queries(&context, &query_list, Path::new("rust"), &mut rust_files);
    assert!(rust_files[0].contents.contains("pub game_id: String,"));

    let mut typescript_files: Vec<GeneratedFile<String>> = Vec::new();
    core::generate_queries(
        &context,
        &std::collections::HashMap::new(),
        &query_list,
        Path::new("typescript"),
        &mut typescript_files,
    );
    let metadata = typescript_files
        .iter()
        .find(|file| {
            file.path == Path::new("typescript/queries/metadata/clocktowerParticipantCreate.ts")
        })
        .expect("ClocktowerParticipant create metadata");
    assert!(metadata.contents.contains("gameId: z.string()"));
}

#[test]
fn generated_query_inputs_resolve_uuid_record_id_references() {
    let schema_source = r#"
record ClocktowerGame {
    @public
    id Id.Uuid @id
}
"#;
    let query_source = r#"
query ClocktowerGameKeystone($id: ClocktowerGame.id) {
    clocktowerGame {
        @where { id == $id }
        id
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

    let mut rust_files = Vec::new();
    rust::generate_queries(&context, &query_list, Path::new("rust"), &mut rust_files);
    assert!(rust_files[0].contents.contains("pub id: String,"));

    let mut typescript_files = Vec::new();
    core::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("typescript"),
        &mut typescript_files,
    );
    let metadata = typescript_files
        .iter()
        .find(|file| {
            file.path == Path::new("typescript/queries/metadata/clocktowerGameKeystone.ts")
        })
        .expect("ClocktowerGameKeystone metadata");
    assert!(metadata.contents.contains("id: z.string()"));

    let mut manifest_files = Vec::new();
    pyre::generate::manifest::generate_queries(
        &context,
        &query_list,
        &query_info,
        &mut manifest_files,
    );
    assert!(manifest_files[0].contents.contains("\"type\": \"Id.Uuid\""));
}

#[test]
fn generated_rust_inputs_use_typed_custom_unions() {
    let schema_source = r#"
type Status
    = Active
    | Inactive

type Delivery
    = Pickup
    | Courier { trackingCode String }

record Order {
    @public
    id Id.Int @id
    status Status
    delivery Delivery
    statuses Json<List<Status>>
    deliveries Json<Dict<Delivery>>
}
"#;
    let query_source = r#"
insert CreateOrder($status: Status, $delivery: Delivery, $statuses: Json<List<Status>>, $deliveries: Json<Dict<Delivery>>) {
    order {
        status = $status
        delivery = $delivery
        statuses = $statuses
        deliveries = $deliveries
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");

    let mut files = Vec::new();
    rust::generate_queries(&context, &query_list, Path::new("rust"), &mut files);
    let generated = &files[0].contents;

    assert!(generated.contains("pub enum Status"));
    assert!(generated.contains("pub enum Delivery"));
    assert!(generated.contains("#[serde(tag = \"_type\")]"));
    assert!(generated.contains("Courier {"));
    assert!(generated.contains("tracking_code: String,"));
    assert!(generated.contains("pub status: Status,"));
    assert!(generated.contains("pub delivery: Delivery,"));
    assert!(generated.contains("pub statuses: Vec<Status>,"));
    assert!(generated.contains("pub deliveries: std::collections::HashMap<String, Delivery>,"));
    assert!(!generated.contains("pub status: serde_json::Value,"));
    assert!(!generated.contains("pub delivery: serde_json::Value,"));
}

#[test]
fn generated_rust_datetime_accepts_epoch_or_text() {
    let schema_source = r#"
record Event {
    @public
    id Id.Int @id
    startsAt DateTime
}
"#;
    let query_source = r#"
insert CreateEvent($startsAt: DateTime) {
    event {
        startsAt = $startsAt
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let mut files = Vec::new();
    rust::generate_queries(&context, &query_list, Path::new("rust"), &mut files);
    let generated = &files[0].contents;

    assert!(generated.contains("pub enum DateTime"));
    assert!(generated.contains("UnixSeconds(i64)"));
    assert!(generated.contains("Text(String)"));
    assert!(generated.contains("pub starts_at: DateTime,"));
}
