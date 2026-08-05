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
            && content.contains("counts: z.record(z.string(), z.number())")
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
        value    Int
        fallback Int?
     }
   | AttributeBool {
        value Bool
     }
   | AttributeTimestamp {
        value DateTime
     }
    | AttributeCustom {
         variant String
         fields  Dict<Attribute>
      }
   | AttributeSet {
        elementType String
        items       List<AttributeCustomValue>
     }
   | AttributeChoiceList {
        choiceType String
        items      List<AttributeCustomValue>
     }
   | AttributePool {
        keyType String
        entries List<AttributePoolEntry>
     }
   | AttributeNested {
        items List<Dict<AttributeCustomValue>>
     }

type AttributePoolEntry
   = AttributePoolEntry {
        key       String
        max       Int
        remaining Int
     }

type AttributeCustomValue
   = AttributeCustomValue {
        variant String
        fields Dict<Attribute>
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
        decode_ts.contains("fields: z.record(z.string(), z.lazy(() => Attribute)).optional()"),
        "Expected recursive custom type field to use z.lazy. Generated:\n{}",
        decode_ts
    );
    assert!(
        decode_ts.contains("export type Attribute =\n")
            && decode_ts.contains("type AttributeInput =\n")
            && decode_ts.contains("fields?: Record<string, unknown>")
            && decode_ts.contains("items?: Array<unknown>")
            && decode_ts.contains("entries?: Array<unknown>")
            && decode_ts.contains("fallback?: number | null")
            && decode_ts.contains("value?: boolean | number")
            && decode_ts.contains("value?: number | string | Date")
            && decode_ts.contains("items?: Array<Record<string, unknown>>")
            && decode_ts.contains("const AttributeDiscriminated: z.ZodType<Attribute, AttributeInput>")
            && decode_ts.contains("export const Attribute: z.ZodType<Attribute, unknown>")
            && decode_ts.contains("export type AttributeCustomValue =\n")
            && decode_ts.contains("type AttributeCustomValueInput =\n")
            && decode_ts.contains("const AttributeCustomValueDiscriminated: z.ZodType<AttributeCustomValue, AttributeCustomValueInput>")
            && decode_ts.contains("export const AttributeCustomValue: z.ZodType<AttributeCustomValue, unknown>")
            && decode_ts.contains("entries: z.array(z.lazy(() => AttributePoolEntry)).optional()"),
        "Expected recursive groups to separate decoder input and output types. Generated:\n{}",
        decode_ts
    );
    assert!(
        decode_ts.contains("userIds: z.array(z.string()).optional()"),
        "Expected Json<List<String>> variant field to validate as a string array. Generated:\n{}",
        decode_ts
    );

    let tsc = Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc");
    assert!(
        tsc.exists(),
        "TypeScript is required for generated decoder regression tests; run `bun install`"
    );
    let temp_dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("temporary directory");
    std::fs::write(temp_dir.path().join("decode.ts"), &decode_ts).expect("write generated decoder");
    std::fs::write(
        temp_dir.path().join("verify.ts"),
        include_str!("fixtures/typescript/recursive_preprocess_verify.ts.fixture"),
    )
    .expect("write recursive decoder verification");
    let output = std::process::Command::new(&tsc)
        .args([
            "--noEmit",
            "--strict",
            "--skipLibCheck",
            "--module",
            "preserve",
            "--moduleResolution",
            "bundler",
            "decode.ts",
            "verify.ts",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("typecheck generated recursive decoder");
    assert!(
        output.status.success(),
        "generated recursive decoder failed to compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let output = std::process::Command::new("bun")
        .args(["run", "verify.ts"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Bun is required for generated decoder regression tests; run `bun install`");
    assert!(
        output.status.success(),
        "generated recursive decoder failed at runtime\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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

#[test]
fn core_session_validator_composes_named_type_decoders() {
    let schema_source = r#"
type ParticipantStatus
    = Storyteller
    | Player

type CampaignRole
    = Owner { campaignId String }
    | Member { campaignId String }

session {
    clocktowerParticipantStatus ParticipantStatus?
    campaignRole                CampaignRole?
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
    let mut files = Vec::new();
    core::generate_schema(
        &context,
        &database,
        Path::new("typescript/core"),
        &mut files,
    );
    let decode = files
        .iter()
        .find(|file| path_ends_with(&file.path, "typescript/core/decode.ts"))
        .expect("generated decode file");

    assert!(decode.contents.contains(
        "clocktowerParticipantStatus?: ParticipantStatus | null;\n  campaignRole?: CampaignRole | null;"
    ));
    assert!(decode.contents.contains(
        "clocktowerParticipantStatus: ParticipantStatus.nullish(),\n  campaignRole: CampaignRole.nullish(),"
    ));
    assert!(!decode.contents.contains("z.any() /* ParticipantStatus */"));
    assert!(!decode.contents.contains("z.any() /* CampaignRole */"));

    let participant_validator = decode
        .contents
        .find("export const ParticipantStatus =")
        .expect("participant validator");
    let session_validator = decode
        .contents
        .find("export const SessionValidator =")
        .expect("session validator");
    assert!(participant_validator < session_validator);

    let temp_dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("temporary directory");
    std::fs::write(temp_dir.path().join("decode.ts"), &decode.contents)
        .expect("write generated decoder");
    std::fs::write(
        temp_dir.path().join("verify.ts"),
        r#"
import { SessionValidator, type Session } from "./decode.ts";

const validSession: Session = {
  clocktowerParticipantStatus: "Player",
  campaignRole: { _type: "Member", campaignId: "campaign-1" },
};
SessionValidator.parse(validSession);
const nullSession: Session = { clocktowerParticipantStatus: null };
SessionValidator.parse(nullSession);
SessionValidator.parse({});
// @ts-expect-error Invalid variants must not be part of the generated Session type.
const invalidSession: Session = { clocktowerParticipantStatus: "Observer" };

const scalar = SessionValidator.parse({
  clocktowerParticipantStatus: { _type: "Storyteller" },
});
const scalarSession: Session = scalar;
if (scalarSession.clocktowerParticipantStatus !== "Storyteller") {
  throw new Error(`Expected scalar normalization, got ${JSON.stringify(scalarSession)}`);
}

const constructed = SessionValidator.parse({
  campaignRole: { _type: "Owner", campaignId: "campaign-1" },
});
const constructedSession: Session = constructed;
if (constructedSession.campaignRole?._type !== "Owner") {
  throw new Error(`Expected constructed value, got ${JSON.stringify(constructedSession)}`);
}

const invalid = SessionValidator.safeParse({ clocktowerParticipantStatus: "Observer" });
if (invalid.success) {
  throw new Error("Expected invalid participant status to fail");
}
const path = invalid.error.issues[0]?.path;
if (JSON.stringify(path) !== JSON.stringify(["clocktowerParticipantStatus"])) {
  throw new Error(`Expected precise session field path, got ${JSON.stringify(path)}`);
}
"#,
    )
    .expect("write runtime verification");

    let tsc = Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc");
    assert!(
        tsc.exists(),
        "TypeScript is required for generated decoder regression tests; run `bun install`"
    );
    let output = std::process::Command::new(tsc)
        .args([
            "--noEmit",
            "--strict",
            "--skipLibCheck",
            "--module",
            "preserve",
            "--moduleResolution",
            "bundler",
            "--allowImportingTsExtensions",
            "verify.ts",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("typecheck generated session validator");
    assert!(
        output.status.success(),
        "generated session types failed to compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let output = std::process::Command::new("bun")
        .arg("run")
        .arg("verify.ts")
        .current_dir(temp_dir.path())
        .output()
        .expect("run generated session validator");
    assert!(
        output.status.success(),
        "generated session validator failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn typescript_session_references_preserve_id_storage_types() {
    let schema_source = r#"
session {
    uuidRecordId UuidRecord.id?
    intRecordId  IntRecord.id?
}

record UuidRecord {
    @public
    id Id.Uuid @id
}

record IntRecord {
    @public
    id Id.Int @id
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    schema.namespace = "Records".to_string();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let mut files = Vec::new();
    core::generate_schema(
        &context,
        &database,
        Path::new("typescript/core"),
        &mut files,
    );

    let decode = files
        .iter()
        .find(|file| path_ends_with(&file.path, "typescript/core/decode.ts"))
        .expect("generated decode file");
    assert!(decode.contents.contains("uuidRecordId?: string | null;"));
    assert!(decode.contents.contains("intRecordId?: number | null;"));
    assert!(decode
        .contents
        .contains("uuidRecordId: z.string().nullish(),"));
    assert!(decode
        .contents
        .contains("intRecordId: z.number().nullish(),"));

    let env = typescript::to_env(&context, &database).expect("env should generate");
    assert!(env.contains("uuidRecordId: z.string().optional(),"));
    assert!(env.contains("intRecordId: z.number().optional(),"));

    let temp_dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("temporary directory");
    std::fs::write(temp_dir.path().join("decode.ts"), &decode.contents)
        .expect("write generated decoder");
    std::fs::write(
        temp_dir.path().join("verify.ts"),
        r#"
import { SessionValidator } from "./decode.ts";

SessionValidator.parse({ uuidRecordId: "550e8400-e29b-41d4-a716-446655440000" });
SessionValidator.parse({ intRecordId: 42 });

if (SessionValidator.safeParse({ uuidRecordId: 42 }).success) {
  throw new Error("Expected numeric UUID record ID to fail");
}
"#,
    )
    .expect("write runtime verification");

    let output = std::process::Command::new("bun")
        .arg("run")
        .arg("verify.ts")
        .current_dir(temp_dir.path())
        .output()
        .expect("run generated session validator");
    assert!(
        output.status.success(),
        "generated session validator failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn typescript_metadata_serializes_payload_union_parameters_but_not_unit_enums() {
    let schema_source = r#"
type Content
   = Folder
   | Markdown {
        body     String
        metadata Json<Dict<String>>
     }

type Visibility
   = Hidden
   | Public

record Node {
    @public
    id         Int @id
    content    Content
    visibility Visibility
}
"#;
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");
    let query_list = parser::parse_query(
        "query.pyre",
        r#"
insert CreateNode($content: Content, $visibility: Visibility) {
    node {
        content = $content
        visibility = $visibility
    }
}
"#,
    )
    .expect("query parses");
    let query_info = typecheck::check_queries(&query_list, &context).expect("query typechecks");
    let mut files = Vec::new();

    core::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("typescript/core"),
        &mut files,
    );

    let metadata = files
        .iter()
        .find(|file| path_ends_with(&file.path, "queries/metadata/createNode.ts"))
        .expect("generated metadata file");
    assert!(metadata.contents.contains("json_input_args: [\"content\"]"));
}
