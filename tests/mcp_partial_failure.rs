use pyre::server::{
    manifest::{Manifest, PyreSession},
    query,
};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("pyre")).unwrap();
    std::fs::write(dir.path().join("pyre/session.pyre"), "session {}\n").unwrap();
    std::fs::write(
        dir.path().join("pyre/schema.pyre"),
        "record User {\n id Int @id\n name String\n @public\n}\n",
    )
    .unwrap();
    assert_cmd::Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migrate", "test.db", "--push"])
        .assert()
        .success();
    dir
}

fn call(dir: &TempDir, source: &str, params: Value) -> Value {
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("pyre"))
        .current_dir(dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "pyre_query", "arguments": {
            "database": "test.db", "query": source, "params": params,
            "auth": "private-auth-token", "session": {"unused": "private-session-value"}
        }}
    });
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn tool_value(response: &Value) -> Value {
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected tool result: {response}")),
    )
    .unwrap()
}

#[tokio::test]
async fn committed_results_survive_later_database_failure() {
    let dir = workspace();
    let db = libsql::Builder::new_local(dir.path().join("test.db"))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("CREATE UNIQUE INDEX unique_name ON users(name)", ())
        .await
        .unwrap();

    let response = call(
        &dir,
        r#"
        insert First { user { name = "Ada" } }
        insert Duplicate { user { name = "Ada" } }
        insert NotRun { user { name = "Grace" } }
    "#,
        json!({}),
    );
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], true);
    let value = tool_value(&response);
    assert_eq!(value["ok"], false);
    assert_eq!(value["failedQuery"], "Duplicate");
    assert_eq!(value["results"].as_array().unwrap().len(), 1);
    assert_eq!(value["results"][0]["name"], "First");
    assert_eq!(value["results"][0]["operation"], "insert");
    assert!(value["results"][0].get("response").is_some());
    assert!(value["results"][0].get("affectedRows").is_some());
    let error = value["error"].as_str().unwrap();
    assert!(
        error.contains("query 'Duplicate': execute SQL statement"),
        "{error}"
    );
    assert!(error.contains("UNIQUE constraint failed"), "{error}");
    let mut rows = conn.query("SELECT name FROM users", ()).await.unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "Ada"
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[test]
fn completed_read_survives_later_input_failure() {
    let dir = workspace();
    let response = call(
        &dir,
        r#"
        query First { user { id name } }
        insert MissingInput($name: String) { user { name = $name } }
    "#,
        json!({}),
    );
    assert_eq!(response["result"]["isError"], true);
    let value = tool_value(&response);
    assert_eq!(value["ok"], false);
    assert_eq!(value["failedQuery"], "MissingInput");
    assert_eq!(value["results"][0]["response"]["user"], json!([]));
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("missing input field 'name'"));
}

#[tokio::test]
async fn first_failure_keeps_json_rpc_error_and_does_not_expose_values() {
    let dir = workspace();
    let db = libsql::Builder::new_local(dir.path().join("test.db"))
        .build()
        .await
        .unwrap();
    db.connect()
        .unwrap()
        .execute("DROP TABLE users", ())
        .await
        .unwrap();
    for source in [
        "insert Failed($name: String) { user { name = $name } }",
        "insert Failed($name: String) { user { name = $name } } query NotRun($name: String) { user { @where { name == $name } id } }",
    ] {
        let response = call(&dir, source, json!({"name": "private-parameter-value"}));
        assert!(response.get("result").is_none(), "{response}");
        assert_eq!(response["error"]["code"], -32603);
        let error = response["error"]["message"].as_str().unwrap();
        assert!(error.contains("query 'Failed': execute SQL statement"), "{error}");
        for secret in ["private-parameter-value", "private-auth-token", "private-session-value"] {
            assert!(!error.contains(secret), "{error}");
        }
    }
}

#[tokio::test]
async fn runtime_reports_statement_index_and_transaction_stages() {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("CREATE TABLE items (id INTEGER PRIMARY KEY)", ())
        .await
        .unwrap();
    let mut manifest: Manifest = serde_json::from_value(json!({
        "version": 1, "session_schema": {}, "queries": {"Test": {
            "id": "Test", "operation": "insert", "input_schema": {},
            "session_args": [], "optional_input_args": [], "json_input_args": [],
            "sql": [
                {"include": false, "params": [], "sql": "INSERT INTO items VALUES (1)"},
                {"include": false, "params": [], "sql": "INSERT INTO items VALUES (1)"}
            ]
        }}
    }))
    .unwrap();
    let session = PyreSession::new(json!({}), &manifest.session_schema).unwrap();
    let error = query::run(&conn, &manifest, "Test", json!({}), &session)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("execute SQL statement 2 (1-based)"),
        "{error}"
    );
    let mut rows = conn.query("SELECT COUNT(*) FROM items", ()).await.unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);

    let tx = conn.transaction().await.unwrap();
    let error = query::run(&conn, &manifest, "Test", json!({}), &session)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("begin transaction: database error"),
        "{error}"
    );
    tx.rollback().await.unwrap();

    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    conn.execute(
        "CREATE TABLE children (parent INTEGER REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED)",
        (),
    )
    .await
    .unwrap();
    let statements = &mut manifest.queries.get_mut("Test").unwrap().sql;
    statements.truncate(1);
    statements[0].sql = "INSERT INTO children VALUES (99)".to_string();
    let error = query::run(&conn, &manifest, "Test", json!({}), &session)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("commit transaction: database error"),
        "{error}"
    );
}
