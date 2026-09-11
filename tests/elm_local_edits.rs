use pyre::{ast, filesystem::GeneratedFile, generate::client::elm, parser, typecheck};
use std::{fs, path::Path, process::Command};

#[test]
fn generated_local_edits_compile_and_run() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir_in(root.join("target")).unwrap();
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        include_str!("fixtures/elm-local-edits/schema.pyre"),
        &mut schema,
    )
    .unwrap();
    let mut archive = ast::Schema {
        namespace: "Archive".into(),
        ..ast::Schema::default()
    };
    parser::run(
        "archive.pyre",
        "record ArchiveEntry {\n @public\n key Id.Int @id\n title String\n}\n",
        &mut archive,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![schema, archive],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = parser::parse_query("commands.pyre", "insert NamedAudit($message: String) { audit { message = $message id updatedAt } }\nquery ReadIssues { issue { id title } }").unwrap();
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files: Vec<GeneratedFile<String>> = vec![];
    elm::generate(Path::new("src"), &database, &mut files);
    elm::generate_queries(&context, &info, &queries, Path::new("src"), &mut files);
    pyre::generate::typescript::core::generate_schema(
        &context,
        &database,
        Path::new("typescript/core"),
        &mut files,
    );
    pyre::generate::typescript::core::generate_queries(
        &context,
        &info,
        &queries,
        Path::new("typescript/core"),
        &mut files,
    );
    for file in files {
        let path = temp.path().join(file.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, file.contents).unwrap();
    }
    fs::write(
        temp.path().join("elm.json"),
        include_str!("fixtures/elm-local-edits/elm.json"),
    )
    .unwrap();
    fs::write(
        temp.path().join("src/Test.elm"),
        include_str!("fixtures/elm-local-edits/Test.elm"),
    )
    .unwrap();
    let output = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "elm@0.19.1-6",
            "elm",
            "make",
            "src/Test.elm",
            "--output=test.js",
        ])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("node").args(["-e", "const app = require('./test.js').Elm.Test.init(); app.ports.output.subscribe(value => { if (!value) process.exitCode = 1; }); app.ports.effectOut.subscribe(value => console.log(JSON.stringify(value)));"]).current_dir(temp.path()).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(temp.path().join("effect.json"), &output.stdout).unwrap();
    let output = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "bun@latest",
            "bun",
            "test",
            "tests/fixtures/elm-local-edits/bridge.test.ts",
        ])
        .env("PYRE_ELM_EDIT_FIXTURE", temp.path())
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    conformance::run(root, temp.path());
    for expression in [
        "Issue.update (Db.Id.uuid uuid) [ Audit.message \"bad\" ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.title Nothing ]",
        "Issue.update (Db.Id.int 1) [ Issue.title \"bad\" ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.id (Db.Id.uuid uuid) ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.owner \"bad\" ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.updatedAt Time.utc ]",
        "Audit.create { id = Db.Id.int 1, message = \"bad\" }",
        "Batch.succeed Tuple.pair |> Batch.and (Issue.delete (Db.Id.uuid uuid)) |> Batch.and (Archive.delete (Db.Id.int 1))",
        "Pyre.submit archive (Issue.delete (Db.Id.uuid uuid)) Pyre.init",
        "Issue.delete (Db.Id.int 1)",
        "Audit.delete archiveId",
    ] {
        let source = format!("module Bad exposing (bad)\nimport Db.Default.Edit.Issue as Issue\nimport Db.Default.Edit.Audit as Audit\nimport Db.Archive.Edit.ArchiveEntry as Archive\nimport Db.Database\nimport Db.EditIds\nimport Pyre\nimport Pyre.Batch as Batch\nimport Time\nimport Db.Id\narchive : Db.Database.DatabaseId Db.Database.Archive\narchive = Db.Database.fromString \"archive\"\narchiveId : Db.EditIds.ArchiveArchiveEntry\narchiveId = Db.Id.int 1\nuuid = \"00000000-0000-4000-8000-000000000001\"\nbad = {}\n", expression);
        fs::write(temp.path().join("src/Bad.elm"), source).unwrap();
        let output = Command::new("npx").args(["--yes", "--package", "elm@0.19.1-6", "elm", "make", "src/Bad.elm", "--output=/dev/null"]).current_dir(temp.path()).output().unwrap();
        assert!(!output.status.success(), "unexpected compile success: {expression}");
    }
}

#[path = "helpers/local_edit_conformance.rs"]
mod conformance;
