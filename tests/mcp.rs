use assert_cmd::Command;
use libsql;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};
use tempfile::TempDir;

struct TestContext {
    temp_dir: TempDir,
    workspace_path: PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        let workspace_path = temp_dir.path().to_path_buf();
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

fn write_schema(ctx: &TestContext) {
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

fn write_session_schema(ctx: &TestContext) {
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

record Note {
    id      Int    @id
    ownerId Int
    body    String
    @public
}
        "#,
    )
    .unwrap();
}

fn write_nullable_schema(ctx: &TestContext) {
    std::fs::write(
        ctx.workspace_path.join("pyre/schema.pyre"),
        r#"
record User {
    id       Int     @id
    nickname String?
    @public
}
        "#,
    )
    .unwrap();
}

fn message_json(value: &serde_json::Value) -> String {
    format!("{}\n", value)
}

fn parse_message_json(output: &[u8]) -> serde_json::Value {
    let raw = String::from_utf8_lossy(output);
    let body = raw.lines().next().expect("expected MCP response");
    serde_json::from_str(body).expect("expected JSON response body")
}

fn call_mcp(ctx: &TestContext, request: serde_json::Value) -> serde_json::Value {
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("pyre"))
        .arg("mcp")
        .current_dir(&ctx.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pyre mcp");

    {
        let stdin = child.stdin.as_mut().expect("expected child stdin");
        stdin
            .write_all(message_json(&request).as_bytes())
            .expect("failed to write MCP request");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("failed to wait for pyre mcp");
    assert!(
        output.status.success(),
        "pyre mcp failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    parse_message_json(&output.stdout)
}

fn call_mcp_tool(ctx: &TestContext, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let response = call_mcp(
        ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }),
    );

    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected MCP text content, got response: {response}"));
    serde_json::from_str(text).expect("expected JSON tool result text")
}

fn call_mcp_tool_error(
    ctx: &TestContext,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    call_mcp(
        ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }),
    )
}

fn call_mcp_session(ctx: &TestContext, requests: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("pyre"))
        .arg("mcp")
        .current_dir(&ctx.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pyre mcp");

    {
        let stdin = child.stdin.as_mut().expect("expected child stdin");
        for request in requests {
            stdin
                .write_all(message_json(&request).as_bytes())
                .expect("failed to write MCP request");
        }
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("failed to wait for pyre mcp");
    assert!(
        output.status.success(),
        "pyre mcp failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("expected JSON response body"))
        .collect()
}

#[test]
fn initialize_ping_and_tools_list() {
    let ctx = TestContext::new();

    let initialize = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    );
    assert_eq!(initialize["result"]["serverInfo"]["name"], "pyre");
    assert!(initialize["result"]["capabilities"]["tools"].is_object());
    assert!(initialize["result"]["capabilities"]["resources"].is_object());

    let ping = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "ping"
        }),
    );
    assert_eq!(ping["result"], json!({}));

    let tools = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list"
        }),
    );
    let tool_names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert!(tool_names.contains(&"pyre_query"));
    assert!(tool_names.contains(&"pyre_preview_query"));
    assert!(tool_names.contains(&"pyre_explain_query"));
    assert!(tool_names.contains(&"pyre_db_status"));
    assert!(tool_names.contains(&"pyre_introspect"));
    assert!(tool_names.contains(&"pyre_schema"));
}

#[test]
fn initialize_negotiates_supported_protocol_version() {
    let ctx = TestContext::new();

    let initialize = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2099-01-01",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    );

    assert_eq!(initialize["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn one_mcp_session_handles_lifecycle_and_tool_call() {
    let ctx = TestContext::new();

    let responses = call_mcp_session(
        &ctx,
        vec![
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "pyre_docs",
                    "arguments": { "topic": "schema" }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "pyre");
    assert!(responses[1]["result"]["tools"].is_array());
    assert!(responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Pyre Schema Guide"));
}

#[test]
fn docs_are_exposed_as_resources() {
    let ctx = TestContext::new();

    let resources = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/list"
        }),
    );
    let resource_uris = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(resource_uris.contains(&"pyre://project/schema"));
    assert!(resource_uris.contains(&"pyre://guides/getting-started"));
    assert!(resource_uris.contains(&"pyre://guides/schema"));
    assert!(resource_uris.contains(&"pyre://guides/query"));
    assert!(resource_uris.contains(&"pyre://guides/namespacing"));
    assert!(resource_uris.contains(&"pyre://guides/mcp"));
    assert!(resource_uris.contains(&"pyre://guides/project-structure"));
    assert!(!resource_uris.contains(&"pyre://guides/simple"));
    assert!(!resource_uris.contains(&"pyre://guides/generated-crud"));
    assert!(resource_uris.contains(&"pyre://guides/serve"));
    assert!(resource_uris.contains(&"pyre://guides/troubleshooting"));

    let read = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/read",
            "params": {
                "uri": "pyre://guides/getting-started"
            }
        }),
    );
    assert_eq!(read["result"]["contents"][0]["mimeType"], "text/markdown");
    assert!(read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Pyre"));

    let schema_docs = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {
                "uri": "pyre://guides/schema"
            }
        }),
    );
    assert!(schema_docs["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Pyre Schema Guide"));

    let query_docs = call_mcp_tool(&ctx, "pyre_docs", json!({ "topic": "query" }));
    assert!(query_docs["content"]
        .as_str()
        .unwrap()
        .contains("Pyre Query Guide"));

    let serve_docs = call_mcp_tool(&ctx, "pyre_docs", json!({ "topic": "serve" }));
    assert!(serve_docs["content"]
        .as_str()
        .unwrap()
        .contains("`pyre serve`"));

    let mcp_docs = call_mcp_tool(&ctx, "pyre_docs", json!({ "topic": "mcp" }));
    assert!(mcp_docs["content"]
        .as_str()
        .unwrap()
        .contains("CLI To MCP Mapping"));

    let troubleshooting_docs =
        call_mcp_tool(&ctx, "pyre_docs", json!({ "topic": "troubleshooting" }));
    assert!(troubleshooting_docs["content"]
        .as_str()
        .unwrap()
        .contains("`Schema Not Found`"));
}

#[test]
fn introspect_tool_runs_cli_introspect() {
    let ctx = TestContext::new();
    let db_path = ctx.workspace_path.join("sample.db");

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = libsql::Builder::new_local(db_path.to_string_lossy().as_ref())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            (),
        )
        .await
        .unwrap();
    });

    let result = call_mcp_tool(
        &ctx,
        "pyre_introspect",
        json!({
            "database": "sample.db"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["command"][3], "introspect");
    assert!(result["stdout"]
        .as_str()
        .unwrap()
        .contains("Schema written to"));
}

#[test]
fn schema_is_exposed_as_resource() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    let read = call_mcp(
        &ctx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": {
                "uri": "pyre://project/schema"
            }
        }),
    );
    assert_eq!(
        read["result"]["contents"][0]["mimeType"],
        "application/json"
    );
    assert!(read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("record User"));
    assert!(read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("session {"));
}

#[test]
fn init_rejects_session_declaration_in_schema_source() {
    let ctx = TestContext::new();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_init",
        json!({
            "dir": "app-pyre",
            "schema": "session {\n    userId Int\n}\n\nrecord User {\n    id Int @id\n    @public\n}\n"
        }),
    );

    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("must not contain a session declaration"));
}

#[test]
fn init_creates_single_database_schema_from_required_source() {
    let ctx = TestContext::new();

    let result = call_mcp_tool(
        &ctx,
        "pyre_init",
        json!({
            "dir": "app-pyre",
            "schema": "record User {\n    id Int @id\n    name String\n    @public\n}\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["dir"], "app-pyre");
    assert_eq!(result["namespace"], "_default");
    assert_eq!(
        normalize_json_paths(&result["createdFiles"]),
        json!(["app-pyre/session.pyre", "app-pyre/schema.pyre"])
    );

    assert!(ctx.workspace_path.join("app-pyre/session.pyre").exists());
    let schema_path = ctx.workspace_path.join("app-pyre/schema.pyre");
    let schema = std::fs::read_to_string(schema_path).unwrap();
    assert!(schema.contains("record User"));
    assert!(schema.contains("name String"));
}

#[test]
fn init_accepts_shared_session_source() {
    let ctx = TestContext::new();

    let result = call_mcp_tool(
        &ctx,
        "pyre_init",
        json!({
            "dir": "app-pyre",
            "session": "session {\n    userId Int\n}\n",
            "schema": "record User {\n    id Int @id\n    ownerId Int\n    @allow(query) { ownerId == Session.userId }\n}\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(
        std::fs::read_to_string(ctx.workspace_path.join("app-pyre/session.pyre")).unwrap(),
        "session {\n    userId Int\n}\n"
    );
}

#[test]
fn init_creates_namespaced_schema() {
    let ctx = TestContext::new();

    let result = call_mcp_tool(
        &ctx,
        "pyre_init",
        json!({
            "dir": "multi-pyre",
            "namespace": "Billing",
            "schema": "record Invoice {\n    id Int @id\n    amount Int\n    @public\n}\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["namespace"], "Billing");
    assert_eq!(
        normalize_json_paths(&result["createdFiles"]),
        json!([
            "multi-pyre/session.pyre",
            "multi-pyre/schema/Billing/schema.pyre"
        ])
    );
    assert!(ctx.workspace_path.join("multi-pyre/session.pyre").exists());
    assert!(ctx
        .workspace_path
        .join("multi-pyre/schema/Billing/schema.pyre")
        .exists());
}

fn normalize_json_paths(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(path) => serde_json::Value::String(path.replace('\\', "/")),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(normalize_json_paths).collect())
        }
        _ => value.clone(),
    }
}

#[test]
fn init_rejects_invalid_schema_without_creating_directory() {
    let ctx = TestContext::new();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_init",
        json!({
            "dir": "bad-pyre",
            "schema": "record Broken {\n    id Nope\n}\n"
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    assert_eq!(
        response["error"]["data"]["message"],
        response["error"]["message"]
    );
    assert!(!ctx.workspace_path.join("bad-pyre").exists());
}

#[test]
fn init_rejects_unsafe_paths() {
    let ctx = TestContext::new();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_init",
        json!({
            "dir": "../outside",
            "schema": "record User {\n    id Int @id\n}\n"
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("must not contain"));
}

#[test]
fn database_tools_reject_unsafe_local_database_paths() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    for (tool, arguments) in [
        (
            "pyre_generate_migration",
            json!({
                "name": "bad",
                "database": "../outside.db"
            }),
        ),
        (
            "pyre_migrate",
            json!({
                "database": "../outside.db",
                "push": true
            }),
        ),
        (
            "pyre_db_status",
            json!({
                "database": "../outside.db"
            }),
        ),
        (
            "pyre_query",
            json!({
                "database": "../outside.db",
                "query": "query GetUsers { user { id } }"
            }),
        ),
    ] {
        let response = call_mcp_tool_error(&ctx, tool, arguments);
        assert_eq!(response["error"]["code"], -32603, "tool: {tool}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("must not contain"),
            "tool: {tool}, response: {response}"
        );
    }
}

#[test]
fn database_tools_allow_remote_and_env_database_refs() {
    let ctx = TestContext::new();

    for arguments in [
        json!({ "database": "libsql://example.turso.io", "auth": "token" }),
        json!({ "database": "$PYRE_TEST_DB" }),
    ] {
        let response = call_mcp_tool_error(&ctx, "pyre_db_status", arguments);
        assert!(!response.to_string().contains("must not contain"));
    }
}

#[test]
fn init_rejects_lowercase_namespace_without_exiting() {
    let ctx = TestContext::new();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_init",
        json!({
            "dir": "lowercase-pyre",
            "namespace": "billing",
            "schema": "record Invoice {\n    id Int @id\n}\n"
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("namespace must be capitalized"));
    assert!(!ctx.workspace_path.join("lowercase-pyre").exists());
}

#[test]
fn init_refuses_existing_directory() {
    let ctx = TestContext::new();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_init",
        json!({
            "dir": "pyre",
            "schema": "record User {\n    id Int @id\n}\n"
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Directory already exists"));
}

#[test]
fn init_can_create_and_migrate_local_database() {
    let ctx = TestContext::new();

    let result = call_mcp_tool(
        &ctx,
        "pyre_init",
        json!({
            "dir": "db-pyre",
            "schema": "record User {\n    id Int @id\n    name String\n    @public\n}\n",
            "database": "pyre.db"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["database"]["path"], "pyre.db");
    assert_eq!(result["database"]["migrated"], true);
    assert!(ctx.workspace_path.join("pyre.db").exists());
}

#[test]
fn init_refuses_existing_database_without_creating_schema_directory() {
    let ctx = TestContext::new();
    std::fs::write(ctx.workspace_path.join("pyre.db"), "already here").unwrap();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_init",
        json!({
            "dir": "db-conflict-pyre",
            "schema": "record User {\n    id Int @id\n}\n",
            "database": "pyre.db"
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Database already exists"));
    assert!(!ctx.workspace_path.join("db-conflict-pyre").exists());
}

#[test]
fn parse_error_and_notification_no_response() {
    let ctx = TestContext::new();

    let mut parse_error_child = StdCommand::new(assert_cmd::cargo::cargo_bin("pyre"))
        .arg("mcp")
        .current_dir(&ctx.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pyre mcp");
    parse_error_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\n")
        .unwrap();
    drop(parse_error_child.stdin.take());
    let parse_error_output = parse_error_child.wait_with_output().unwrap();
    let parse_error = parse_message_json(&parse_error_output.stdout);
    assert_eq!(parse_error["error"]["code"], -32700);

    let mut notification_child = StdCommand::new(assert_cmd::cargo::cargo_bin("pyre"))
        .arg("mcp")
        .current_dir(&ctx.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pyre mcp");
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    notification_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(message_json(&notification).as_bytes())
        .unwrap();
    drop(notification_child.stdin.take());
    let notification_output = notification_child.wait_with_output().unwrap();
    assert!(notification_output.stdout.is_empty());
}

#[test]
fn schema_and_check_tools() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    let schema = call_mcp_tool(&ctx, "pyre_schema", json!({}));
    assert_eq!(schema["schemas"][0]["namespace"], "_default");
    assert!(schema["schemas"][0]["content"]
        .as_str()
        .unwrap()
        .contains("record User"));

    let check = call_mcp_tool(&ctx, "pyre_check", json!({}));
    assert_eq!(check["ok"], true);
    assert_eq!(check["stdout"], json!("[]\n"));
}

#[test]
fn db_status_reports_up_to_date_after_push() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let status = call_mcp_tool(&ctx, "pyre_db_status", json!({ "database": ".yak/yak.db" }));

    assert_eq!(status["ok"], true);
    assert_eq!(status["accessible"], true);
    assert_eq!(status["status"], "up_to_date");
    assert_eq!(status["pendingMigrations"], json!([]));
}

#[test]
fn pyre_query_executes_selection_and_mutation() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let insert = call_mcp_tool(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "insert CreateUser { user { name = \"Ada\" } }",
            "params": {}
        }),
    );
    assert_eq!(insert["ok"], true);
    assert_eq!(insert["results"][0]["operation"], "insert");

    let select = call_mcp_tool(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query GetUsers { user { id name } }",
            "params": {}
        }),
    );

    assert_eq!(select["ok"], true);
    assert_eq!(select["results"][0]["operation"], "query");
    assert_eq!(select["results"][0]["response"]["user"][0]["name"], "Ada");
}

#[test]
fn pyre_preview_query_returns_generated_sql_without_database() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    let preview = call_mcp_tool(
        &ctx,
        "pyre_preview_query",
        json!({
            "query": "query UserByName($name: String) {\n    user {\n        @where { name == $name }\n        id\n        name\n    }\n}"
        }),
    );

    assert_eq!(preview["ok"], true);
    assert_eq!(preview["results"][0]["name"], "UserByName");
    assert_eq!(preview["results"][0]["operation"], "query");
    assert_eq!(
        preview["results"][0]["inputSchema"]["name"]["type"],
        "String"
    );
    assert!(preview["results"][0]["sql"][0]["sql"]
        .as_str()
        .unwrap()
        .contains("$name"));
}

#[test]
fn pyre_explain_query_uses_real_params_and_database_plan() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let explain = call_mcp_tool(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query UserByName($name: String) {\n    user {\n        @where { name == $name }\n        id\n        name\n    }\n}",
            "params": { "name": "Ada" }
        }),
    );

    assert_eq!(explain["ok"], true);
    assert_eq!(explain["results"][0]["name"], "UserByName");
    assert_eq!(explain["results"][0]["operation"], "query");
    assert_eq!(
        explain["results"][0]["statements"][0]["values"],
        json!(["Ada"])
    );
    assert!(explain["results"][0]["statements"][0]["sql"]
        .as_str()
        .unwrap()
        .contains('?'));
    assert!(!explain["results"][0]["statements"][0]["plan"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn pyre_explain_query_does_not_execute_insert_update_or_delete() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let explain_insert = call_mcp_tool(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "insert CreateUser($name: String) { user { name = $name } }",
            "params": { "name": "Ada" }
        }),
    );
    assert_eq!(explain_insert["ok"], true);

    let empty_after_explain_insert = call_mcp_tool(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query GetUsers { user { id name } }",
            "params": {}
        }),
    );
    assert_eq!(
        empty_after_explain_insert["results"][0]["response"]["user"],
        json!([])
    );

    let insert = call_mcp_tool(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "insert CreateUser($name: String) { user { name = $name } }",
            "params": { "name": "Ada" }
        }),
    );
    assert_eq!(insert["ok"], true);

    let explain_update = call_mcp_tool(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "update RenameUser($id: Int, $name: String) { user { @where { id == $id } name = $name } }",
            "params": { "id": 1, "name": "Grace" }
        }),
    );
    assert_eq!(explain_update["ok"], true);

    let explain_delete = call_mcp_tool(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "delete DeleteUser($id: Int) { user { @where { id == $id } } }",
            "params": { "id": 1 }
        }),
    );
    assert_eq!(explain_delete["ok"], true);

    let unchanged = call_mcp_tool(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query GetUsers { user { id name } }",
            "params": {}
        }),
    );
    assert_eq!(
        unchanged["results"][0]["response"]["user"][0]["name"],
        "Ada"
    );
    assert_eq!(
        unchanged["results"][0]["response"]["user"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pyre_explain_query_uses_session_values_without_executing_query() {
    let ctx = TestContext::new();
    write_session_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let explain = call_mcp_tool(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query MyNotes { note { @where { ownerId == Session.userId } id body } }",
            "params": {},
            "session": { "userId": 7 }
        }),
    );

    assert_eq!(explain["ok"], true);
    assert_eq!(explain["results"][0]["statements"][0]["values"], json!([7]));

    let missing_session = call_mcp_tool_error(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query MyNotes { note { @where { ownerId == Session.userId } id body } }",
            "params": {}
        }),
    );
    assert_eq!(missing_session["error"]["code"], -32603);
    assert!(missing_session["error"]["message"]
        .as_str()
        .unwrap()
        .contains("session"));
}

#[test]
fn pyre_explain_query_accepts_nullable_params() {
    let ctx = TestContext::new();
    write_nullable_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let explain = call_mcp_tool(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query UserByNickname($nickname: String?) { user { @where { nickname == $nickname } id nickname } }",
            "params": { "nickname": null }
        }),
    );

    assert_eq!(explain["ok"], true);
    assert_eq!(
        explain["results"][0]["statements"][0]["values"],
        json!([null])
    );
}

#[test]
fn pyre_explain_query_reports_invalid_parameters_like_query() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_explain_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query UserByName($name: String) {\n    user {\n        @where { name == $name }\n        id\n        name\n    }\n}",
            "params": { "name": 42 }
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    let message = response["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("invalid input") || message.contains("expected"),
        "unexpected error message: {}",
        message
    );
}

#[test]
fn pyre_query_reports_mutation_typecheck_failure() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "insert BadUser { user { name = 1 } }",
            "params": {}
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("typecheck"));
}

#[test]
fn pyre_query_reports_invalid_parameters() {
    let ctx = TestContext::new();
    write_schema(&ctx);

    ctx.run_command("migrate")
        .arg(".yak/yak.db")
        .arg("--push")
        .assert()
        .success();

    let response = call_mcp_tool_error(
        &ctx,
        "pyre_query",
        json!({
            "database": ".yak/yak.db",
            "query": "query UserByName($name: String) {\n    user {\n        @where { name == $name }\n        id\n        name\n    }\n}",
            "params": { "name": 42 }
        }),
    );

    assert_eq!(response["error"]["code"], -32603);
    let message = response["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("invalid input") || message.contains("expected"),
        "unexpected error message: {}",
        message
    );
}
