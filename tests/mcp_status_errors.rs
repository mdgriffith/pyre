use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

const SCHEMA: &str = "record User {\n    id Int @id\n    name String\n    @public\n}\n";

fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("pyre")).unwrap();
    std::fs::write(dir.path().join("pyre/schema.pyre"), SCHEMA).unwrap();
    std::fs::write(dir.path().join("pyre/session.pyre"), "session {\n}\n").unwrap();
    dir
}

fn call(dir: &TempDir, name: &str, arguments: Value) -> Value {
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("pyre"))
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "ping"
        })
    )
    .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["id"], 2);
    assert!(responses[1].get("result").is_some());
    responses[0].clone()
}

fn status(dir: &TempDir) -> Value {
    let response = call(dir, "pyre_db_status", json!({"database": "test.db"}));
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect(&response.to_string()),
    )
    .unwrap()
}

#[test]
fn missing_migration_directory_is_allowed() {
    let dir = workspace();
    let result = status(&dir);
    assert_eq!(result["ok"], true);
    assert_eq!(result["pendingMigrations"], json!([]));
    assert_eq!(result["schema"]["checked"], true);
}

#[test]
fn migration_directory_errors_include_operation_and_path() {
    let dir = workspace();
    std::fs::write(dir.path().join("pyre/migrations"), "not a directory").unwrap();
    let response = call(&dir, "pyre_db_status", json!({"database": "test.db"}));
    let error = response["error"]["message"].as_str().unwrap();
    assert!(
        error.contains("Failed to read migrations in pyre/migrations:"),
        "{error}"
    );
    assert!(error.to_lowercase().contains("not a directory"), "{error}");
}

#[tokio::test]
async fn invalid_stored_schema_returns_unknown_with_stored_source_diagnostics() {
    for (source, failure, excerpt) in [
        (
            "record Stored {\n    id ???\n}\n".to_string(),
            "parse",
            "id ???",
        ),
        (
            SCHEMA.replace("name String", "storedName BogusType"),
            "typecheck",
            "storedName BogusType",
        ),
    ] {
        let dir = workspace();
        let db = libsql::Builder::new_local(dir.path().join("test.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("CREATE TABLE _pyre_migrations (id INTEGER, name TEXT, schema TEXT, finished_at TEXT, error TEXT);")
            .await.unwrap();
        conn.execute(
            "INSERT INTO _pyre_migrations VALUES (1, 'initial', ?1, '2026-01-01', NULL)",
            [source],
        )
        .await
        .unwrap();
        for pending in [false, true] {
            if pending {
                std::fs::create_dir_all(dir.path().join("pyre/migrations/pending")).unwrap();
            }
            let result = status(&dir);
            assert_eq!(result["ok"], true);
            assert_eq!(result["accessible"], true);
            assert_eq!(result["status"], "unknown", "{result}");
            assert_eq!(result["schema"]["checked"], false);
            assert!(result["schema"].get("diff").is_none());
            assert!(result["schema"].get("upToDate").is_none());
            assert_eq!(result["appliedMigrations"], json!(["initial"]));
            assert_eq!(
                result["pendingMigrations"],
                if pending {
                    json!(["pending"])
                } else {
                    json!([])
                }
            );
            let error = result["schema"]["error"].as_str().unwrap();
            assert!(error.contains(&format!("failed to {failure}")), "{error}");
            assert!(error.contains("schema.pyre"), "{error}");
            assert!(error.contains(excerpt), "{error}");
            assert!(error.contains('^'), "{error}");
            assert!(!error.contains('\u{1b}'), "{error}");
        }
    }
}

#[cfg(unix)]
#[test]
fn migration_directory_permission_errors_are_not_ignored() {
    use std::os::unix::fs::PermissionsExt;

    let dir = workspace();
    let path = dir.path().join("pyre/migrations");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o300)).unwrap();
    // Privileged users can bypass directory permissions.
    if std::fs::read_dir(&path).is_ok() {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let response = call(&dir, "pyre_db_status", json!({"database": "test.db"}));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = response["error"]["message"].as_str().unwrap();
    assert!(
        error.contains("Failed to read migrations in pyre/migrations:"),
        "{error}"
    );
    assert!(
        error.to_lowercase().contains("permission denied"),
        "{error}"
    );
}

#[test]
fn init_directory_error_includes_operation_and_path() {
    let dir = workspace();
    std::fs::write(dir.path().join("blocked"), "not a directory").unwrap();
    let response = call(
        &dir,
        "pyre_init",
        json!({"dir": "blocked/project", "schema": SCHEMA}),
    );
    let error = response["error"]["message"].as_str().unwrap();
    assert!(
        error.contains("Failed to create directory blocked/project:"),
        "{error}"
    );
}
