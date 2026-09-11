use pyre::{ast, filesystem::GeneratedFile, generate, parser, typecheck};
use std::path::Path;
use std::process::Command;

fn fixture() -> (
    ast::Database,
    typecheck::Context,
    Vec<GeneratedFile<String>>,
) {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
type References = References {
    personKey Person.personKey
    auditKey Audit.auditKey?
}
record Person {
    @public
    personKey Id.Uuid @id
    name String
}
record Audit {
    @public
    auditKey Id.Int @id
    message String
}
record Note {
    @public
    id Id.Int @id
    personKey Person.personKey
    auditKey Audit.auditKey?
    title String
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let mut database = ast::Database {
        schemas: vec![schema],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let mut files = Vec::new();
    generate::generate_schema(&context, &database, &mut files);
    let mut queries = parser::parse_query(
        "query.pyre",
        r#"
query Notes {
    note {
        id
        personKey
        auditKey
    }
}
"#,
    )
    .expect("query parses");
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = typecheck::check_queries(&queries, &context).expect("query typechecks");
    generate::write_queries(&context, &queries, &info, &mut files);
    (database, context, files)
}

fn content<'a>(files: &'a [GeneratedFile<String>], suffix: &str) -> &'a str {
    &files
        .iter()
        .find(|file| file.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("missing {suffix}"))
        .contents
}

#[test]
fn typed_non_id_references_match_primary_key_kind() {
    let (_, _, files) = fixture();
    let metadata = content(&files, "schema.ts");
    assert!(
        metadata.contains("primaryKey: { name: \"personKey\", kind: \"uuid\" }"),
        "{metadata}"
    );
    assert!(
        metadata.contains("primaryKey: { name: \"auditKey\", kind: \"int\" }"),
        "{metadata}"
    );
    let ids = content(&files, "Db/Id.elm");
    assert!(ids.contains("type alias Person = Uuid PersonId"));
    assert!(ids.contains("type alias Audit = Integer AuditId"));
    assert!(ids.contains("uuid : String -> Uuid guard"));
    assert!(!ids.contains("Cmd") && !ids.contains("Random"));
    let db = content(&files, "client/elm/Db.elm");
    assert!(db.contains("personKey : Db.Id.Person"), "{db}");
    assert!(db.contains("auditKey : Maybe Db.Id.Audit"), "{db}");
    let query = content(&files, "Query/Notes.elm");
    for expected in [
        "personKey : Db.Id.Person",
        "auditKey : Maybe Db.Id.Audit",
        "Db.Id.decodeUuid",
        "Db.Id.decodeInt",
    ] {
        assert!(query.contains(expected), "missing {expected}:\n{query}");
    }
    let create = content(&files, "Query/NoteCreate.elm");
    assert!(
        create.contains("Db.Id.encodeUuid") && create.contains("Db.Id.encodeInt"),
        "{create}"
    );
    let stream = content(&files, "Db/Table/Notes.elm");
    assert!(
        stream.contains("personKeyIn : List Db.Id.Person"),
        "{stream}"
    );
    assert!(stream.contains("auditKeyIn : List Db.Id.Audit"), "{stream}");
    assert!(stream.contains("Db.Id.encodeUuid") && stream.contains("Db.Id.encodeInt"));
    for (module, record, key, kind) in [
        ("People", "Person", "personKey", "Uuid"),
        ("Audits", "Audit", "auditKey", "Int"),
    ] {
        let table = content(&files, &format!("Db/Table/{module}.elm"));
        for expected in [
            format!("{key} : Db.Id.{record}"),
            format!("{key}In : List Db.Id.{record}"),
            format!("Db.Decode.andField \"{key}\" Db.Id.decode{kind}"),
            format!("StreamInternal.addCondition \"{key}\" (Encode.object [ ( \"$in\", Encode.list Db.Id.encode{kind} values ) ])"),
        ] {
            assert!(table.contains(&expected), "missing {expected}:\n{table}");
        }
        assert!(!table.contains("idIn :"), "{table}");
        let stream = content(&files, "Db/Stream.elm");
        assert!(
            stream.contains(&format!("{record}Row {module}.Row")),
            "{stream}"
        );
        assert!(
            stream.contains(&format!("decodeRow {record}Row {module}.decodeRow row")),
            "{stream}"
        );
    }
}

#[test]
fn generated_elm_compiles_with_uuid_allocation_before_construction() {
    let elm = std::env::var("ELM_BINARY").unwrap_or_else(|_| "elm".into());
    if Command::new(&elm).arg("--version").output().is_err() {
        eprintln!("Skipping Elm compilation: install elm or set ELM_BINARY");
        return;
    }
    let (_, _, files) = fixture();
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    for file in &files {
        if let Ok(relative) = file.path.strip_prefix("client/elm") {
            let path = dir.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &file.contents).unwrap();
        }
    }
    std::fs::write(dir.path().join("elm.json"), r#"{
        "type": "application", "source-directories": ["."], "elm-version": "0.19.1",
        "dependencies": {"direct": {"elm/core": "1.0.5", "elm/json": "1.1.3", "elm/time": "1.0.0"}, "indirect": {}},
        "test-dependencies": {"direct": {}, "indirect": {}}
    }"#).unwrap();
    std::fs::write(
        dir.path().join("IdentityCheck.elm"),
        r#"
port module IdentityCheck exposing (main)

import Db
import Db.Decode
import Db.Encode
import Db.Id
import Db.Stream
import Db.Table.Notes
import Db.Table.People as People
import Db.Table.Audits as Audits
import Json.Decode as Decode
import Json.Encode as Encode
import Query.Notes
import Query.NoteCreate
import Query.PersonCreate
import Query.AuditCreate

port requestUuid : () -> Cmd msg
port uuidAllocated : (String -> msg) -> Sub msg

type Msg = Allocated String

personRowId : People.Row -> Db.Id.Person
personRowId row =
    row.personKey

auditRowId : Audits.Row -> Db.Id.Audit
auditRowId row =
    row.auditKey

personIdDecoder : Decode.Decoder Db.Id.Person
personIdDecoder =
    Decode.map personRowId People.decodeRow

auditIdDecoder : Decode.Decoder Db.Id.Audit
auditIdDecoder =
    Decode.map auditRowId Audits.decodeRow

subscriptions : Db.Id.Person -> Db.Id.Audit -> List Db.Stream.EntitySubscription
subscriptions personKey auditKey =
    [ People.stream |> People.personKeyIn [ personKey ] |> Db.Stream.person
    , Audits.stream |> Audits.auditKeyIn [ auditKey ] |> Db.Stream.audit
    ]

changeId : Db.Stream.EntityChange -> Encode.Value
changeId change =
    case change of
        Db.Stream.PersonRow row ->
            Db.Id.encodeUuid (personRowId row)

        Db.Stream.AuditRow row ->
            Db.Id.encodeInt (auditRowId row)

        _ ->
            Encode.null

type alias Model =
    { references : Db.References
    , createInput : Query.PersonCreate.Input
    }

main : Program () (Maybe Model) Msg
main =
    Platform.worker
        { init = \_ -> ( Nothing, requestUuid () )
        , subscriptions = \_ -> uuidAllocated Allocated
        , update = \(Allocated raw) _ ->
            ( Just
                { references = Db.References { personKey = Db.Id.uuid raw, auditKey = Just (Db.Id.int 7) }
                , createInput = { personKey = Db.Id.uuid raw, name = "New" }
                }
            , Cmd.none
            )
        }
"#,
    )
    .unwrap();
    let output = Command::new(&elm)
        .args(["make", "IdentityCheck.elm", "--output=/dev/null"])
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

#[test]
fn unsupported_primary_keys_are_not_labeled_as_uuids() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "record Slug {\n @public\n key String @id\n}\n",
        &mut schema,
    )
    .unwrap();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();
    let mut files = Vec::new();
    generate::generate_schema(&context, &database, &mut files);
    let metadata = content(&files, "schema.ts");
    assert!(
        metadata.contains("primaryKey: { name: \"key\", kind: \"unsupported\" }"),
        "{metadata}"
    );
}

#[test]
fn generated_typescript_ids_and_references_compile() {
    let (_, _, files) = fixture();
    let types = content(&files, "typescript/types.ts");
    assert!(types.contains("export type PersonId = string &"), "{types}");
    assert!(types.contains("export type AuditId = number &"), "{types}");
    assert!(types.contains("\"personKey\": PersonId;"), "{types}");
    assert!(types.contains("\"auditKey\": AuditId | null;"), "{types}");
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    std::fs::write(dir.path().join("types.ts"), types).unwrap();
    std::fs::write(
        dir.path().join("verify.ts"),
        r#"
import type { Person, PersonId, Audit, AuditId, Note, NoteId } from './types';
declare const person: Person;
declare const audit: Audit;
declare const note: Note;
const personId: PersonId = person.personKey;
const personRef: PersonId = note.personKey;
const uuid: string = personId;
const auditId: AuditId = audit.auditKey;
const integer: number = auditId;
const optionalRef: AuditId | null = note.auditKey;
// @ts-expect-error UUID identities are not integers
const wrongKind: number = personId;
// @ts-expect-error Integers are not UUID identities
const wrongUuid: PersonId = auditId;
// @ts-expect-error Different integer tables remain distinct
const wrongTable: NoteId = auditId;
"#,
    )
    .unwrap();
    let output = Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc"))
        .args([
            "--noEmit",
            "--strict",
            "--skipLibCheck",
            "types.ts",
            "verify.ts",
        ])
        .current_dir(dir.path())
        .output()
        .expect("tsc installed (bun install)");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
