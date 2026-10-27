use assert_cmd::Command;
use pyre::ast::{ExternalIdentity, IdentityKind};
use pyre::server::schema::{self, EnsureDatabaseOutcome};
use std::path::Path;

fn project(root: &Path, app: &str, auth: &str, session: Option<&str>) {
    for (namespace, source) in [("App", app), ("Auth", auth)] {
        let directory = root.join("pyre/schema").join(namespace);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("schema.pyre"), source).unwrap();
    }
    std::fs::write(
        root.join("pyre/session.pyre"),
        session.unwrap_or("session {\n}\n"),
    )
    .unwrap();
}

fn migrate(root: &Path, mode: &str) {
    if mode == "files" {
        Command::cargo_bin("pyre")
            .unwrap()
            .current_dir(root)
            .args([
                "migration",
                "upgrade",
                "--db",
                "app.db",
                "--namespace",
                "App",
            ])
            .assert()
            .success();
    }
    let mut command = Command::cargo_bin("pyre").unwrap();
    command
        .current_dir(root)
        .args(["migrate", "app.db", "--namespace", "App"]);
    if mode == "push" {
        command.arg("--push");
    }
    command.assert().success();
}

#[tokio::test]
async fn legacy_external_scalar_snapshots_upgrade_without_rebuilding_columns() {
    for (kind, scalar, value) in [
        (
            IdentityKind::Uuid,
            "String",
            "'01900000-0000-7000-8000-000000000001'",
        ),
        (IdentityKind::Int, "Int", "42"),
    ] {
        for mode in ["runtime", "push", "files"] {
            let root = tempfile::tempdir().unwrap();
            let db = libsql::Builder::new_local(root.path().join("app.db"))
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            let legacy = format!("@syncable(false)\nrecord Note {{\n    @public\n    id Id.Uuid @id\n    ownerId {scalar}\n}}\n");
            schema::ensure_database(&conn, "App", &legacy)
                .await
                .unwrap();
            conn.execute_batch(&format!("insert into notes (id, ownerId) values ('01900000-0000-7000-8000-000000000002', {value}); drop table _pyre_external_identities; alter table _pyre_migrations drop column schema_namespace;")).await.unwrap();
            let current = legacy.replace(&format!("ownerId {scalar}"), "ownerId Auth.User.id");
            let identity = ExternalIdentity {
                namespace: "Auth".into(),
                record: "User".into(),
                field: "id".into(),
                kind,
            };
            if mode == "runtime" {
                assert_eq!(
                    schema::ensure_database_with_external_identities(
                        &conn,
                        "App",
                        &current,
                        &[identity.clone()],
                    )
                    .await
                    .unwrap(),
                    EnsureDatabaseOutcome::Migrated
                );
            } else {
                let id = match kind {
                    IdentityKind::Uuid => "Id.Uuid",
                    IdentityKind::Int => "Id.Int",
                };
                project(
                    root.path(),
                    &current,
                    &format!(
                        "@syncable(false)\nrecord User {{\n    @public\n    id {id} @id\n}}\n"
                    ),
                    None,
                );
                migrate(root.path(), mode);
            }
            let loaded = schema::load_schema_from_database(&conn).await.unwrap();
            assert_eq!(
                loaded.context().unwrap().external_identities,
                vec![identity.clone()]
            );
            let row = conn
                .query("select ownerId from notes", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            match kind {
                IdentityKind::Uuid => assert_eq!(
                    row.get::<String>(0).unwrap(),
                    "01900000-0000-7000-8000-000000000001"
                ),
                IdentityKind::Int => assert_eq!(row.get::<i64>(0).unwrap(), 42),
            }
            // Compatibility with legacy annotations must not permit a real
            // TEXT/INTEGER conversion without an explicit SQL migration.
            let changed = ExternalIdentity {
                kind: match kind {
                    IdentityKind::Uuid => IdentityKind::Int,
                    IdentityKind::Int => IdentityKind::Uuid,
                },
                ..identity
            };
            assert!(schema::ensure_database_with_external_identities(
                &conn,
                "App",
                &current,
                &[changed],
            )
            .await
            .is_err());
        }
    }
}

#[tokio::test]
async fn retired_external_session_identities_do_not_block_migrations() {
    for mode in [
        "runtime",
        "push",
        "files",
        "no-pending-files",
        "applied-files",
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(root.path().join("app.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let app = "record Note {\n    @public\n    id Id.Uuid @id\n}\n";
        let old = format!("session {{\n    ownerId Auth.User.id?\n}}\n{app}");
        let identity = ExternalIdentity {
            namespace: "Auth".into(),
            record: "User".into(),
            field: "id".into(),
            kind: IdentityKind::Uuid,
        };
        schema::ensure_database_with_external_identities(&conn, "App", &old, &[identity])
            .await
            .unwrap();
        conn.execute(
            "insert into notes (id) values ('01900000-0000-7000-8000-000000000001')",
            (),
        )
        .await
        .unwrap();
        if mode == "applied-files" {
            project(
                root.path(),
                app,
                "record User {\n    @public\n    id Id.Uuid @id\n}\n",
                Some("session {\n    ownerId Auth.User.id?\n}\n"),
            );
            migrate(root.path(), "files");
        }
        let session = "session {\n    isAdmin Bool\n}\n";
        let current = format!("{session}{app}");
        if mode == "runtime" {
            // Historical metadata may resolve the old snapshot, but may not
            // make the retired identity valid in a newly authored schema.
            assert!(
                schema::ensure_database_with_external_identities(&conn, "App", &old, &[])
                    .await
                    .is_err()
            );
            schema::ensure_database_with_external_identities(&conn, "App", &current, &[])
                .await
                .unwrap();
        } else {
            project(
                root.path(),
                app,
                "record Account {\n    @public\n    id Id.Uuid @id\n}\n",
                Some(session),
            );
            Command::cargo_bin("pyre")
                .unwrap()
                .current_dir(root.path())
                .arg("check")
                .assert()
                .success();
            migrate(root.path(), mode);
        }
        let loaded = schema::load_schema_from_database(&conn).await.unwrap();
        let context = loaded.context().unwrap();
        assert!(context.external_identities.is_empty());
        let fields = pyre::ast::collect_columns(&context.session.as_ref().unwrap().fields);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "isAdmin");
        let row = conn
            .query("select count(*) from notes", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 1);
    }
}

#[tokio::test]
async fn renamed_external_identity_fields_resolve_old_snapshots_until_refresh() {
    for mode in [
        "runtime",
        "push",
        "files",
        "no-pending-files",
        "applied-files",
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(root.path().join("app.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let app = "record Note {\n    @public\n    id Id.Uuid @id\n}\n";
        let old_session = "session {\n    userId Auth.User.id?\n}\n";
        let old = format!("{old_session}{app}");
        let old_identity = ExternalIdentity {
            namespace: "Auth".into(),
            record: "User".into(),
            field: "id".into(),
            kind: IdentityKind::Uuid,
        };
        schema::ensure_database_with_external_identities(
            &conn,
            "App",
            &old,
            &[old_identity.clone()],
        )
        .await
        .unwrap();
        conn.execute(
            "insert into notes (id) values ('01900000-0000-7000-8000-000000000001')",
            (),
        )
        .await
        .unwrap();
        if mode == "applied-files" {
            project(
                root.path(),
                app,
                "record User {\n    @public\n    id Id.Uuid @id\n}\n",
                Some(old_session),
            );
            migrate(root.path(), "files");
        }
        let session = old_session.replace("Auth.User.id", "Auth.User.key");
        let current = format!("{session}{app}");
        let identity = ExternalIdentity {
            field: "key".into(),
            ..old_identity.clone()
        };
        // Historical metadata may resolve the old snapshot, but must not make
        // its retired field valid in a newly authored schema.
        assert!(schema::ensure_database_with_external_identities(
            &conn,
            "App",
            &old,
            &[identity.clone()],
        )
        .await
        .is_err());
        let unchanged = schema::load_schema_from_database(&conn).await.unwrap();
        assert_eq!(
            unchanged.context().unwrap().external_identities,
            vec![old_identity]
        );
        if mode == "runtime" {
            assert_eq!(
                schema::ensure_database_with_external_identities(
                    &conn,
                    "App",
                    &current,
                    &[identity.clone()],
                )
                .await
                .unwrap(),
                EnsureDatabaseOutcome::Migrated
            );
        } else {
            project(
                root.path(),
                app,
                "record User {\n    @public\n    key Id.Uuid @id\n}\n",
                Some(&session),
            );
            migrate(root.path(), mode);
        }
        let loaded = schema::load_schema_from_database(&conn).await.unwrap();
        let context = loaded.context().unwrap();
        assert_eq!(context.external_identities, vec![identity]);
        let fields = pyre::ast::collect_columns(&context.session.as_ref().unwrap().fields);
        assert_eq!(fields[0].type_.to_string(), "Auth.User.key");
        let row = conn
            .query("select count(*) from notes", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 1);
    }
}

#[tokio::test]
async fn no_pending_migrations_refresh_required_identities_and_preserve_history() {
    let root = tempfile::tempdir().unwrap();
    let db = libsql::Builder::new_local(root.path().join("app.db"))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let app = "record Note {\n    @public\n    id Id.Uuid @id\n    ownerId Auth.User.id\n}\n";
    let auth = "record User {\n    @public\n    id Id.Uuid @id\n}\nrecord Account {\n    @public\n    id Id.Uuid @id\n}\n";
    project(root.path(), app, auth, None);
    migrate(root.path(), "files");
    conn.execute(
        "insert into notes (id, ownerId) values (?, ?)",
        libsql::params![
            "01900000-0000-7000-8000-000000000001",
            "01900000-0000-7000-8000-000000000002"
        ],
    )
    .await
    .unwrap();
    let history_sql =
        "select json_group_array(json_array(id, name, sql, finished_at)) from _pyre_migrations";
    let history: String = conn
        .query(history_sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    project(
        root.path(),
        app,
        auth,
        Some("session {\n    accountId Auth.Account.id?\n}\n"),
    );
    migrate(root.path(), "no-pending-files");
    let loaded = schema::load_schema_from_database(&conn).await.unwrap();
    let source = pyre::db::migrate::schema_to_storage_string(
        loaded.context().unwrap(),
        loaded.schema().unwrap(),
    );
    assert!(source.contains("accountId Auth.Account.id?"));
    assert_eq!(loaded.context().unwrap().external_identities.len(), 2);
    assert!(loaded
        .context()
        .unwrap()
        .external_identities
        .iter()
        .any(|identity| identity.record == "Account"));
    let updated_history: String = conn
        .query(history_sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(history, updated_history);

    // A logical identity change still needs an explicit migration even though
    // both columns use TEXT. Failure must roll back the attempted refresh.
    project(
        root.path(),
        &app.replace("ownerId Auth.User.id", "ownerId Auth.Account.id"),
        auth,
        Some("session {\n    isAdmin Bool\n}\n"),
    );
    Command::cargo_bin("pyre")
        .unwrap()
        .current_dir(root.path())
        .args(["migrate", "app.db", "--namespace", "App"])
        .assert()
        .failure();
    let after = schema::load_schema_from_database(&conn).await.unwrap();
    assert_eq!(
        pyre::db::migrate::schema_to_storage_string(
            after.context().unwrap(),
            after.schema().unwrap()
        ),
        source
    );
    assert_eq!(
        after.context().unwrap().external_identities,
        loaded.context().unwrap().external_identities
    );
    let count: i64 = conn
        .query("select count(*) from notes", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn local_identity_qualification_does_not_require_a_physical_migration() {
    for mode in ["runtime", "push", "files", "no-pending-files"] {
        let root = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(root.path().join("app.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let old = "record User {\n    @public\n    id Id.Uuid @id\n}\nrecord Account {\n    @public\n    id Id.Uuid @id\n}\nrecord Note {\n    @public\n    id Id.Uuid @id\n    ownerId User.id\n    owners Json<List<User.id>>\n}\n";
        schema::ensure_database(&conn, "App", old).await.unwrap();
        conn.execute(
            "insert into notes (id, ownerId, owners) values (?, ?, jsonb(?))",
            libsql::params![
                "01900000-0000-7000-8000-000000000001",
                "01900000-0000-7000-8000-000000000002",
                "[]"
            ],
        )
        .await
        .unwrap();
        let current = old.replace("User.id", "App.User.id");
        if mode == "runtime" {
            schema::ensure_database(&conn, "App", &current)
                .await
                .unwrap();
            // Equal SQLite scalar types must not erase distinct identity brands.
            let changed = current.replace("ownerId App.User.id", "ownerId App.Account.id");
            assert!(schema::ensure_database(&conn, "App", &changed)
                .await
                .is_err());
        } else {
            project(
                root.path(),
                &current,
                "record AuthUser {\n    @public\n    id Id.Uuid @id\n}\n",
                None,
            );
            migrate(root.path(), mode);
        }
        let loaded = schema::load_schema_from_database(&conn).await.unwrap();
        let source = pyre::db::migrate::schema_to_storage_string(
            loaded.context().unwrap(),
            loaded.schema().unwrap(),
        );
        assert!(source.contains("ownerId App.User.id"));
        let row = conn
            .query("select ownerId from notes", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get::<String>(0).unwrap(),
            "01900000-0000-7000-8000-000000000002"
        );
    }
}
