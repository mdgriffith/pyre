use pyre::ast;
use pyre::filesystem::GeneratedFile;
use pyre::generate::typescript::core;
use pyre::parser;
use pyre::typecheck;
use std::path::Path;

fn path_ends_with(path: &Path, suffix: &str) -> bool {
    path.ends_with(Path::new(suffix))
}

#[test]
fn generated_typescript_query_shape_preserves_where_placeholders() {
    let schema_source = r#"
session {
    userId Int
}

record Rulebook {
    @public

    id Id.Int @id
    ownerId Int
    name String
}
"#;

    let query_source = r#"
query GetRulebookByName($name: String) {
    rulebook {
        @where { name == $name && ownerId == Session.userId }

        id
        name
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
    core::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("typescript/core"),
        &mut files,
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "queries/metadata/getRulebookByName.ts"))
        .expect("generated metadata file");

    let content = &generated.contents;

    assert!(
        content.contains("\"@where\": { \"$and\": [ { \"name\": { \"$var\": \"name\" } }, { \"ownerId\": { \"$session\": \"userId\" } } ] }")
            || content.contains("\"@where\": { \"$and\": [{ \"name\": { \"$var\": \"name\" } }, { \"ownerId\": { \"$session\": \"userId\" } }] }"),
        "TypeScript queryShape should preserve variable and session placeholders in @where. Generated:\n{}",
        content
    );
}

#[test]
fn generated_typescript_query_input_validates_typed_json_params_without_stringifying() {
    let schema_source = r#"
type Lifecycle
   = Running
   | Finished {
        reason String
     }

record Event {
    @public
    id Id.Int @id
    payload Json<Lifecycle>
}
"#;

    let query_source = r#"
insert SeedEvent($payload: Json<Lifecycle>) {
    event {
        payload = $payload
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
    core::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("typescript/core"),
        &mut files,
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "queries/metadata/seedEvent.ts"))
        .expect("generated metadata file");

    let content = &generated.contents;

    assert!(
        content.contains("payload: Decode.Lifecycle"),
        "Expected typed Json param to use generated union validator. Generated:\n{}",
        content
    );

    assert!(
        content.contains("const InputValidator = z.object({\n  payload: Decode.Lifecycle"),
        "Expected typed Json param to use its runtime decoder. Generated:\n{}",
        content
    );

    assert!(
        content.contains("json_input_args: [\"payload\"]"),
        "Expected typed Json param metadata to mark JSON inputs. Generated:\n{}",
        content
    );

    assert!(
        !content.contains("JSON.stringify(input.payload)"),
        "Did not expect typed Json param to be stringified. Generated:\n{}",
        content
    );
}

#[test]
fn generated_typescript_datetime_input_preserves_public_type_and_coerces_at_runtime() {
    let schema_source = r#"
record Event {
    @public
    id Id.Int @id
    startedAt DateTime
}
"#;
    let query_source = r#"
insert CreateEvent($startedAt: DateTime) {
    event { startedAt = $startedAt }
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
    core::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("typescript/core"),
        &mut files,
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "queries/metadata/createEvent.ts"))
        .expect("generated metadata file");
    let content = &generated.contents;

    assert!(content.contains("startedAt: z.union([z.date(), z.string(), z.number()])"));
    assert!(content.contains("startedAt: Decode.CoercedDate"));
    assert!(content.contains("export type Input = z.infer<typeof RawInputValidator>;"));
}
