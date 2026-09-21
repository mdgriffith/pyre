use pyre::{ast, generate, generated_queries, parser, typecheck};
use std::process::Command;

#[test]
fn generated_builders_compile() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
record Document {
    @public
    id Id.Uuid @id
    title String
    owner String @immutable
    summary String?
    tags Json<List<String>>
}
"#,
        &mut schema,
    )
    .unwrap();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = ast::QueryList { queries: vec![] };
    generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files = vec![];
    generate::generate_schema(&context, &database, &mut files);
    generate::write_queries(&context, &queries, &info, &mut files);
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    for file in files {
        let path = dir.path().join(file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file.contents).unwrap();
    }
    std::fs::write(dir.path().join("elm.json"), r#"{
      "type": "application", "source-directories": ["client/elm", "."], "elm-version": "0.19.1",
      "dependencies": {"direct": {"elm/core": "1.0.5", "elm/json": "1.1.3", "elm/time": "1.0.0"}, "indirect": {}},
      "test-dependencies": {"direct": {}, "indirect": {}}
    }"#).unwrap();
    std::fs::write(dir.path().join("Main.elm"), r#"port module Main exposing (main)
import Db.Edit
import Db.Edit.Document as Document
import Db.Edit.Internal as Internal
import Db.Id
import Json.Encode as Encode
import Platform
port output : Encode.Value -> Cmd msg
main =
    Platform.worker
        { init = \() -> ( (), output (Encode.list Internal.encode [ create, Document.update (Db.Id.uuid "01900000-0000-7000-8000-000000000000") patch ]) )
        , update = \() model -> ( model, Cmd.none )
        , subscriptions = \_ -> Sub.none
        }
create = Document.create { title = "Title", owner = "Owner", tags = [] } [ Document.withSummary Nothing ]
patch = [ Document.title "New title", Document.summary Nothing, Document.tags [ "tag" ] ]
"#).unwrap();
    let output = Command::new("elm")
        .args(["make", "Main.elm", "--output=elm.js"])
        .current_dir(dir.path())
        .output()
        .expect("elm must be on PATH");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let main = std::fs::read_to_string(dir.path().join("Main.elm")).unwrap();
    for invalid in [
        "Document.title Nothing",
        "Document.owner \"forbidden\"",
        "Document.create { title = \"missing\" } []",
    ] {
        std::fs::write(
            dir.path().join("Main.elm"),
            format!("{main}\nbad = {invalid}\n"),
        )
        .unwrap();
        let output = Command::new("elm")
            .args(["make", "Main.elm", "--output=/dev/null"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "Invalid builder unexpectedly compiled: {invalid}"
        );
    }
    std::fs::write(dir.path().join("verify.ts"), r#"
import { documentCreate, documentUpdate, batch } from './typescript/core/edits';
import { captureOperations } from '@pyre/client/operations';
import { meta as createMeta } from './typescript/core/queries/metadata/documentCreate';
import { meta as updateMeta } from './typescript/core/queries/metadata/documentUpdate';
const created = documentCreate({ title: 'Title', owner: 'Owner', tags: [], summary: null });
const edits = batch([created, documentUpdate('01900000-0000-7000-8000-000000000000', { summary: null })]);
const captured = captureOperations(edits);
if (captured.length !== 2) throw new Error('Missing operations');
if (!createMeta.InputValidator.safeParse(captured[0].input).success) throw new Error('Create input rejected');
if (updateMeta.InputValidator.safeParse({ id: '01900000-0000-7000-8000-000000000000', owner: 'forged' }).success) throw new Error('Protected input accepted');
// @ts-expect-error ID is allocated by the runtime
documentCreate({ id: 'forged', title: 'Title', owner: 'Owner', tags: [] });
// @ts-expect-error Immutable fields are not patchable
documentUpdate('id', { owner: 'Other' });
// @ts-expect-error Non-nullable values cannot be cleared
documentUpdate('id', { title: null });
// @ts-expect-error Required create fields cannot be omitted
documentCreate({ title: 'Title' });
"#).unwrap();
    let tsc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc");
    let output = Command::new(tsc)
        .args([
            "--noEmit",
            "--strict",
            "--skipLibCheck",
            "--target",
            "ES2022",
            "--module",
            "ESNext",
            "--moduleResolution",
            "Bundler",
            "verify.ts",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(dir.path().join("verify-elm.ts"), r#"
import assert from 'node:assert/strict';
import { Elm } from './elm.js';
const app = Elm.Main.init({ flags: null });
const edits: any[] = await new Promise(resolve => app.ports.output.subscribe(resolve));
assert.deepEqual(edits[0].input, { title: 'Title', owner: 'Owner', tags: [], summary: null });
assert.equal(edits[0].createId, 'id');
assert.deepEqual(edits[1].input, { id: '01900000-0000-7000-8000-000000000000', title: 'New title', summary: null, tags: ['tag'] });
assert(!('owner' in edits[1].input));
"#).unwrap();
    for script in ["verify.ts", "verify-elm.ts"] {
        let output = Command::new("bun")
            .arg(script)
            .current_dir(dir.path())
            .output()
            .expect("bun must be on PATH");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
