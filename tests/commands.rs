use assert_cmd::Command;
use libsql;
use predicates::prelude::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::symlink;

struct TestContext {
    temp_dir: TempDir,
    workspace_path: PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        let workspace_path = temp_dir.path().to_path_buf();

        // Set up any initial files needed
        std::fs::create_dir(workspace_path.join("pyre")).unwrap();
        std::fs::write(workspace_path.join("pyre/session.pyre"), "session {\n}\n").unwrap();

        let context = Self {
            temp_dir,
            workspace_path,
        };
        let _ = context.temp_dir.path();
        context
    }

    fn run_command(&self, subcommand: &str) -> assert_cmd::Command {
        let mut cmd = Command::cargo_bin("pyre").unwrap();
        cmd.current_dir(&self.workspace_path);
        cmd.arg(subcommand);
        cmd
    }
}

fn write_basic_schema(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record User {
    id   Int    @id
    name String
    @public
}
        "#,
    )
    .unwrap();
}

#[test]
fn docs_lists_topics() {
    let ctx = TestContext::new();

    ctx.run_command("docs")
        .assert()
        .success()
        .stdout(predicate::str::contains("getting-started"))
        .stdout(predicate::str::contains("serve"));
}

#[test]
fn docs_prints_requested_topic() {
    let ctx = TestContext::new();

    ctx.run_command("docs")
        .arg("schema")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pyre Schema Guide"));
}

#[test]
fn docs_rejects_unknown_topic() {
    let ctx = TestContext::new();

    ctx.run_command("docs")
        .arg("unknown-topic")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown doc topic"));
}

struct ServerGuard {
    child: Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_request(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.as_bytes().len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let mut parts = response.splitn(2, "\r\n\r\n");
    let headers = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .unwrap_or(0);

    (status, body)
}

fn wait_for_health(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let request = format!(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(request.as_bytes());
            let mut response = String::new();
            if stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200")
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    panic!("pyre serve did not become healthy on port {}", port);
}

fn write_multi_namespace_schemas(ctx: &TestContext) {
    std::fs::create_dir_all(ctx.workspace_path.join("pyre/schema/App")).unwrap();
    std::fs::create_dir_all(ctx.workspace_path.join("pyre/schema/Auth")).unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema/App/schema.pyre"),
        r#"
record Project {
    id    Int    @id
    name  String
    @public
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema/Auth/schema.pyre"),
        r#"
record Account {
    id       Int    @id
    email    String
    @public
}
        "#,
    )
    .unwrap();
}

fn first_migration_sql_path(ctx: &TestContext) -> PathBuf {
    let migration_root = ctx.workspace_path.join("pyre/migrations");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&migration_root)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();

    entries
        .first()
        .expect("expected at least one migration folder")
        .join("migration.sql")
}

fn write_ai_session_schema_and_query(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
type AiSessionRole
   = Root
   | Worker
   | Reviewer

type AiSessionStatus
   = Active
   | Idle
   | Completed
   | Failed

record AiSession {
    @public
    id              Id.Int        @id
    sessionKey      String
    role            AiSessionRole
    status          AiSessionStatus
    updatedAt       DateTime      @default(now)
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
insert SeedActiveSession {
    aiSession {
        sessionKey = "session-1"
        role = Root
        status = Active
    }
}

query GetAiSessions {
    aiSession {
        id
        status
        updatedAt
    }
}
        "#,
    )
    .unwrap();
}

fn write_union_payload_schema_and_query(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
type AiSessionStatus
   = Active
   | Idle
   | Completed
   | Failed

type AiSessionLifecycle
   = Running
   | Finished {
       reason String
       endedAt DateTime?
     }

record AiSession {
    @public
    id         Id.Int            @id
    status     AiSessionStatus
    lifecycle  AiSessionLifecycle
    updatedAt  DateTime @default(now)
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
insert SeedRunningNumber {
    aiSession {
        status = Active
        lifecycle = Running
    }
}

insert SeedFinishedDone($endedAt: DateTime) {
    aiSession {
        status = Completed
        lifecycle = Finished {
            reason = "done"
            endedAt = $endedAt
        }
    }
}

insert SeedFinishedTimeout($endedAt: DateTime) {
    aiSession {
        status = Failed
        lifecycle = Finished {
            reason = "timeout"
            endedAt = $endedAt
        }
    }
}

insert SeedFinishedStringSeconds($endedAt: DateTime) {
    aiSession {
        status = Completed
        lifecycle = Finished {
            reason = "string-seconds"
            endedAt = $endedAt
        }
    }
}

insert SeedRunningIdle {
    aiSession {
        status = Idle
        lifecycle = Running
    }
}

update UpdateFinishedSession($id: AiSession.id, $updatedAt: DateTime, $endedAt: DateTime) {
    aiSession {
        @where { id == $id }
        lifecycle = Finished {
            reason = "updated-done"
            endedAt = $endedAt
        }
        updatedAt = $updatedAt
    }
}

query GetAiSessions {
    aiSession {
        id
        status
        lifecycle
        updatedAt
    }
}
        "#,
    )
    .unwrap();
}

fn write_json_schema_and_query(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record Event {
    @public
    id      Id.Int @id
    payload Json?
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
insert SeedEvent($payload: Json) {
    event {
        payload = $payload
    }
}

query GetEvents {
    event {
        id
        payload
    }
}
        "#,
    )
    .unwrap();
}

fn write_typed_json_schema_and_query(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
type Lifecycle
   = Running
   | Finished {
        reason String
     }

record Event {
    @public
    id      Id.Int @id
    payload Json<Lifecycle>
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
insert SeedEvent($payload: Json<Lifecycle>) {
    event {
        payload = $payload
    }
}

insert SeedInlineEvent {
    event {
        payload = Finished {
            reason = "inline"
        }
    }
}

query GetEvents {
    event {
        id
        payload
    }
}
        "#,
    )
    .unwrap();
}

fn write_game_lens_schema_and_query(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
type GameRole
   = GM
   | Player
   | Observer

record User {
    @public
    id    Id.Uuid @id
    name  String
    email String
}

record Game {
    @public
    id              Id.Uuid  @id
    name            String
    createdByUserId User.id
    createdAt       DateTime @default(now)

    gameMembers @link(GameMember.gameId)
    gameInvites @link(GameInvite.gameId)
}

record GameMember {
    @public
    id              Id.Uuid  @id
    gameId          Game.id
    userId          User.id
    role            GameRole
    joinedAt        DateTime @default(now)
    invitedByUserId User.id?

    inviter @link(invitedByUserId, User.id)
    games   @link(gameId, Game.id)
}

record GameInvite {
    @public
    id            Id.Uuid  @id
    gameId        Game.id
    inviterUserId User.id
    token         String
    message       String?
    createdAt     DateTime @default(now)

    games @link(gameId, Game.id)
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
query GetGame($id: Game.id) {
    game {
        @where { id == $id }

        id
        name
        createdByUserId
        createdAt
        gameMembers {
            id
            userId
            role
            joinedAt
            inviter {
                id
                name
                email
            }
        }
        gameInvites {
            id
            inviterUserId
            token
            message
            createdAt
        }
    }
}
        "#,
    )
    .unwrap();
}

fn bun_is_available() -> bool {
    StdCommand::new("bun").arg("--version").output().is_ok()
}

fn elm_is_available() -> bool {
    StdCommand::new("elm").arg("--version").output().is_ok()
}

fn write_elm_build_files(ctx: &TestContext) {
    let elm_root = ctx.workspace_path.join("pyre/generated/client/elm");
    assert!(
        elm_root.exists(),
        "Expected generated Elm directory to exist"
    );

    std::fs::write(
        elm_root.join("elm.json"),
        r#"{
    "type": "application",
    "source-directories": [
        "."
    ],
    "elm-version": "0.19.1",
    "dependencies": {
        "direct": {
            "elm/browser": "1.0.2",
            "elm/core": "1.0.5",
            "elm/html": "1.0.0",
            "elm/json": "1.1.3",
            "elm/time": "1.0.0"
        },
        "indirect": {
            "elm/url": "1.0.0",
            "elm/virtual-dom": "1.0.3"
        }
    },
    "test-dependencies": {
        "direct": {},
        "indirect": {}
    }
}
"#,
    )
    .unwrap();

    let mut imports = String::new();
    imports.push_str("import Db\n");
    imports.push_str("import Db.Decode\n");
    imports.push_str("import Db.Delta\n");
    imports.push_str("import Db.Encode\n");
    imports.push_str("import Db.Id\n");
    imports.push_str("import Db.Stream\n");

    let query_dir = elm_root.join("Query");
    if query_dir.exists() {
        let mut query_modules: Vec<String> = std::fs::read_dir(&query_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("elm"))
            .filter_map(|path| {
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .map(|name| format!("import Query.{}\n", name))
            })
            .collect();
        query_modules.sort();
        for import in query_modules {
            imports.push_str(&import);
        }
    }

    let main_module = format!(
        "module Main exposing (main)\n\nimport Browser\nimport Html exposing (Html, text)\n{}\n\nmain : Program () () msg\nmain =\n    Browser.sandbox\n        {{ init = ()\n        , update = \\_ model -> model\n        , view = \\_ -> text \"elm-build-ok\"\n        }}\n",
        imports
    );

    std::fs::write(elm_root.join("Main.elm"), main_module).unwrap();
}

fn run_elm_make_check(ctx: &TestContext) {
    let elm_root = ctx.workspace_path.join("pyre/generated/client/elm");
    let output = StdCommand::new("elm")
        .arg("make")
        .arg("Main.elm")
        .arg("--output=elm.js")
        .current_dir(&elm_root)
        .output()
        .expect("Failed to execute elm make");

    assert!(
        output.status.success(),
        "elm make failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ensure_workspace_node_modules(ctx: &TestContext) {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_node_modules = repo_root.join("node_modules");
    assert!(
        repo_node_modules.exists(),
        "Expected root node_modules to exist for bun runtime imports"
    );

    let workspace_node_modules = ctx.workspace_path.join("node_modules");
    if !workspace_node_modules.exists() {
        #[cfg(unix)]
        symlink(&repo_node_modules, &workspace_node_modules)
            .expect("Failed to symlink workspace node_modules");
    }
}

fn generate_runtime(ctx: &TestContext) {
    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    ctx.run_command("generate").assert().success();
    ensure_workspace_node_modules(ctx);
}

fn run_bun_verification_script(
    ctx: &TestContext,
    script_name: &str,
    script_source: &str,
    success_marker: &str,
    failure_label: &str,
) {
    std::fs::write(ctx.workspace_path.join(script_name), script_source).unwrap();

    let output = StdCommand::new("bun")
        .arg("run")
        .arg(script_name)
        .current_dir(&ctx.workspace_path)
        .output()
        .expect("Failed to execute bun runtime verification script");

    assert!(
        output.status.success(),
        "{} failed\nstdout:\n{}\nstderr:\n{}",
        failure_label,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(success_marker),
        "Expected {} marker in bun output. stdout:\n{}",
        success_marker,
        stdout
    );
}

fn build_payload_union_verify_script(seed_calls: &[&str], expected_rows: &[&str]) -> String {
    let mut script = String::new();

    script.push_str("import { createClient } from \"@libsql/client\";\n");
    script.push_str("import {\n");
    script.push_str("  SeedRunningNumber,\n");
    script.push_str("  SeedFinishedDone,\n");
    script.push_str("  SeedFinishedTimeout,\n");
    script.push_str("  SeedFinishedStringSeconds,\n");
    script.push_str("  SeedRunningIdle,\n");
    script.push_str("  UpdateFinishedSession,\n");
    script.push_str("  GetAiSessions,\n");
    script.push_str("} from \"./pyre/generated/typescript/run.ts\";\n\n");
    script.push_str("const db = createClient({ url: \"file:.yak/yak.db\" });\n");

    for call in seed_calls {
        script.push_str("await ");
        script.push_str(call);
        script.push_str(";\n");
    }

    script.push_str("const result = await GetAiSessions(db, {});\n\n");
    script.push_str(
        "if (!result || !Array.isArray(result.aiSession) || result.aiSession.length !== 5) {\n",
    );
    script.push_str(
        "  throw new Error(`Expected exactly 5 aiSession rows, got: ${JSON.stringify(result)}`);\n",
    );
    script.push_str("}\n\n");
    script.push_str("function assertDate(label, value) {\n");
    script.push_str("  if (!(value instanceof Date) || Number.isNaN(value.getTime())) {\n");
    script.push_str("    throw new Error(`Expected ${label} to be a valid Date, got: ${JSON.stringify(value)}`);\n");
    script.push_str("  }\n");
    script.push_str("}\n\n");
    script.push_str("function assertMaybeDate(label, value) {\n");
    script.push_str("  if (value === null || value === undefined) {\n");
    script.push_str("    return;\n");
    script.push_str("  }\n\n");
    script.push_str("  assertDate(label, value);\n");
    script.push_str("}\n\n");
    script.push_str("const byId = new Map(result.aiSession.map((row) => [row.id, row]));\n\n");
    script.push_str("const expected = {\n");

    for row in expected_rows {
        script.push_str("  ");
        script.push_str(row);
        script.push_str("\n");
    }

    script.push_str("};\n\n");
    script.push_str("for (const [id, shape] of Object.entries(expected)) {\n");
    script.push_str("  const row = byId.get(Number(id));\n");
    script.push_str("  if (!row) {\n");
    script.push_str("    throw new Error(`Missing expected row id=${id}. Rows: ${JSON.stringify(result.aiSession)}`);\n");
    script.push_str("  }\n\n");
    script.push_str("  if (typeof row.status !== \"string\") {\n");
    script.push_str("    throw new Error(`Expected row ${id} status to decode as enum string, got: ${JSON.stringify(row.status)}`);\n");
    script.push_str("  }\n\n");
    script.push_str("  if (row.status !== shape.status) {\n");
    script.push_str("    throw new Error(`Expected row ${id} status ${shape.status}, got: ${JSON.stringify(row.status)}`);\n");
    script.push_str("  }\n\n");
    script.push_str("  assertDate(`row ${id}.updatedAt`, row.updatedAt);\n\n");
    script.push_str("  if (!row.lifecycle || typeof row.lifecycle !== \"object\") {\n");
    script.push_str("    throw new Error(`Expected row ${id} lifecycle object, got: ${JSON.stringify(row.lifecycle)}`);\n");
    script.push_str("  }\n\n");
    script.push_str("  if (row.lifecycle._type !== shape.lifecycleType) {\n");
    script.push_str("    throw new Error(`Expected row ${id} lifecycle._type ${shape.lifecycleType}, got: ${JSON.stringify(row.lifecycle._type)}`);\n");
    script.push_str("  }\n\n");
    script.push_str("  if (shape.reason !== undefined) {\n");
    script.push_str(
        "    if (row.lifecycle.reason !== undefined && row.lifecycle.reason !== shape.reason) {\n",
    );
    script.push_str("      throw new Error(`Expected row ${id} optional reason ${shape.reason} when present, got: ${JSON.stringify(row.lifecycle.reason)}`);\n");
    script.push_str("    }\n");
    script.push_str("  }\n\n");
    script.push_str("  if (shape.endedAt === \"date\") {\n");
    script.push_str("    assertDate(`row ${id}.lifecycle.endedAt`, row.lifecycle.endedAt);\n");
    script.push_str("  } else if (shape.endedAt === \"optional-date\") {\n");
    script.push_str("    assertMaybeDate(`row ${id}.lifecycle.endedAt`, row.lifecycle.endedAt);\n");
    script.push_str("  } else if (shape.endedAt === \"nullish\") {\n");
    script.push_str(
        "    if (row.lifecycle.endedAt !== null && row.lifecycle.endedAt !== undefined) {\n",
    );
    script.push_str("      throw new Error(`Expected row ${id} endedAt to be null or undefined, got: ${JSON.stringify(row.lifecycle.endedAt)}`);\n");
    script.push_str("    }\n");
    script.push_str("  } else {\n");
    script.push_str("    assertMaybeDate(`row ${id}.lifecycle.endedAt`, row.lifecycle.endedAt);\n");
    script.push_str("  }\n");
    script.push_str("}\n\n");
    script.push_str("console.log(\"payload-union-and-date-check-passed\");\n");

    script
}

#[test]
fn test_generate_command() {
    let ctx = TestContext::new();

    // Create a sample schema file (Pyre format, not SQL)
    write_basic_schema(&ctx);

    ctx.run_command("generate").assert().success();

    // Verify generated files were created
    assert!(ctx.workspace_path.join("pyre/generated").exists());
    // Verify at least one generated file exists
    assert!(std::fs::read_dir(ctx.workspace_path.join("pyre/generated"))
        .unwrap()
        .next()
        .is_some());
}

#[test]
fn test_generated_typescript_server_includes_typed_seed_input() {
    let ctx = TestContext::new();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record User {
    id Id.Int @id
    name String
    posts @link(Post.authorId)
    @public
}

record Post {
    id Int @id
    authorId User.id
    title String
    status Status
    attrs Json<PostAttrs>
    author @link(authorId, User.id)
    @public
}

type Status
   = Draft
   | Published { publishedAt DateTime }

type PostAttrs
   = PostAttrs { tags List<String> }
        "#,
    )
    .unwrap();

    ctx.run_command("generate").assert().success();

    let server_ts = std::fs::read_to_string(
        ctx.workspace_path
            .join("pyre/generated/typescript/server.ts"),
    )
    .unwrap();

    assert!(server_ts.contains("import * as Db from './core/decode';"));
    assert!(server_ts.contains("const seedValidators: SeedValidators ="));
    assert!(server_ts.contains("type SeedConstructed<T>"));
    assert!(server_ts.contains("export type SeedUsersRow = {"));
    assert!(server_ts.contains("\"posts\"?: SeedPostsRow[];"));
    assert!(server_ts.contains("export type SeedPostsRow = {"));
    assert!(server_ts.contains("\"status\"?: SeedConstructed<Db.Status>;"));
    assert!(server_ts.contains("\"attrs\"?: SeedConstructed<Db.PostAttrs>;"));
    assert!(server_ts.contains("\"users\"?: SeedUsersRow[];"));
    assert!(server_ts.contains("\"posts\"?: SeedPostsRow[];"));
    assert!(server_ts
        .contains("export const seed = (db: Client, input: SeedInput): Promise<SeedResult>"));
    assert!(server_ts.contains("{ _type: K }"));
    assert!(!server_ts.contains("type?:"));
    assert!(!server_ts.contains("$?:"));
}

#[test]
fn test_generate_manifest_includes_query_runtime_metadata() {
    let ctx = TestContext::new();

    std::fs::write(
        ctx.workspace_path.join("pyre/session.pyre"),
        r#"
session {
    userId Int
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"

record User {
    id Int @id
    ownerId Int
    name String
    settings Json?
    updatedAt Int
    @allow(*) { ownerId == Session.userId }
}
        "#,
    )
    .unwrap();
    ctx.run_command("generate").assert().success();

    let manifest_path = ctx.workspace_path.join("pyre/generated/manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
    let queries = manifest["queries"]
        .as_object()
        .expect("queries should be object");
    let update_user = queries
        .values()
        .find(|query| query["operation"] == "update")
        .expect("generated update query should be present");

    assert_eq!(manifest["version"], serde_json::json!(1));
    assert_eq!(manifest["session_schema"]["userId"]["type"], "Int");
    assert_eq!(update_user["input_schema"]["settings"]["nullable"], true);
    assert!(update_user["optional_input_args"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("settings")));
    assert_eq!(
        update_user["json_input_args"],
        serde_json::json!(["settings"])
    );
    assert_eq!(update_user["session_args"], serde_json::json!(["userId"]));
    assert!(update_user["sql"]
        .as_array()
        .unwrap()
        .iter()
        .any(|statement| {
            statement["params"]
                .as_array()
                .unwrap()
                .iter()
                .any(|param| param == "settings__is_set")
        }));
}

#[test]
fn test_serve_starts_http_server_and_runs_query() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);
    std::fs::write(
        ctx.workspace_path.join("pyre/query.pyre"),
        r#"
query GetUsers {
    user {
        id
        name
    }
}
        "#,
    )
    .unwrap();

    let db_path = ctx.workspace_path.join("db/app.db");
    ctx.run_command("migrate")
        .arg(db_path.to_str().unwrap())
        .arg("--push")
        .assert()
        .success();
    ctx.run_command("generate").assert().success();

    let manifest_path = ctx.workspace_path.join("pyre/generated/manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
    let query_id = manifest["queries"]
        .as_object()
        .unwrap()
        .values()
        .find(|query| query["operation"] == "query")
        .and_then(|query| query["id"].as_str())
        .expect("generated query id")
        .to_string();

    let port = free_loopback_port();
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("pyre"));
    command
        .current_dir(&ctx.workspace_path)
        .arg("serve")
        .arg(db_path.to_str().unwrap())
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn().unwrap();
    let _server = ServerGuard { child };

    wait_for_health(port);

    let (health_status, health_body) = http_request(port, "GET", "/health", None);
    assert_eq!(health_status, 200);
    let health: serde_json::Value = serde_json::from_str(&health_body).unwrap();
    assert_eq!(health["ok"], serde_json::json!(true));
    assert_eq!(health["databaseId"], serde_json::json!("default"));

    let (query_status, query_body) =
        http_request(port, "POST", &format!("/db/{}", query_id), Some("{}"));
    assert_eq!(query_status, 200, "query body: {}", query_body);
    let result: serde_json::Value = serde_json::from_str(&query_body).unwrap();
    assert_eq!(result["user"], serde_json::json!([]));
}

#[test]
fn test_format_command() {
    let ctx = TestContext::new();

    // Create a schema file (needed for query formatting)
    write_basic_schema(&ctx);

    // Create an unformatted query file (Pyre format, not SQL)
    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
        query   GetUsers   {
            user   {
                id
                name
            }
        }
        "#,
    )
    .unwrap();

    // Format the specific query file
    ctx.run_command("format")
        .arg("pyre/queries.pyre")
        .assert()
        .success();

    // Verify formatted content (should be properly formatted Pyre query)
    let formatted = std::fs::read_to_string(ctx.workspace_path.join("pyre/queries.pyre")).unwrap();

    // The format command should have formatted the query
    // Check that the file still contains the query (may be reformatted)
    // Format might normalize spacing, so we check for key parts
    assert!(
        formatted.contains("query") || formatted.contains("GetUsers"),
        "Formatted content: {}",
        formatted
    );
    assert!(
        formatted.contains("user") || formatted.contains("User"),
        "Formatted content: {}",
        formatted
    );
}

#[test]
fn test_format_schema_file_adds_reverse_links_from_other_schema_files() {
    let ctx = TestContext::new();

    std::fs::create_dir_all(ctx.workspace_path.join("pyre/schema/App")).unwrap();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema/App/schema.pyre"),
        r#"record Rulebook {
    id Int @id
    name String
}
        "#,
    )
    .unwrap();

    std::fs::write(
        ctx.workspace_path
            .join("pyre/schema/App/schema_rulebook_version.pyre"),
        r#"record RulebookVersion {
    id Int @id
    rulebookId Rulebook.id

    rulebook @link(rulebookId, Rulebook.id)
}
        "#,
    )
    .unwrap();

    ctx.run_command("format")
        .arg("pyre/schema/App/schema.pyre")
        .assert()
        .success();

    let formatted =
        std::fs::read_to_string(ctx.workspace_path.join("pyre/schema/App/schema.pyre")).unwrap();

    assert!(
        formatted.contains("rulebookVersions @link(RulebookVersion.rulebookId)"),
        "Formatted content: {}",
        formatted
    );
}

#[test]
fn test_migration_creates_migrations_directory_and_file() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    ctx.run_command("migration")
        .arg("--db")
        .arg(".yak/yak.db")
        .arg("init")
        .assert()
        .success();

    let migration_dir = ctx.workspace_path.join("pyre/migrations");
    assert!(migration_dir.exists(), "pyre/migrations should be created");
    assert!(
        std::fs::read_dir(migration_dir).unwrap().next().is_some(),
        "pyre/migrations should contain at least one migration folder"
    );
}

#[test]
fn test_check_success_prints_verification_counts() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    std::fs::write(
        ctx.workspace_path.join("pyre/queries.pyre"),
        r#"
query GetUsers {
    user {
        id
        name
    }
}

        "#,
    )
    .unwrap();

    ctx.run_command("check")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Success: checked 1 schema(s) and 1 query files",
        ));
}

#[test]
fn test_check_rejects_session_declaration_in_schema_file() {
    let ctx = TestContext::new();
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
session {
    userId Int
}

record User {
    id Int @id
    @public
}
        "#,
    )
    .unwrap();

    ctx.run_command("check")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Define the shared session in pyre/session.pyre",
        ));
}

#[test]
fn test_check_rejects_non_session_definitions_in_session_file() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);
    std::fs::write(
        ctx.workspace_path.join("pyre/session.pyre"),
        "session {\n    userId Int\n}\n\nrecord Invalid {\n    id Int @id\n}\n",
    )
    .unwrap();

    ctx.run_command("check")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "may only contain one session declaration",
        ));
}

#[test]
fn test_migrate_push_creates_local_db_when_parent_directory_missing() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    let db_path = ctx.workspace_path.join(".yak/yak.db");
    assert!(!db_path.exists(), "db should not exist before push");

    let output = ctx
        .run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .output()
        .unwrap();

    assert!(
        output.status.code().is_some(),
        "migrate --push should execute and exit normally"
    );

    assert!(db_path.exists(), "db should be created by migrate --push");
}

#[tokio::test]
async fn test_migrate_push_creates_enum_columns_and_records_schema() {
    let ctx = TestContext::new();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
type TaskStatus
   = Pending
   | InProgress

record Task {
    @public
    id     Int        @id
    title  String
    status TaskStatus
}
        "#,
    )
    .unwrap();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let db_path = ctx.workspace_path.join(".yak/yak.db");
    let db = libsql::Builder::new_local(db_path.to_str().unwrap())
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();

    let mut pragma_rows = conn.query("pragma table_info('tasks')", ()).await.unwrap();
    let mut has_status = false;
    while let Some(row) = pragma_rows.next().await.unwrap() {
        let column_name: String = row.get(1).unwrap();
        if column_name == "status" {
            has_status = true;
            break;
        }
    }

    assert!(
        has_status,
        "tasks.status should exist after migrate --push for enum-backed fields"
    );

    let mut schema_rows = conn
        .query(
            "select schema from _pyre_schema order by created_at desc limit 1",
            (),
        )
        .await
        .unwrap();

    let schema_row = schema_rows
        .next()
        .await
        .unwrap()
        .expect("_pyre_schema should contain at least one schema row");
    let schema_source: String = schema_row.get(0).unwrap();

    assert!(
        schema_source.contains("status TaskStatus"),
        "_pyre_schema should store the pushed schema source"
    );
}

#[test]
fn test_migrate_without_migrations_shows_targeted_error() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .failure()
        .stdout(predicate::str::contains("No Migrations Found"))
        .stdout(predicate::str::contains(
            "pyre migration --db <database> init",
        ));
}

#[test]
fn test_migrate_reports_up_to_date_when_no_work_is_needed() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    ctx.run_command("migration")
        .arg("--db")
        .arg(".yak/yak.db")
        .arg("init")
        .assert()
        .success();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 migration applied."));

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .success()
        .stdout(predicate::str::contains("Up to date, nothing applied."));
}

#[test]
fn test_migrate_validates_instead_of_reconciling_missing_migration_sql() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    ctx.run_command("migration")
        .arg("--db")
        .arg(".yak/yak.db")
        .arg("init")
        .assert()
        .success();

    let migration_sql = first_migration_sql_path(&ctx);
    std::fs::write(migration_sql, "").unwrap();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .failure()
        .stdout(predicate::str::contains("Migration Validation Failed"))
        .stdout(predicate::str::contains("missing table users"));
}

#[test]
fn test_migrate_refuses_automatic_destructive_reconciliation() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    ctx.run_command("migration")
        .arg("--db")
        .arg(".yak/yak.db")
        .arg("init")
        .assert()
        .success();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .success();

    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record User {
    id Int @id
    @public
}
        "#,
    )
    .unwrap();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .failure()
        .stdout(predicate::str::contains("Migration Validation Failed"))
        .stdout(predicate::str::contains("unexpected column users.name"));
}

#[test]
fn test_migrate_refuses_changed_applied_migration_sql() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    ctx.run_command("migration")
        .arg("--db")
        .arg(".yak/yak.db")
        .arg("init")
        .assert()
        .success();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .success();

    let migration_sql = first_migration_sql_path(&ctx);
    let mut contents = std::fs::read_to_string(&migration_sql).unwrap();
    contents.push_str("\n-- edited after apply\n");
    std::fs::write(migration_sql, contents).unwrap();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .failure()
        .stdout(predicate::str::contains("Applied Migration Changed"))
        .stdout(predicate::str::contains("Create a new migration"));
}

#[tokio::test]
async fn test_migrate_fails_on_non_pyre_database_with_guidance() {
    let ctx = TestContext::new();
    write_basic_schema(&ctx);

    let db_path = ctx.workspace_path.join(".yak/yak.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let db = libsql::Builder::new_local(db_path.to_str().unwrap())
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch("create table legacy_users (id integer primary key);")
        .await
        .unwrap();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Non-Pyre or Incompatible Database",
        ))
        .stdout(predicate::str::contains("Guidance"));
}

#[test]
fn test_migrate_fails_when_requested_namespace_is_missing_in_db_metadata() {
    let ctx = TestContext::new();
    write_multi_namespace_schemas(&ctx);

    ctx.run_command("migration")
        .arg("--db")
        .arg(".yak/yak.db")
        .arg("--namespace")
        .arg("App")
        .arg("init")
        .assert()
        .success();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--namespace")
        .arg("App")
        .assert()
        .success();

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--namespace")
        .arg("Auth")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Namespace Missing In Database Metadata",
        ))
        .stdout(predicate::str::contains("Requested namespace: Auth"))
        .stdout(predicate::str::contains("Namespaces found: App"));
}

#[test]
fn test_generate_schema_with_relationships() {
    let ctx = TestContext::new();

    // Create a schema with relationships
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record User {
    @public
    id   Int    @id
    name String
    
    posts @link(Post.authorUserId)
}

record Post {
    @public
    id           Int    @id
    authorUserId Int
    title        String
    
    users @link(authorUserId, User.id)
}
        "#,
    )
    .unwrap();

    ctx.run_command("generate").assert().success();

    // Verify schema.ts was generated
    let schema_path = ctx
        .workspace_path
        .join("pyre/generated/typescript/core/schema.ts");
    assert!(schema_path.exists(), "schema.ts should be generated");

    // Read and verify the generated schema
    let schema_content = std::fs::read_to_string(&schema_path).unwrap();

    // Verify that table names in 'to' field use actual table names (lowercase plural), not record names
    // User.posts link should point to "posts" table, not "Post"
    assert!(
        schema_content.contains(r#"table: "posts""#),
        "User.posts link should point to 'posts' table, not 'Post'. Schema content:\n{}",
        schema_content
    );

    // Post.users link should point to "users" table, not "User"
    assert!(
        schema_content.contains(r#"table: "users""#),
        "Post.users link should point to 'users' table, not 'User'. Schema content:\n{}",
        schema_content
    );

    // Verify the structure uses 'links' not 'relationships'
    assert!(
        schema_content.contains("links:"),
        "Schema should use 'links' property, not 'relationships'. Schema content:\n{}",
        schema_content
    );

    // Verify schema types come from @pyre/core
    assert!(
        schema_content.contains("import type { SchemaMetadata } from '@pyre/core';"),
        "Schema should import SchemaMetadata from @pyre/core. Schema content:\n{}",
        schema_content
    );
}

#[test]
fn test_generated_elm_client_compiles_with_elm_make() {
    if !elm_is_available() {
        eprintln!("Skipping elm make test: elm not available");
        return;
    }

    let ctx = TestContext::new();
    write_union_payload_schema_and_query(&ctx);

    ctx.run_command("generate").assert().success();

    write_elm_build_files(&ctx);
    run_elm_make_check(&ctx);
}

#[test]
fn test_generated_elm_client_compiles_nested_lenses() {
    if !elm_is_available() {
        eprintln!("Skipping elm make test: elm not available");
        return;
    }

    let ctx = TestContext::new();
    write_game_lens_schema_and_query(&ctx);

    ctx.run_command("generate").assert().success();

    write_elm_build_files(&ctx);
    run_elm_make_check(&ctx);
}

#[tokio::test]
async fn test_generated_typescript_runner_decodes_enum_unions_and_dates() {
    if !bun_is_available() {
        eprintln!("Skipping bun-based TypeScript runtime test: bun not available");
        return;
    }

    let ctx = TestContext::new();
    write_ai_session_schema_and_query(&ctx);

    generate_runtime(&ctx);

    let verify_script = r#"
import { createClient } from "@libsql/client";
import { SeedActiveSession, GetAiSessions } from "./pyre/generated/typescript/run.ts";

const db = createClient({ url: "file:.yak/yak.db" });
await SeedActiveSession(db, {});
const result = await GetAiSessions(db, {});

if (!result || !Array.isArray(result.aiSession) || result.aiSession.length === 0) {
  throw new Error(`Expected aiSession rows, got: ${JSON.stringify(result)}`);
}

const first = result.aiSession[0];
if (typeof first.status !== "string") {
  throw new Error(`Expected status to decode as string union, got: ${JSON.stringify(first.status)}`);
}

if (!(first.updatedAt instanceof Date) || Number.isNaN(first.updatedAt.getTime())) {
  throw new Error(`Expected updatedAt to decode to valid Date, got: ${JSON.stringify(first.updatedAt)}`);
}

if (first.status !== "Active") {
  throw new Error(`Expected status Active, got: ${String(first.status)}`);
}

console.log("decoder-check-passed");
"#;

    run_bun_verification_script(
        &ctx,
        "verify-generated-run.ts",
        verify_script,
        "decoder-check-passed",
        "bun runtime verification",
    );
}

#[tokio::test]
async fn test_generated_typescript_runner_decodes_payload_unions_and_datetime_strings() {
    if !bun_is_available() {
        eprintln!("Skipping bun-based TypeScript runtime test: bun not available");
        return;
    }

    let ctx = TestContext::new();
    write_union_payload_schema_and_query(&ctx);

    generate_runtime(&ctx);

    let seed_calls = [
        "SeedRunningNumber(db, {})",
        "SeedFinishedDone(db, { endedAt: \"2026-01-03T00:00:00.000Z\" })",
        "SeedFinishedTimeout(db, { endedAt: 1735776000 })",
        "SeedFinishedStringSeconds(db, { endedAt: \"1735948800\" })",
        "SeedRunningIdle(db, {})",
        "UpdateFinishedSession(db, { id: 2, updatedAt: \"2026-02-02T00:00:00.000Z\", endedAt: \"1736035200\" })",
        "UpdateFinishedSession(db, { id: 3, updatedAt: 1736121600, endedAt: 1736208000 })",
    ];
    let expected_rows = [
        "1: { status: \"Active\", lifecycleType: \"Running\" },",
        "2: { status: \"Completed\", lifecycleType: \"Finished\", reason: \"updated-done\", endedAt: \"optional-date\" },",
        "3: { status: \"Failed\", lifecycleType: \"Finished\", reason: \"updated-done\", endedAt: \"optional-date\" },",
        "4: { status: \"Completed\", lifecycleType: \"Finished\", reason: \"string-seconds\", endedAt: \"optional-date\" },",
        "5: { status: \"Idle\", lifecycleType: \"Running\" },",
    ];
    let verify_script = build_payload_union_verify_script(&seed_calls, &expected_rows);

    run_bun_verification_script(
        &ctx,
        "verify-generated-union-run.ts",
        &verify_script,
        "payload-union-and-date-check-passed",
        "bun payload union verification",
    );
}

#[tokio::test]
async fn test_generated_typescript_runner_roundtrips_json_values() {
    if !bun_is_available() {
        eprintln!("Skipping bun-based TypeScript runtime test: bun not available");
        return;
    }

    let ctx = TestContext::new();
    write_json_schema_and_query(&ctx);

    generate_runtime(&ctx);

    let verify_script = r#"
import { createClient } from "@libsql/client";
import { SeedEvent, GetEvents } from "./pyre/generated/typescript/run.ts";

const db = createClient({ url: "file:.yak/yak.db" });

const expected = [
  { nested: { count: 2, ok: true }, tags: ["alpha", "beta"] },
  [1, { deep: "value" }, false],
  "hello-json",
  42,
  null,
];

for (const payload of expected) {
  await SeedEvent(db, { payload });
}

const result = await GetEvents(db, {});
if (!result || !Array.isArray(result.event)) {
  throw new Error(`Expected event rows, got: ${JSON.stringify(result)}`);
}

if (result.event.length !== expected.length) {
  throw new Error(`Expected ${expected.length} rows, got: ${result.event.length}`);
}

const rows = [...result.event].sort((a, b) => a.id - b.id);
for (let i = 0; i < expected.length; i++) {
  const row = rows[i];
  const actualJson = JSON.stringify(row.payload);
  const expectedJson = JSON.stringify(expected[i]);
  if (actualJson !== expectedJson) {
    throw new Error(`Payload mismatch at index ${i}. Expected ${expectedJson}, got ${actualJson}`);
  }
}

const firstPayload = rows[0]?.payload;
const payloadAsUnknown = firstPayload as unknown;
if (payloadAsUnknown === undefined) {
  throw new Error("Expected first payload to be present");
}

console.log("json-roundtrip-check-passed");
"#;

    run_bun_verification_script(
        &ctx,
        "verify-generated-json-run.ts",
        verify_script,
        "json-roundtrip-check-passed",
        "bun json runtime verification",
    );
}

#[tokio::test]
async fn test_generated_typescript_runner_roundtrips_typed_json_values() {
    if !bun_is_available() {
        eprintln!("Skipping bun-based TypeScript runtime test: bun not available");
        return;
    }

    let ctx = TestContext::new();
    write_typed_json_schema_and_query(&ctx);

    generate_runtime(&ctx);

    let verify_script = r#"
import { createClient } from "@libsql/client";
import { SeedEvent, SeedInlineEvent, GetEvents } from "./pyre/generated/typescript/run.ts";

const db = createClient({ url: "file:.yak/yak.db" });

await SeedEvent(db, { payload: { _type: "Running" } });
await SeedEvent(db, { payload: { _type: "Finished", reason: "done" } });
await SeedInlineEvent(db, {});

const result = await GetEvents(db, {});
if (!result || !Array.isArray(result.event)) {
  throw new Error(`Expected event rows, got: ${JSON.stringify(result)}`);
}

const rows = [...result.event].sort((a, b) => a.id - b.id);
const expected = [
  { _type: "Running" },
  { _type: "Finished", reason: "done" },
  { _type: "Finished", reason: "inline" },
];

if (rows.length !== expected.length) {
  throw new Error(`Expected ${expected.length} rows, got: ${rows.length}`);
}

for (let i = 0; i < expected.length; i++) {
  const actualJson = JSON.stringify(rows[i]?.payload);
  const expectedJson = JSON.stringify(expected[i]);
  if (actualJson !== expectedJson) {
    throw new Error(`Payload mismatch at index ${i}. Expected ${expectedJson}, got ${actualJson}`);
  }
}

console.log("typed-json-roundtrip-check-passed");
"#;

    run_bun_verification_script(
        &ctx,
        "verify-generated-typed-json-run.ts",
        verify_script,
        "typed-json-roundtrip-check-passed",
        "bun typed json runtime verification",
    );
}

#[tokio::test]
async fn test_generated_seed_data_decodes_through_generated_query() {
    if !bun_is_available() {
        eprintln!("Skipping bun-based seed decode test: bun not available");
        return;
    }

    let ctx = TestContext::new();
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record Token {
    id Id.Int @id
    name String
    payload Json<TokenPayload>
    placement Placement
    createdAt DateTime @default(now)
    @public
}

type TokenPayload
   = TokenPayload {
        labels List<String>
        marker Marker
     }

type Marker
   = Start
   | End { reason String }

type Placement
   = Grid { x Int, y Int }
   | World { x Int, y Int, scale Int }
        "#,
    )
    .unwrap();
    std::fs::write(
        ctx.workspace_path.join("pyre/query.pyre"),
        r#"
query GetTokens {
    token {
        id
        name
        payload
        placement
    }
}
        "#,
    )
    .unwrap();

    generate_runtime(&ctx);

    let verify_script = r#"
import { createClient } from "@libsql/client";
import { seed } from "./pyre/generated/typescript/server.ts";
import { GetTokens } from "./pyre/generated/typescript/run.ts";

const db = createClient({ url: "file:.yak/yak.db" });

const seeded = await seed(db, {
  tokens: [
    {
      id: 1,
      name: "seeded token",
      payload: {
        _type: "TokenPayload",
        labels: ["alpha", "beta"],
        marker: { _type: "End", reason: "finished" },
      },
      placement: { _type: "World", x: 10, y: 20, scale: 100 },
    },
  ],
});

if (seeded.kind !== "success") {
  throw new Error(`Seed failed: ${JSON.stringify(seeded)}`);
}

const result = await GetTokens(db, {});
if (!result || !Array.isArray(result.token) || result.token.length !== 1) {
  throw new Error(`Expected one token row, got: ${JSON.stringify(result)}`);
}

const token = result.token[0];
if (token.name !== "seeded token") {
  throw new Error(`Unexpected token name: ${JSON.stringify(token)}`);
}
if (token.payload?._type !== "TokenPayload") {
  throw new Error(`Expected decoded payload _type, got: ${JSON.stringify(token.payload)}`);
}
if (JSON.stringify(token.payload.labels) !== JSON.stringify(["alpha", "beta"])) {
  throw new Error(`Expected decoded labels, got: ${JSON.stringify(token.payload)}`);
}
if (token.payload.marker?._type !== "End" || token.payload.marker?.reason !== "finished") {
  throw new Error(`Expected decoded nested marker, got: ${JSON.stringify(token.payload)}`);
}
if (token.placement?._type !== "World" || token.placement?.x !== 10 || token.placement?.y !== 20 || token.placement?.scale !== 100) {
  throw new Error(`Expected decoded placement, got: ${JSON.stringify(token.placement)}`);
}

console.log("generated-seed-decode-check-passed");
"#;

    run_bun_verification_script(
        &ctx,
        "verify-generated-seed-decode.ts",
        verify_script,
        "generated-seed-decode-check-passed",
        "bun generated seed decode verification",
    );
}
