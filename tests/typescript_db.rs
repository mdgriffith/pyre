use pyre::ast;
use pyre::filesystem::GeneratedFile;
use pyre::generate::server::typescript;
use pyre::generate::typescript::core;
use pyre::parser;
use pyre::typecheck;
use std::path::Path;

fn path_ends_with(path: &Path, suffix: &str) -> bool {
    path.ends_with(Path::new(suffix))
}

#[test]
fn typescript_schema_and_decoders_render_typed_json_containers() {
    let schema_source = r#"
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

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let schema_ts = typescript::schema(&database);

    let query_source = r#"
insert CreateEvent($payload: Json<Lifecycle>, $tags: Json<List<String>>, $counts: Json<Dict<Int>>) {
    event {
        payload = $payload
        tags = $tags
        counts = $counts
    }
}
"#;

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

    let metadata = files
        .iter()
        .find(|f| path_ends_with(&f.path, "queries/metadata/createEvent.ts"))
        .expect("generated metadata file");

    let content = &metadata.contents;

    assert!(
        schema_ts.contains("\"payload\": Lifecycle;")
            && schema_ts.contains("\"tags\": Array<string>;")
            && schema_ts.contains("\"counts\": Record<string, number>;")
            && schema_ts.contains("\"_type\": \"Finished\";"),
        "Expected typed Json fields to surface as rich TypeScript types. Generated schema:\n{}",
        schema_ts
    );

    assert!(
        content.contains("payload: Decode.Lifecycle")
            && content.contains("tags: z.array(z.string())")
            && content.contains("counts: z.record(z.number())")
            && content.contains("json_input_args: [\"payload\", \"tags\", \"counts\"]"),
        "Expected typed Json fields to use recursive TypeScript query validators. Generated metadata:\n{}",
        content
    );
}

#[test]
fn typescript_decoders_render_recursive_typed_json_with_lazy_validators() {
    let schema_source = r#"
type Attribute
   = AttributeInt {
        value Int
     }
   | AttributeBool {
        value Bool
     }
   | AttributeCustom {
        variant String
        fields  Dict<Attribute>
     }

type DocumentVisibility
   = DocumentVisibleToEveryone
   | DocumentHidden
   | DocumentVisibleToSelectedUsers { userIds Json<List<String>> }

record Entity {
    @public
    id Int @id
    attrs Json<Dict<Attribute>>
}

record Document {
    @public
    id Int @id
    visibility DocumentVisibility
}

"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let decode_ts = pyre::generate::server::typescript::to_schema_decoders(&database);

    assert!(
        decode_ts.contains("fields: z.record(z.lazy(() => Attribute)).optional()"),
        "Expected recursive custom type field to use z.lazy. Generated:\n{}",
        decode_ts
    );
    assert!(
        decode_ts.contains("userIds: z.array(z.string()).optional()"),
        "Expected Json<List<String>> variant field to validate as a string array. Generated:\n{}",
        decode_ts
    );
}

#[test]
fn typescript_session_validator_uses_custom_type_decoder() {
    let schema_source = r#"
type Role
    = Admin
    | Member

session {
    role Role
}

record User {
    @public
    id Id.Int @id
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let env = typescript::to_env(&context, &database).expect("env should generate");

    assert!(env.contains("import * as Db from './decode';"));
    assert!(env.contains("role: Db.Role"));
    assert!(!env.contains("role: z.any()"));
}
