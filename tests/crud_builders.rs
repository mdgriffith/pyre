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
    assignee Account.id?
}
record Account {
    @public
    id Id.Uuid @id
    name String
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
import Db.Edit.Account as Account
import Db.Edit.Internal as Internal
import Db.Id
import Db.Database
import Json.Encode as Encode
import Platform
import Query.DocumentDelete
port output : Encode.Value -> Cmd msg
main =
    Platform.worker
        { init = \() -> ( (), output (Encode.list identity [ Internal.encode create, Internal.encode (Document.update (Db.Id.uuid "01900000-0000-7000-8000-000000000000") patch), receiptCheck ]) )
        , update = \() model -> ( model, Cmd.none )
        , subscriptions = \_ -> Sub.none
        }
create = Document.create { title = "Title", owner = "Owner", tags = [] } [ Document.withSummary Nothing ]
patch = [ Document.title "New title", Document.summary Nothing, Document.tags [ "tag" ] ]
receiptCheck =
    let
        database = Db.Database.fromString "tenant:1"
        wire = Encode.object
            [ ( "databaseId", Encode.string "tenant:1" )
            , ( "requestId", Encode.string "request:1" )
            , ( "result", Encode.object
                [ ( "ok", Encode.bool True )
                , ( "value", Encode.list identity
                    [ Encode.object
                        [ ( "index", Encode.int 0 )
                        , ( "queryId", Encode.string Query.DocumentDelete.id )
                        , ( "result", Encode.object [ ( "document", Encode.list identity [ Encode.object [ ( "id", Encode.string "01900000-0000-7000-8000-000000000000" ) ] ] ) ] )
                        ]
                    ] )
                ] )
            ]
    in
    case Db.Edit.receive database "request:1" wire of
        Err _ -> Encode.bool False
        Ok receipt ->
            case ( Document.deleteResult 0 receipt, Document.createResult 0 receipt, Db.Edit.receive (Db.Database.fromString "tenant:2") "request:1" wire ) of
                ( Ok deleted, Err _, Err _ ) -> Encode.bool (List.length deleted.document == 1)
                _ -> Encode.bool False
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
        "Document.update (Db.Id.uuid \"01900000-0000-7000-8000-000000000000\") [ Account.name \"wrong record\" ]",
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
import { documentCreate, documentUpdate, documentId, accountId, batch, database } from './typescript/core/edits';
import { captureOperations } from '@pyre/client/operations';
import type { PyreClient } from '@pyre/client';
import { meta as createMeta } from './typescript/core/queries/metadata/documentCreate';
import { meta as updateMeta } from './typescript/core/queries/metadata/documentUpdate';
import { meta as deleteMeta } from './typescript/core/queries/metadata/documentDelete';
if (createMeta.optimistic.kind !== 'create' || deleteMeta.optimistic.kind !== 'delete') throw new Error('Missing CRUD prediction');
const created = documentCreate({ title: 'Title', owner: 'Owner', tags: [], summary: null });
const id = documentId('01900000-0000-7000-8000-000000000000');
const account = accountId('01900000-0000-7000-8000-000000000001');
documentUpdate(id, { assignee: account });
// @ts-expect-error Another record's ID cannot target this record
documentUpdate(account, {});
// @ts-expect-error Foreign keys retain their target record identity
documentUpdate(id, { assignee: id });
const edits = batch([created, documentUpdate(id, { summary: null })]);
const captured = captureOperations(edits);
if (captured.length !== 2) throw new Error('Missing operations');
if (!createMeta.InputValidator.safeParse(captured[0].input).success) throw new Error('Create input rejected');
if (updateMeta.InputValidator.safeParse({ id: '01900000-0000-7000-8000-000000000000', owner: 'forged' }).success) throw new Error('Protected input accepted');
// @ts-expect-error ID is allocated by the runtime
documentCreate({ id: 'forged', title: 'Title', owner: 'Owner', tags: [] });
// @ts-expect-error Immutable fields are not patchable
documentUpdate(id, { owner: 'Other' });
// @ts-expect-error Non-nullable values cannot be cleared
documentUpdate(id, { title: null });
// @ts-expect-error Required create fields cannot be omitted
documentCreate({ title: 'Title' });
// @ts-expect-error Unbranded strings are not record identities
documentUpdate('01900000-0000-7000-8000-000000000000', {});
async function typedSubmission(client: PyreClient) {
  const result = await client.submit(database('_default', 'tenant:1'), edits);
  if (result.ok) documentUpdate(result.value[0].result.document[0].id, { title: 'Typed result' });
  // @ts-expect-error Namespace is checked at the submission boundary
  client.submit(database('Other', 'tenant:1'), edits);
  // @ts-expect-error Generated operations require namespace evidence
  client.submit('tenant:1', edits);
}
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
assert.equal(edits[0].optimistic.kind, 'create');
assert.deepEqual(edits[1].input, { id: '01900000-0000-7000-8000-000000000000', title: 'New title', summary: null, tags: ['tag'] });
assert(!('owner' in edits[1].input));
assert.equal(edits[2], true, 'typed receipt must reject another database and wrong operation accessor');
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
