use pyre::{ast, generate, parser, typecheck};
use std::path::Path;
use std::process::Command;

fn fixture(source: &str) -> (ast::Database, typecheck::Context) {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema should parse");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    (database, context)
}

fn generated_state_files() -> Vec<pyre::filesystem::GeneratedFile<String>> {
    let (database, context) = fixture(
        r#"type Node
   = Text { value String }
   | Children { items List<Node?> }

session {
    userId Int
}

state Connection {
    userId Int = Session.userId
    cursor Json<Dict<List<Node?>>>?
    label String @default("online")
}

state Shared {
    count Int @default(0)
    started DateTime @default(now)
    root Node @default(Text { value = "root" })
    selection Dict<List<Node?>>?
}
"#,
    );
    let mut files = Vec::new();
    generate::generate_schema(&context, &database, &mut files);
    files
}

#[test]
fn state_generation_is_conditional_and_does_not_change_elm_output() {
    let (database, context) = fixture(
        r#"@syncable(false)
record Note {
    @public
    id Id.Int @id
    body String
}
"#,
    );
    let mut files = Vec::new();
    generate::generate_schema(&context, &database, &mut files);
    assert!(!files.iter().any(|file| file.path.ends_with("state.ts")));
    assert!(!files.iter().any(|file| file.path.ends_with("state.rs")));

    let state_files = generated_state_files();
    assert!(state_files
        .iter()
        .any(|file| file.path == Path::new("typescript/core/state.ts")));
    assert!(state_files
        .iter()
        .any(|file| file.path == Path::new("rust/state.rs")));
    assert!(state_files
        .iter()
        .filter(|file| file.path.starts_with("client/elm"))
        .all(|file| !file.contents.contains("ConnectionPatch")
            && !file.contents.contains("SharedPatch")));
}

#[test]
fn generated_typescript_state_surface_enforces_complete_values_and_writable_patches() {
    let files = generated_state_files();
    let state = files
        .iter()
        .find(|file| file.path == Path::new("typescript/core/state.ts"))
        .expect("generated TypeScript state module");
    assert!(state.contents.contains("userId: number;"));
    assert!(state
        .contents
        .contains("cursor?: Record<string, Array<Node | null>> | null;"));
    assert!(!state
        .contents
        .split("export interface ConnectionPatch")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap()
        .contains("userId"));
    assert!(state.contents.contains(
        "Connection: { writableFields: [\"cursor\", \"label\"], derivedFields: [\"userId\"] }"
    ));

    let temp_dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    std::fs::write(temp_dir.path().join("state.ts"), &state.contents).unwrap();
    std::fs::write(
        temp_dir.path().join("verify.ts"),
        r#"import type { Connection, ConnectionPatch, Shared, SharedPatch } from "./state";

const connection: Connection = {
  userId: 7,
  cursor: { left: [{ _type: "Text", value: "complete" }, null] },
  label: "online",
};
const emptyConnectionPatch: ConnectionPatch = {};
const clearCursor: ConnectionPatch = { cursor: null };
const replaceCursor: ConnectionPatch = {
  cursor: { left: [{ _type: "Children", items: [] }] },
};
const shared: Shared = {
  count: 0,
  started: new Date(),
  root: { _type: "Text", value: "root" },
  selection: null,
};
const clearSelection: SharedPatch = { selection: null };
void [connection, emptyConnectionPatch, clearCursor, replaceCursor, shared, clearSelection];

// @ts-expect-error Derived Connection fields are readable but never writable.
const derivedPatch: ConnectionPatch = { userId: 8 };
// @ts-expect-error Non-nullable writable fields do not accept null.
const invalidNull: SharedPatch = { count: null };
// @ts-expect-error Nested values are complete replacements, not recursive partials.
const partialNestedPatch: SharedPatch = { root: { _type: "Text" } };
// @ts-expect-error Complete readable state requires every top-level field.
const incompleteShared: Shared = { count: 0, started: new Date(), root: { _type: "Text", value: "root" } };
void [derivedPatch, invalidNull, partialNestedPatch, incompleteShared];
"#,
    )
    .unwrap();

    let tsc = Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc");
    assert!(tsc.exists(), "TypeScript is required; run `bun install`");
    let output = Command::new(tsc)
        .args([
            "--noEmit",
            "--strict",
            "--skipLibCheck",
            "--module",
            "preserve",
            "--moduleResolution",
            "bundler",
            "verify.ts",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("typecheck generated state types");
    assert!(
        output.status.success(),
        "generated TypeScript state types failed to compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_rust_state_surface_compiles_and_preserves_patch_null_semantics() {
    let files = generated_state_files();
    let state = files
        .iter()
        .find(|file| file.path == Path::new("rust/state.rs"))
        .expect("generated Rust state module");
    assert!(state.contents.contains("pub user_id: i64"));
    assert!(state.contents.contains("pub cursor: PatchField<Option<"));
    let connection_patch = state
        .contents
        .split("pub struct ConnectionPatch")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(!connection_patch.contains("user_id"));

    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp_dir.path().join("src")).unwrap();
    std::fs::write(temp_dir.path().join("src/state.rs"), &state.contents).unwrap();
    std::fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"[package]
name = "generated-state-check"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1.0.203", features = ["derive"] }
serde_json = "1.0.117"
"#,
    )
    .unwrap();
    std::fs::write(
        temp_dir.path().join("src/main.rs"),
        r#"mod state;

fn main() {
    let connection = state::Connection {
        cursor: Some(std::collections::HashMap::from([(
            "left".to_string(),
            vec![Some(state::Node::Text { value: "complete".to_string() }), None],
        )])),
        label: "online".to_string(),
        user_id: 7,
    };
    assert_eq!(serde_json::to_value(connection).unwrap()["userId"], 7);

    let omitted = state::ConnectionPatch::default();
    assert_eq!(serde_json::to_value(omitted).unwrap(), serde_json::json!({}));
    let cleared = state::ConnectionPatch {
        cursor: state::PatchField::Value(None),
        ..Default::default()
    };
    assert_eq!(serde_json::to_value(cleared).unwrap(), serde_json::json!({ "cursor": null }));
    let decoded: state::ConnectionPatch = serde_json::from_value(serde_json::json!({ "cursor": null })).unwrap();
    assert_eq!(decoded.cursor, state::PatchField::Value(None));
    assert!(serde_json::from_value::<state::SharedPatch>(serde_json::json!({ "count": null })).is_err());
    assert_eq!(state::CONNECTION_METADATA.derived_fields, &["userId"]);
    assert_eq!(state::CONNECTION_METADATA.writable_fields, &["cursor", "label"]);
}
"#,
    )
    .unwrap();

    let output = Command::new("cargo")
        .args(["run", "--offline", "--quiet"])
        .current_dir(temp_dir.path())
        .output()
        .expect("compile and run generated Rust state types");
    assert!(
        output.status.success(),
        "generated Rust state types failed to compile or run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
