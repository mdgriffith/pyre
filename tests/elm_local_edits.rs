use pyre::{ast, filesystem::GeneratedFile, generate::client::elm, parser, typecheck};
use std::{fs, path::Path, process::Command};

#[test]
fn generated_update_setters_cannot_collide_with_crud_functions() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "record Collision {\n @public\n id Id.Uuid @id\n update String?\n delete String?\n createWith String?\n}\n",
        &mut schema,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![schema],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = ast::QueryList { queries: vec![] };
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files = Vec::new();
    elm::generate(Path::new("src"), &database, &mut files);
    elm::generate_queries(&context, &info, &queries, Path::new("src"), &mut files);
    let module = files
        .iter()
        .find(|file| file.path.ends_with("Db/Default/Edit/Collision.elm"))
        .unwrap();
    for setter in ["setUpdate", "setDelete", "setCreateWith"] {
        assert!(module
            .contents
            .contains(&format!("{setter} : (Maybe (String)) -> Patch")));
    }
    assert!(module
        .contents
        .contains("withUpdate : (Maybe (String)) -> CreateOption"));
    assert!(module
        .contents
        .contains("withDelete : (Maybe (String)) -> CreateOption"));
    assert!(module
        .contents
        .contains("withCreateWith : (Maybe (String)) -> CreateOption"));
}

#[test]
fn query_only_namespaces_do_not_generate_elm_local_edit_modules() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "@syncable(false)\nrecord ReadOnly {\n @public\n id Id.Int @id\n name String\n}\n",
        &mut schema,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![schema],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = parser::parse_query(
        "queries.pyre",
        "query ReadOnlyRows { readOnly { id name } }\ninsert NamedWrite($name: String) { readOnly { name = $name id } }\n",
    )
    .unwrap();
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files = Vec::new();
    elm::generate_queries(&context, &info, &queries, Path::new("src"), &mut files);

    assert!(files
        .iter()
        .any(|file| file.path.ends_with("Query/ReadOnlyRows.elm")));
    assert!(files
        .iter()
        .any(|file| file.path.ends_with("Query/NamedWrite.elm")));
    assert!(!files
        .iter()
        .any(|file| file.path.to_string_lossy().contains("Db/Default/Edit/")));
}

#[test]
fn collision_safe_namespace_and_edit_id_names_compile() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir_in(root.join("target")).unwrap();
    let definitions = [
        ("_default", "DefaultThing"),
        ("Default", "NamedThing"),
        ("Foo", "BarBaz"),
        ("FooBar", "Baz"),
    ];
    let mut schemas = Vec::new();
    for (namespace, record) in definitions {
        let mut schema = ast::Schema {
            namespace: namespace.into(),
            ..ast::Schema::default()
        };
        parser::run(
            &format!("{namespace}/schema.pyre"),
            &format!("record {record} {{\n @public\n id Id.Uuid @id\n}}\n"),
            &mut schema,
        )
        .unwrap();
        schemas.push(schema);
    }
    let foo = schemas
        .iter_mut()
        .find(|schema| schema.namespace == "Foo")
        .unwrap();
    parser::run(
        "Foo/collisions.pyre",
        "record FooBar {\n @public\n id Id.Uuid @id\n fooBar String @default(\"camel\")\n foo_bar String @default(\"snake\")\n}\nrecord Foo_Bar {\n @public\n id Id.Uuid @id\n}\nrecord A {\n @public\n id Id.Uuid @id\n}\nrecord AIdentity {\n @public\n id Id.Uuid @id\n}\n",
        foo,
    )
    .unwrap();
    let mut database = ast::Database { schemas };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = ast::QueryList { queries: vec![] };
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files = Vec::new();
    elm::generate(Path::new("src"), &database, &mut files);
    elm::generate_queries(&context, &info, &queries, Path::new("src"), &mut files);

    let generated = |suffix: &str| {
        &files
            .iter()
            .find(|file| file.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("missing generated {suffix}"))
            .contents
    };
    let database_ids = generated("Db/Database.elm");
    assert!(database_ids.contains("type Default\n    = Default"));
    assert!(database_ids.contains("type DefaultNamespace\n    = DefaultNamespace"));
    assert!(files
        .iter()
        .any(|file| file.path.ends_with("Db/Default/Edit/DefaultThing.elm")));
    assert!(files.iter().any(|file| file
        .path
        .ends_with("Db/DefaultNamespace/Edit/NamedThing.elm")));
    let edit_ids = generated("Db/EditIds.elm");
    assert!(edit_ids.contains("type alias FooBarBaz ="));
    assert!(edit_ids.contains("type alias FooBarBazNamespace ="));
    assert!(edit_ids.contains("type alias FooA =\n    Db.Id.A"));
    assert!(edit_ids.contains("type alias FooAIdentity =\n    Db.Id.AIdentity"));
    assert!(files
        .iter()
        .any(|file| file.path.ends_with("Db/Foo/Edit/FooBar.elm")));
    assert!(files
        .iter()
        .any(|file| file.path.ends_with("Db/Foo/Edit/FooBarNamespace.elm")));
    let foo_bar = generated("Db/Foo/Edit/FooBar.elm");
    assert!(foo_bar.contains("setFooBar : (String) -> Patch"));
    assert!(foo_bar.contains("setFooBarNamespace : (String) -> Patch"));
    assert!(foo_bar.contains("withFooBar : (String) -> CreateOption"));
    assert!(foo_bar.contains("withFooBarNamespace : (String) -> CreateOption"));
    assert!(foo_bar.contains("Patch \"fooBar\""));
    assert!(foo_bar.contains("Patch \"foo_bar\""));
    assert!(foo_bar.contains("CreateOption \"fooBar\""));
    assert!(foo_bar.contains("CreateOption \"foo_bar\""));

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
        temp.path().join("src/Collision.elm"),
        r#"module Collision exposing (all)

import Db.Database
import Db.Default.Edit.DefaultThing as DefaultThing
import Db.DefaultNamespace.Edit.NamedThing as NamedThing
import Db.EditIds
import Db.Foo.Edit.BarBaz as BarBaz
import Db.Foo.Edit.FooBar as FooBar
import Db.Foo.Edit.FooBarNamespace as FooBarUnderscore
import Db.FooBar.Edit.Baz as Baz
import Db.Id

defaultDb : Db.Database.DatabaseId Db.Database.Default
defaultDb = Db.Database.fromString "default"

namedDb : Db.Database.DatabaseId Db.Database.DefaultNamespace
namedDb = Db.Database.fromString "named"

fooId : Db.EditIds.FooBarBaz
fooId = Db.Id.uuid "00000000-0000-4000-8000-000000000003"

fooBarId : Db.EditIds.FooBarBazNamespace
fooBarId = Db.Id.uuid "00000000-0000-4000-8000-000000000004"

aId : Db.EditIds.FooA
aId = Db.Id.uuid "00000000-0000-4000-8000-000000000005"

aIdentityId : Db.EditIds.FooAIdentity
aIdentityId = Db.Id.uuid "00000000-0000-4000-8000-000000000006"

all =
    { defaultThing = DefaultThing.delete (Db.Id.uuid "00000000-0000-4000-8000-000000000001")
    , namedThing = NamedThing.delete (Db.Id.uuid "00000000-0000-4000-8000-000000000002")
    , barBaz = BarBaz.delete fooId
    , baz = Baz.delete fooBarId
    , fooBar = FooBar.delete (Db.Id.uuid "00000000-0000-4000-8000-000000000007")
    , setters = FooBar.update (Db.Id.uuid "00000000-0000-4000-8000-000000000007") [ FooBar.setFooBar "camel", FooBar.setFooBarNamespace "snake" ]
    , options = [ FooBar.withFooBar "camel", FooBar.withFooBarNamespace "snake" ]
    , fooBarUnderscore = FooBarUnderscore.delete (Db.Id.uuid "00000000-0000-4000-8000-000000000008")
    , a = aId
    , aIdentity = aIdentityId
    }
"#,
    )
    .unwrap();
    let output = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "elm@0.19.1-6",
            "elm",
            "make",
            "src/Collision.elm",
            "--output=/dev/null",
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
}

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
        include_str!("fixtures/elm-local-edits/archive.pyre"),
        &mut archive,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![schema, archive],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = parser::parse_query("commands.pyre", "insert NamedAudit($id: Audit.id, $message: String) { audit { id = $id message = $message updatedAt } }\nquery ReadIssues { issue { id title } }\nquery LocalEdits { issue { id } }\nquery QueryUpdate { issue { id } }").unwrap();
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
        "Issue.update (Db.Id.uuid uuid) [ Audit.setMessage \"bad\" ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.setTitle Nothing ]",
        "Issue.update (Db.Id.int 1) [ Issue.setTitle \"bad\" ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.setId (Db.Id.uuid uuid) ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.setOwner \"bad\" ]",
        "Issue.update (Db.Id.uuid uuid) [ Issue.setUpdatedAt Time.utc ]",
        "Audit.create { id = Db.Id.uuid uuid, message = \"bad\" }",
        "Batch.succeed Tuple.pair |> Batch.and (Issue.delete (Db.Id.uuid uuid)) |> Batch.and (Archive.delete (Db.Id.uuid uuid))",
        "Pyre.submit archive (Issue.delete (Db.Id.uuid uuid)) (Pyre.init \"bad\")",
        "Issue.delete (Db.Id.int 1)",
        "Audit.delete archiveId",
    ] {
        let source = format!("module Bad exposing (bad)\nimport Db.Default.Edit.Issue as Issue\nimport Db.Default.Edit.Audit as Audit\nimport Db.Archive.Edit.ArchiveEntry as Archive\nimport Db.Database\nimport Db.EditIds\nimport Pyre\nimport Pyre.Batch as Batch\nimport Time\nimport Db.Id\narchive : Db.Database.DatabaseId Db.Database.Archive\narchive = Db.Database.fromString \"archive\"\narchiveId : Db.EditIds.ArchiveArchiveEntry\narchiveId = Db.Id.uuid \"00000000-0000-4000-8000-000000000001\"\nuuid = \"00000000-0000-4000-8000-000000000001\"\nbad = {}\n", expression);
        fs::write(temp.path().join("src/Bad.elm"), source).unwrap();
        let output = Command::new("npx").args(["--yes", "--package", "elm@0.19.1-6", "elm", "make", "src/Bad.elm", "--output=/dev/null"]).current_dir(temp.path()).output().unwrap();
        assert!(!output.status.success(), "unexpected compile success: {expression}");
    }
}

#[test]
fn commands_with_attached_databases_are_not_elm_local_edits() {
    let mut app = ast::Schema {
        namespace: "App".into(),
        ..ast::Schema::default()
    };
    parser::run(
        "App/schema.pyre",
        "record Post {\n @public\n id Id.Uuid @id\n title String\n userId Auth.User.id\n user @link(userId, Auth.User.id)\n}\n",
        &mut app,
    )
    .unwrap();
    let mut auth = ast::Schema {
        namespace: "Auth".into(),
        ..ast::Schema::default()
    };
    parser::run(
        "Auth/schema.pyre",
        "record User {\n @public\n id Id.Uuid @id\n email String\n}\n",
        &mut auth,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![app, auth],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let queries = parser::parse_query(
        "queries.pyre",
        "update RenamePost($id: Post.id, $title: String) { post { @where { id == $id } title = $title } user { email } }",
    )
    .unwrap();
    let info = typecheck::check_queries(&queries, &context).unwrap();
    assert!(info["RenamePost"].attached_dbs.contains("Auth"));

    let mut files = Vec::new();
    elm::generate_queries(&context, &info, &queries, Path::new("src"), &mut files);

    assert!(files
        .iter()
        .any(|file| file.path.ends_with("Query/RenamePost.elm")));
    assert!(!files
        .iter()
        .any(|file| file.path.ends_with("Db/App/Edit/Command/RenamePost.elm")));
}

#[path = "helpers/local_edit_conformance.rs"]
mod conformance;
