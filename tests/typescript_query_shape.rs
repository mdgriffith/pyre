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
fn both_client_generators_reject_only_authored_session_arguments() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
type Scope
    = Workspace { id Int }
    | Account { id Int }
type Root
    = Nested { scope Scope }
session {
    userId Int
    pageSize Int
    scope Scope
    root Root
}
record Note {
    id Id.Int @id
    ownerId Int
    score Float
    scope Scope
    root Root
    replies @link(Reply.noteId)
    @allow(*) { ownerId == Session.userId }
}
record Reply {
    id Id.Int @id
    noteId Note.id
    ownerId Int
    @public
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    for (source, rejected, mutation) in [
        ("query Test { note { @where { ownerId == Session.userId } id } }", true, false),
        ("query Test { note { @where { Session.userId == 1 } id } }", true, false),
        ("query Test { note { @where { Or(ownerId == 1, And(ownerId == 2, Session.scope.Workspace.id == 3)) } id } }", true, false),
        ("query Test { note { @where { ownerId == Session.scope.Workspace.id } id } }", true, false),
        ("query Test { note { @where { Session.root.Nested.scope.Workspace.id == 3 } id } }", true, false),
        ("query Test { note { id replies { @where { ownerId == Session.userId } id } } }", true, false),
        ("query Test { note { @limit(Session.pageSize) id } }", true, false),
        ("query Test { note { id replies { @limit(Session.pageSize) id } } }", true, false),
        ("query Test { note { @where { score == abs(abs(Session.userId)) } id } }", true, false),
        ("query Test { note { @where { scope == Workspace { id = Session.userId } } id } }", true, false),
        ("query Test { note { @where { root == Nested { scope = Workspace { id = Session.userId } } } id } }", true, false),
        ("query Test { note { id } }", false, false),
        ("query Test { note { @limit(5) id } }", false, false),
        ("query Test { note { id replies { @limit(2) id } } }", false, false),
        ("query Test($ownerId: Int) { note { @where { ownerId == $ownerId } id } }", false, false),
        ("update Test { note { @where { ownerId == Session.userId } ownerId = Session.userId id } }", false, true),
    ] {
        let queries = parser::parse_query("query.pyre", source)
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let info = typecheck::check_queries(&queries, &context)
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let mut files = Vec::new();
        pyre::generate::write_queries(&context, &queries, &info, &mut files);
        assert!(files.iter().any(|file| file.contents.contains("session_userId")),
            "server SQL must still bind Session: {source}");
        if source.contains("note { @limit(Session.pageSize)") {
            assert!(files.iter().any(|file| file.contents.contains("limit $session_pageSize")),
                "server SQL must still use the Session limit: {source}");
        }
        for suffix in ["queries/metadata/test.ts", "Query/Test.elm"] {
            let content = &files.iter().find(|file| path_ends_with(&file.path, suffix))
                .expect("generated query").contents;
            assert_eq!(content.contains("\"$error\""), rejected, "{source}: {content}");
            assert!(!content.contains("\"$session\""), "{source}: {content}");
            if rejected {
                assert!(content.contains(pyre::generate::local_query::SESSION_ERROR));
                assert!(!content.contains("\"@where\""));
                assert!(!content.contains("\"@limit\""));
            } else if !mutation {
                assert!(content.contains("queryShape"));
                if source.contains("$ownerId") {
                    assert!(content.contains("\"$var\""));
                }
            } else {
                assert!(!content.contains("queryShape"));
            }
            if suffix.ends_with(".ts") {
                assert!(content.contains("SessionValidator: Decode.SessionValidator"));
                assert!(content.contains("session_args:"));
                assert!(content.contains("\"userId\""));
                if source.contains("Session.pageSize") {
                    assert!(content.lines().any(|line|
                        line.contains("session_args:") && line.contains("\"pageSize\"")),
                        "server metadata must still bind the Session limit: {content}");
                }
            }
        }
    }
}

#[test]
fn generated_typescript_transaction_has_shared_input_and_step_results() {
    let schema_source = r#"
record Note {
    id Id.Int @id
    body String
    updatedAt Int
    @public
}
"#;
    let query_source = r#"
transaction ReplaceNote($id: Note.id, $body: String) {
    delete removed: note {
        @where { id == $id }
        id
    }
    insert created: note {
        body = $body
        updatedAt = 10
        id
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
        .find(|file| path_ends_with(&file.path, "queries/metadata/replaceNote.ts"))
        .expect("generated transaction metadata");

    assert!(generated.contents.contains("id: z.number()"));
    assert!(generated.contents.contains("body: z.string()"));
    assert!(generated.contents.contains("removed: Removed.array()"));
    assert!(generated.contents.contains("created: Created.array()"));
    assert!(generated
        .contents
        .contains("operation: \"transaction\" as const"));
}

#[test]
fn generated_typescript_query_shape_rejects_session_filters() {
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
        content.contains("\"$error\": \"Local queries cannot reference Session; use explicit inputs or execute on the server.\""),
        "TypeScript queryShape should reject explicit session filters. Generated:\n{}",
        content
    );
    assert!(content.contains("SessionValidator: Decode.SessionValidator"));
    assert!(content.contains("session_args:"));
    assert!(!content.contains("\"$session\""));
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

#[test]
fn generated_typescript_crud_omits_immutable_update_artifacts() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
record Document {
    @public
    id      Int @id
    ownerId Int @immutable
    title   String
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let mut query_list = ast::QueryList { queries: vec![] };
    pyre::generated_queries::append_generated_crud_queries(&mut query_list, &context);
    let query_info = typecheck::check_queries(&query_list, &context).expect("CRUD typechecks");
    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    core::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("typescript/core"),
        &mut files,
    );

    let file = |suffix: &str| {
        &files
            .iter()
            .find(|file| path_ends_with(&file.path, suffix))
            .unwrap_or_else(|| panic!("missing generated {suffix}"))
            .contents
    };
    let create_meta = file("queries/metadata/documentCreate.ts");
    let update_meta = file("queries/metadata/documentUpdate.ts");
    let update_sql = file("queries/sql/documentUpdate.ts");

    let create_inputs = create_meta
        .split("export type Input")
        .next()
        .expect("create input validators");
    let update_inputs = update_meta
        .split("export type Input")
        .next()
        .expect("update input validators");
    assert!(create_inputs.contains("ownerId: z.number()"));
    assert!(!update_inputs.contains("ownerId:"));
    assert!(update_meta.contains("ownerId: z.number()"));
    assert!(update_meta.contains("export type Result = z.infer<typeof ReturnData>;"));
    let optimistic = update_meta
        .lines()
        .find(|line| line.contains("optimistic:"))
        .expect("update optimistic metadata");
    assert!(optimistic.contains("{ field: \"title\", input: \"title\" }"));
    assert!(!optimistic.contains("ownerId"));
    let params = update_sql
        .lines()
        .filter(|line| line.contains("params:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(params.contains("\"title\"") && params.contains("\"title__is_set\""));
    assert!(!params.contains("ownerId"));
    assert!(!update_sql.contains("ownerId ="));
    assert!(!update_sql.contains("ownerId__is_set"));
}
