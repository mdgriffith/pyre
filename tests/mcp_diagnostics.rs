use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

const SCHEMA: &str = "record User {\n    id Int @id\n    name String\n    @public\n}\n";

fn project(schema: &str, session: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("pyre")).unwrap();
    std::fs::write(dir.path().join("pyre/schema.pyre"), schema).unwrap();
    std::fs::write(dir.path().join("pyre/session.pyre"), session).unwrap();
    dir
}

fn preview_error(dir: &TempDir, query: &str) -> String {
    tool_error(dir, "pyre_preview_query", json!({"query": query}))
}

fn tool_error(dir: &TempDir, name: &str, arguments: Value) -> String {
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("pyre"))
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    });
    // A second request proves invalid input does not terminate the server.
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{request}").unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
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
    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2, "{stdout}");
    assert_eq!(responses[1]["id"], 2);
    responses[0]["error"]["message"]
        .as_str()
        .expect(&stdout)
        .to_string()
}

#[test]
fn mcp_discovery_rejects_missing_or_non_directory_project_roots() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("not-a-directory"), "").unwrap();
    for tool in ["pyre_project_info", "pyre_schema", "pyre_preview_query"] {
        for (root, diagnostic) in [
            ("missing-project", "Failed to inspect project directory"),
            ("not-a-directory", "Project path is not a directory"),
        ] {
            let mut arguments = json!({"dir": root});
            if tool == "pyre_preview_query" {
                arguments["query"] = json!("query Users { user { id } }");
            }
            let error = tool_error(&dir, tool, arguments);
            assert!(error.contains(root), "{tool}: {error}");
            assert!(error.contains(diagnostic), "{tool}: {error}");
            assert!(!error.contains('\u{1b}'), "{tool}: {error}");
        }
    }
}

#[test]
fn mcp_parse_diagnostics_include_source_without_stderr() {
    for (schema, session, path, excerpt) in [
        (
            "record User {\n    id ???\n}\n",
            "session {\n}\n",
            "schema.pyre",
            "id ???",
        ),
        (
            SCHEMA,
            "session {\n    userId ???\n}\n",
            "session.pyre",
            "userId ???",
        ),
    ] {
        let dir = project(schema, session);
        let error = preview_error(&dir, "query Users { user { id } }");
        assert!(error.contains(path), "{error}");
        assert!(error.contains(excerpt), "{error}");
        assert!(!error.contains("Failed to parse"), "{error}");
        assert!(!error.contains('\u{1b}'), "{error}");
    }
}

#[test]
fn mcp_typecheck_diagnostics_render_query_and_schema_sources() {
    let dir = project(SCHEMA, "session {\n}\n");
    let error = preview_error(
        &dir,
        "query Users {\n    user {\n        missingField\n    }\n}\n",
    );
    assert!(error.contains("mcp.pyre"), "{error}");
    assert!(error.contains("missingField"), "{error}");
    assert!(error.contains('^'), "{error}");
    assert!(!error.contains("error_type:"), "{error}");

    std::fs::write(
        dir.path().join("pyre/schema.pyre"),
        SCHEMA.replace("String", "BogusType"),
    )
    .unwrap();
    let error = preview_error(&dir, "query Users { user { id } }");
    assert!(error.contains("schema.pyre"), "{error}");
    assert!(error.contains("name BogusType"), "{error}");
    assert!(error.contains('^'), "{error}");
    assert!(!error.contains("error_type:"), "{error}");
}

#[test]
fn lowercase_namespace_returns_diagnostic_without_exiting_mcp() {
    let dir = project(SCHEMA, "session {\n}\n");
    std::fs::create_dir_all(dir.path().join("pyre/schema/lowercase")).unwrap();
    std::fs::rename(
        dir.path().join("pyre/schema.pyre"),
        dir.path().join("pyre/schema/lowercase/schema.pyre"),
    )
    .unwrap();
    let error = preview_error(&dir, "query Users { user { id } }");
    assert!(error.contains("schema/lowercase/schema.pyre"), "{error}");
    assert!(
        error.contains("This schema name must be capitalized: lowercase"),
        "{error}"
    );
    assert!(!error.contains('\u{1b}'), "{error}");
}

#[test]
fn cli_parse_errors_print_once_with_real_line_breaks() {
    let dir = project("record User {\n    id ???\n}\n", "session {\n}\n");
    for command in ["check", "generate", "format"] {
        let output = Command::new(assert_cmd::cargo::cargo_bin("pyre"))
            .arg(command)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stderr.matches("id ???").count(), 1, "{command}: {stderr}");
        assert!(!stderr.contains("Failed to parse"), "{stderr}");
        assert!(!stderr.contains("\\n"), "{stderr}");
    }
}
