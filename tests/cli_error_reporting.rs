use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("pyre")).unwrap();
    std::fs::write(dir.path().join("pyre/session.pyre"), "session {\n}\n").unwrap();
    std::fs::write(
        dir.path().join("pyre/schema.pyre"),
        "@syncable(false)\nrecord User {\n    id Int @id\n    name String\n    @public\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn database_commands_fail_on_connection_errors() {
    let dir = workspace();
    for args in [
        vec!["migrate", "$PYRE_TEST_MISSING_DATABASE"],
        vec!["migrate", "$PYRE_TEST_MISSING_DATABASE", "--push"],
        vec!["introspect", "$PYRE_TEST_MISSING_DATABASE"],
        vec!["db", "init", "$PYRE_TEST_MISSING_DATABASE", "--disposable"],
    ] {
        let output = Command::cargo_bin("pyre")
            .unwrap()
            .current_dir(dir.path())
            .env_remove("PYRE_TEST_MISSING_DATABASE")
            .args(args)
            .assert()
            .failure()
            .get_output()
            .clone();
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostic.contains("Unknown Environment Variable"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("PYRE_TEST_MISSING_DATABASE"),
            "{diagnostic}"
        );
    }
}

#[tokio::test]
async fn database_commands_fail_on_introspection_errors() {
    let dir = workspace();
    let db = libsql::Builder::new_local(dir.path().join("broken.db"))
        .build()
        .await
        .unwrap();
    let conn = std::mem::ManuallyDrop::new(db.connect().unwrap());
    conn.execute("CREATE TABLE _pyre_migrations (wrong_column TEXT)", ())
        .await
        .unwrap();
    pyre::db::policy::set_disposable(&conn, None).await.unwrap();
    for args in [
        vec!["migrate", "broken.db", "--push"],
        vec!["introspect", "broken.db"],
    ] {
        Command::cargo_bin("pyre")
            .unwrap()
            .current_dir(dir.path())
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("Failed to introspect database"));
    }
}

#[test]
fn format_invalid_query_fails_without_overwriting_or_success_summary() {
    let dir = workspace();
    let source = "query Broken {";
    let path = dir.path().join("pyre/broken.pyre");
    std::fs::write(&path, source).unwrap();
    for args in [
        vec!["format", "pyre/broken.pyre"],
        vec!["format", "pyre/broken.pyre", "--to-stdout"],
        vec!["format"],
    ] {
        Command::cargo_bin("pyre")
            .unwrap()
            .current_dir(dir.path())
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("Failed to parse query"))
            .stdout(predicate::str::contains("Formatted").not());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
    for args in [vec!["format"], vec!["format", "pyre/broken.pyre"]] {
        Command::cargo_bin("pyre")
            .unwrap()
            .current_dir(dir.path())
            .args(args)
            .write_stdin(source)
            .assert()
            .failure()
            .stderr(predicate::str::contains("Failed to parse query"))
            .stdout("");
    }
}

#[test]
fn migration_sql_failure_identifies_file() {
    let dir = workspace();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migration", "create", "broken", "--draft"])
        .assert()
        .success();
    let folder = dir.path().join("pyre/migrations/_default/broken");
    std::fs::write(folder.join("migration.sql"), "THIS IS NOT SQL;").unwrap();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migration", "check", "broken"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("migration.sql statement 1"))
        .stderr(predicate::str::contains("syntax error"));
}

#[test]
fn migration_generation_rejects_non_directory_and_allows_parallel_drafts() {
    let dir = workspace();
    std::fs::write(dir.path().join("not-a-directory"), "").unwrap();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args([
            "migration",
            "create",
            "initial",
            "--migration-dir",
            "not-a-directory",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Not a directory"));
    for name in ["first", "second"] {
        Command::cargo_bin("pyre")
            .unwrap()
            .current_dir(dir.path())
            .args(["migration", "create", name, "--draft"])
            .assert()
            .success();
        assert!(!dir
            .path()
            .join(format!("pyre/migrations/_default/{name}/manifest.json"))
            .exists());
    }
}

#[test]
fn migration_generation_accepts_missing_migration_directory() {
    let dir = workspace();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migration", "create", "initial"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_dir(dir.path().join("pyre/migrations"))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn destructive_push_requires_force_and_preserves_unrelated_rows() {
    let dir = workspace();
    let path = dir.path().join("pyre/schema.pyre");
    let keep = "record Keep {\n    id Int @id\n    name String\n    @public\n}\n";
    let initial = format!("@syncable(false)\nrecord User {{\n    id Int @id\n    name String\n    @public\n}}\nrecord Message {{\n    id Int @id\n    body String\n    @public\n}}\n{keep}");
    std::fs::write(&path, &initial).unwrap();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["db", "init", "test.db", "--disposable"])
        .assert()
        .success();

    let db = libsql::Builder::new_local(dir.path().join("test.db"))
        .build()
        .await
        .unwrap();
    let conn = std::mem::ManuallyDrop::new(db.connect().unwrap());
    conn.execute_batch("INSERT INTO keeps (id, name) VALUES (1, 'retained'); INSERT INTO users (id, name) VALUES (1, 'old');").await.unwrap();
    std::fs::write(&path, format!("@syncable(false)\n{keep}")).unwrap();

    // Piped input must not silently authorize a destructive push.
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migrate", "test.db", "--push"])
        .write_stdin("y\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "The following tables will be dropped:\n  - messages\n  - users",
        ))
        .stderr(predicate::str::contains("This is a destructive change!"))
        .stderr(predicate::str::contains("--force"));
    let mut rows = conn.query("SELECT count(*) FROM users", ()).await.unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    drop(rows);
    let mut rows = conn
        .query(pyre::db::introspect::GET_SCHEMA, ())
        .await
        .unwrap();
    assert!(rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
        .contains("record User"));
    drop(rows);

    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migrate", "test.db", "--push", "--force"])
        .assert()
        .success();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('users', 'messages')",
            (),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    let mut rows = conn
        .query("SELECT name FROM keeps WHERE id=1", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "retained"
    );
    drop(rows);
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migrate", "test.db", "--push"])
        .assert()
        .success();
}

#[test]
fn force_does_not_bypass_unsupported_column_changes() {
    let dir = workspace();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["db", "init", "test.db", "--disposable"])
        .assert()
        .success();
    std::fs::write(
        dir.path().join("pyre/schema.pyre"),
        "@syncable(false)\nrecord User {\n    id Int @id\n    name Int\n    @public\n}\n",
    )
    .unwrap();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migrate", "test.db", "--push", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Column User.name needs an explicit SQL migration",
        ))
        .stderr(predicate::str::contains("Type: String -> Int"))
        .stderr(predicate::str::contains("No migration SQL was executed"));
}

#[test]
fn serve_refuses_an_unmarked_target_without_creating_missing_paths() {
    let dir = workspace();
    let database = dir.path().join("empty.db");
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .env("PYRE_SCHEMA_DATABASE", &database)
        .args(["serve", "$PYRE_SCHEMA_DATABASE"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            database.to_string_lossy().as_ref(),
        ));
    assert!(!database.exists());
    std::fs::write(&database, []).unwrap();
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["serve", database.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Serving a preserved database requires finalized provenance",
        ));
}

#[cfg(unix)]
#[test]
fn migration_generation_propagates_directory_read_errors() {
    use std::os::unix::fs::PermissionsExt;

    let dir = workspace();
    let migrations = dir.path().join("pyre/migrations/_default");
    std::fs::create_dir_all(&migrations).unwrap();
    std::fs::set_permissions(&migrations, std::fs::Permissions::from_mode(0o300)).unwrap();
    // Privileged users can bypass directory permissions.
    if std::fs::read_dir(&migrations).is_ok() {
        std::fs::set_permissions(&migrations, std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let output = Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(dir.path())
        .args(["migration", "create", "initial"])
        .output()
        .unwrap();
    std::fs::set_permissions(&migrations, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("pyre/migrations"), "{diagnostic}");
    assert!(
        diagnostic.to_lowercase().contains("permission denied"),
        "{diagnostic}"
    );
    assert_eq!(std::fs::read_dir(&migrations).unwrap().count(), 0);
}
