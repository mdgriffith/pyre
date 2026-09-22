use pyre::{ast, generate, generated_queries, parser, typecheck};
use std::process::Command;

#[test]
fn elm_edit_builders_disambiguate_prelude_names() {
    let names = [
        "identity",
        "always",
        "never",
        "not",
        "xor",
        "compare",
        "min",
        "max",
        "clamp",
        "toFloat",
        "round",
        "floor",
        "ceiling",
        "truncate",
        "modBy",
        "remainderBy",
        "negate",
        "abs",
        "sqrt",
        "logBase",
        "e",
        "pi",
        "cos",
        "sin",
        "tan",
        "acos",
        "asin",
        "atan",
        "atan2",
        "degrees",
        "radians",
        "turns",
        "toPolar",
        "fromPolar",
        "isNaN",
        "isInfinite",
        "identity_",
    ];
    let fields = names
        .iter()
        .map(|name| format!("    {name} String\n"))
        .collect::<String>();
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        &format!("record PreludeNames {{\n    @public\n    id Id.Uuid @id\n{fields}}}\n"),
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
    let module =
        std::fs::read_to_string(dir.path().join("client/elm/Db/Edit/PreludeNames.elm")).unwrap();
    for name in names {
        assert!(module.contains(&format!("\n{name}_ : (String) -> Patch")));
        // Only the Elm function name changes, including when a suffixed schema
        // name itself collides with a previously allocated builder name.
        assert!(module.contains(&format!("Patch ( \"{name}\", Encode.string value )")));
    }
    let metadata = std::fs::read_to_string(
        dir.path()
            .join("typescript/core/queries/metadata/preludeNamesUpdate.ts"),
    )
    .unwrap();
    assert!(metadata.contains("\"identity\""));
    assert!(!metadata.contains("identity__"));
    std::fs::write(dir.path().join("elm.json"), r#"{
      "type": "application", "source-directories": ["client/elm"], "elm-version": "0.19.1",
      "dependencies": {"direct": {"elm/core": "1.0.5", "elm/json": "1.1.3", "elm/time": "1.0.0"}, "indirect": {}},
      "test-dependencies": {"direct": {}, "indirect": {}}
    }"#).unwrap();
    let output = Command::new("elm")
        .args([
            "make",
            "client/elm/Db/Edit/PreludeNames.elm",
            "--output=/dev/null",
        ])
        .current_dir(dir.path())
        .output()
        .expect("elm must be on PATH");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn query_only_identity_builders_match_schema() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
@syncable(false)
record Note {
    @public
    key String @id
    id String
    title String
}
record Counter {
    @public
    key Int @id
    title String
}
record Account {
    @public
    key Id.Int @id
    title String
}
record Document {
    @public
    key Id.Uuid @id
    title String
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
    std::fs::write(dir.path().join("verify.ts"), r#"
import assert from 'node:assert/strict';
import { Note, Counter, Account, Document, noteId, counterId, accountId, documentId, type NoteId } from './typescript/core/edits';
import { captureOperations, type Operation } from '@pyre/client/operations';
import { meta as create } from './typescript/core/queries/metadata/noteCreate';
import { meta as update } from './typescript/core/queries/metadata/noteUpdate';
import { meta as remove } from './typescript/core/queries/metadata/noteDelete';
import { meta as counterUpdate } from './typescript/core/queries/metadata/counterUpdate';
import { meta as accountUpdate } from './typescript/core/queries/metadata/accountUpdate';
import { meta as documentUpdate } from './typescript/core/queries/metadata/documentUpdate';
const key = noteId('arbitrary-string-key');
const edits = [Note.create({key, id: 'ordinary', title: 'First'}), Note.update(key, {id: 'other', title: 'Next'}), Note.delete(key)];
for (const [i, validator] of [create, update, remove].entries()) assert(validator.InputValidator.safeParse(captureOperations([edits[i]])[0].input).success);
assert.deepEqual(captureOperations([edits[1]])[0].input, {key: 'arbitrary-string-key', id: 'other', title: 'Next'});
for (const [edit, validator] of [
    [Counter.update(counterId(42), {title: 'Int'}), counterUpdate],
    [Account.update(accountId(43), {title: 'Id.Int'}), accountUpdate],
    [Document.update(documentId('01900000-0000-7000-8000-000000000001'), {title: 'UUID'}), documentUpdate],
] as const) assert(validator.InputValidator.safeParse(captureOperations([edit])[0].input).success);
type ResultOf<T> = T extends Operation<any, infer R> ? R : never;
function typedResult(result: ResultOf<ReturnType<typeof Note.create>>) {
    const id: NoteId = result.note[0].key;
    Note.update(id, {id: result.note[0].id});
}
if (false) {
    // @ts-expect-error String keys require strings
    noteId(1);
    // @ts-expect-error Integer keys require numbers
    counterId('1');
    // @ts-expect-error UUID keys require strings
    documentId(1);
    // @ts-expect-error Create primary keys retain their brand
    Note.create({key: 'raw', id: 'ordinary', title: 'Wrong'});
    // @ts-expect-error Cannot use another record's identity
    Note.delete(documentId('01900000-0000-7000-8000-000000000001'));
}
assert.throws(() => noteId(1 as any));
assert.throws(() => counterId(1.5));
assert.throws(() => accountId(Number.MAX_SAFE_INTEGER + 1));
assert.throws(() => documentId('not-a-uuid'));
"#).unwrap();
    for (command, args) in [
        (
            "node_modules/.bin/tsc",
            vec![
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
            ],
        ),
        ("bun", vec!["verify.ts"]),
    ] {
        let executable = if command.contains('/') {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(command)
        } else {
            command.into()
        };
        let output = Command::new(executable)
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn generated_builders_compile() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
session {
    write Bool
}
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
record Note {
    @allow(query) { True }
    @allow(insert, update, delete) { Session.write == True }
    noteKey Id.Uuid @id
    id String
    title String
}
"#,
        &mut schema,
    )
    .unwrap();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();
    let mut queries = parser::parse_query(
        "queries.pyre",
        "query Notes { note { @where { id == \"ordinary\" } noteKey id title } }",
    )
    .unwrap();
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
import Db.Edit.Note as Note
import Db.Edit.Internal as Internal
import Db.Id
import Db.Database
import Json.Encode as Encode
import Platform
import Query.DocumentDelete
port output : Encode.Value -> Cmd msg
main =
    Platform.worker
        { init = \() -> ( (), output (Encode.list identity [ Internal.encode create, Internal.encode (Document.update (Db.Id.uuid "01900000-0000-7000-8000-000000000000") patch), receiptCheck, Internal.encode (Note.create { id = "ordinary", title = "Custom key" } []) ]) )
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
    std::fs::write(
        dir.path().join("ComposedExample.elm"),
        include_str!("fixtures/ComposedExample.elm"),
    )
    .unwrap();
    let example = Command::new("elm")
        .args(["make", "ComposedExample.elm", "--output=example.js"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        example.status.success(),
        "{}",
        String::from_utf8_lossy(&example.stderr)
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
import { Note } from './typescript/core/edits';
const custom: any = captureOperations([Note.create({ id: 'ordinary', title: 'Custom key' })])[0];
if (custom.optimistic.where.field !== 'noteKey' || custom.input.id !== 'ordinary' || !custom.input.noteKey) throw new Error('Custom create identity');
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
assert.equal(edits[3].createId, 'noteKey');
assert.equal(edits[3].optimistic.where.field, 'noteKey');
assert.deepEqual(edits[3].input, { id: 'ordinary', title: 'Custom key' });
"#).unwrap();
    std::fs::write(
        dir.path().join("composed-browser.ts"),
        include_str!("fixtures/composed-browser.ts"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("composed-conformance.ts"),
        include_str!("fixtures/composed-conformance.ts"),
    )
    .unwrap();
    for script in ["verify.ts", "verify-elm.ts", "composed-conformance.ts"] {
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
