#![cfg(feature = "json")]

use pyre::ast::{ExternalIdentity, IdentityKind};
use pyre::db::bundle::{
    Bundle, BundleError, BundleHash, BundleMetadata, DraftMetadata, History, HistoryError,
    DEFAULT_GRACE_SECONDS, MIGRATION_PATH, SCHEMA_PATH,
};
use pyre::schema_contract::ApiChange;
use serde_json::{json, Value};

const SCHEMA: &str = "@syncable(false)\nrecord Note {\n    @public\n    id Id.Int @id\n}\n";

fn metadata(predecessor: Option<&BundleHash>) -> BundleMetadata {
    BundleMetadata {
        namespace: "App".into(),
        predecessor: predecessor.cloned(),
        target_api_version: "0.0.0".into(),
        grace_seconds: None,
        external_identities: vec![],
    }
}

fn file(path: &str, text: &str) -> (String, Vec<u8>) {
    (path.into(), text.as_bytes().to_vec())
}

fn files(sql: &str) -> Vec<(String, Vec<u8>)> {
    vec![file(MIGRATION_PATH, sql), file(SCHEMA_PATH, SCHEMA)]
}

fn seal(predecessor: Option<&BundleHash>, sql: &str) -> Bundle {
    Bundle::seal(metadata(predecessor), files(sql)).unwrap()
}

fn manifest(bundle: &Bundle) -> Value {
    serde_json::from_slice(&bundle.manifest_json()).unwrap()
}

#[test]
fn finalized_versions_distinguish_storage_additions_and_breaking_changes() {
    let compile = |previous, schema: &str, sql: &str| {
        Bundle::finalize(
            DraftMetadata {
                namespace: "App".into(),
                grace_seconds: Some(60),
                external_identities: vec![],
            },
            previous,
            [file(SCHEMA_PATH, schema), file(MIGRATION_PATH, sql)],
        )
        .unwrap()
    };
    let (root, initial) = compile(None, SCHEMA, "");
    assert_eq!(initial.from, None);
    assert_eq!(initial.to.to_string(), "0.0.0");

    let (data_only, data) = compile(Some(&root), SCHEMA, "delete from notes;");
    assert_eq!(data.change, ApiChange::Unchanged);
    assert_eq!(data.to.to_string(), "0.0.0");
    assert_eq!(
        data_only.runtime_fingerprint().unwrap(),
        root.runtime_fingerprint().unwrap()
    );

    let indexed = SCHEMA.replace("@id", "@id @index");
    let (storage_only, storage) = compile(Some(&data_only), &indexed, "");
    assert_eq!(storage.change, ApiChange::Unchanged);
    assert_ne!(
        storage_only.runtime_fingerprint().unwrap(),
        root.runtime_fingerprint().unwrap()
    );

    let additive = format!(
        "{}    label String?\n}}\n\ntype State = Open\n",
        indexed.trim_end().strip_suffix('}').unwrap()
    );
    let (minor, addition) = compile(Some(&storage_only), &additive, "");
    assert_eq!(addition.change, ApiChange::Minor);
    assert_eq!(addition.to.to_string(), "0.1.0");

    let (major, union) = compile(
        Some(&minor),
        &additive.replace("= Open", "= Open\n    | Closed"),
        "",
    );
    assert_eq!(union.change, ApiChange::Major);
    assert_eq!(union.to.to_string(), "1.0.0");
    assert!(History::new("App", [major, minor, root, storage_only, data_only]).is_ok());
}

#[test]
fn runtime_fingerprint_ignores_layout_but_includes_runtime_semantics() {
    let compile = |namespace: &str, source: &str, kind| {
        pyre::db::bundle::load_standalone_schema(
            namespace,
            source,
            &[ExternalIdentity {
                namespace: "Auth".into(),
                record: "User".into(),
                field: "id".into(),
                kind,
            }],
        )
        .map(|context| pyre::schema_contract::runtime_fingerprint(&context))
        .unwrap()
    };
    let source = concat!(
        "@syncable(false)\n",
        "record Note {\n @public\n id Id.Int @id\n owner Auth.User.id\n}\n",
        "type State = Open\n    | Closed\n"
    );
    let reordered = concat!(
        "@syncable(false)\n",
        "// comment\ntype State = Closed\n   | Open\n\n",
        "record Note {\n owner Auth.User.id\n id Id.Int @id\n @public\n}\n"
    );
    let digest = compile("App", source, IdentityKind::Int);
    assert_eq!(digest, compile("App", reordered, IdentityKind::Int));
    assert_ne!(digest, compile("Another", source, IdentityKind::Int));
    assert_ne!(digest, compile("App", source, IdentityKind::Uuid));
    assert_ne!(
        digest,
        compile(
            "App",
            &source.replace("@public", "@allow(*) { False }"),
            IdentityKind::Int
        )
    );
    assert_eq!(
        digest,
        "v1:e4ce330eb396d6fd317520559a323a8b1d3f3fc6bbdce2cff539f20d96abf6e5"
    );
}

#[test]
fn history_rejects_resealed_artifacts_with_unearned_versions() {
    let root = seal(None, "");
    let mut metadata = metadata(Some(root.hash()));
    metadata.target_api_version = "0.1.0".into();
    let child = Bundle::seal(metadata, files("")).unwrap();
    assert!(matches!(
        History::new("App", [root, child]),
        Err(HistoryError::ApiVersion {
            source: BundleError::WrongTargetApiVersion { .. },
            ..
        })
    ));
}

#[cfg(feature = "database")]
mod execution {
    use super::*;
    use pyre::db::bundle::{
        author,
        database::DatabaseCatalog,
        execute::{self, Application, Binding, Error, Inspection},
        sql,
    };

    const SYNCED: &str = "record Note {\n @public\n id Id.Uuid @id\n text String\n }\n";

    async fn scalar(conn: &libsql::Connection, sql: &str) -> String {
        conn.query(sql, ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap()
    }

    async fn root(source: &str) -> History {
        let empty = History::new("App", []).unwrap();
        let generated = author::create_draft(&empty, source.into(), vec![], None)
            .await
            .unwrap();
        assert!(!generated.requires_manual_sql);
        let (bundle, _) = generated.draft.candidate(&empty).unwrap();
        author::check(&empty, Some(&bundle), None).await.unwrap();
        History::new("App", [bundle]).unwrap()
    }

    async fn exercise(db: &libsql::Database) {
        let conn = db.connect().unwrap();
        let root = root(SYNCED).await;
        let binding = Binding::Database;
        execute::initialize(&conn, &root, binding).await.unwrap();
        assert!(matches!(
            execute::initialize(&conn, &root, binding).await,
            Err(Error::NotApplied(_))
        ));
        let catalog = DatabaseCatalog::new(&conn);
        assert_eq!(
            catalog.history("App").await.unwrap().head().unwrap().hash(),
            root.head().unwrap().hash()
        );
        conn.execute(
            "insert into notes(id, text, updatedAt) values ('01900000-0000-7000-8000-000000000001', 'before', 0)",
            (),
        )
        .await
        .unwrap();
        let epoch = scalar(&conn, "select database_epoch from _pyre_sync").await;

        let mut draft = author::create_draft(&root, SYNCED.into(), vec![], None)
            .await
            .unwrap()
            .draft;
        draft.sql = "-- change rows; preserve exact bytes\nupdate notes set text = 'after;still a string';\n".into();
        draft.assertions.insert(
            "no_broken_rows.sql".into(),
            "select 1 from notes where text != 'after;still a string'".into(),
        );
        catalog.write_draft("rewrite", &draft, None).await.unwrap();
        let finalized = catalog.finalize("App", "rewrite", None).await.unwrap();
        let history = catalog.history("App").await.unwrap();
        assert!(matches!(
            execute::apply_history(&conn, &history, Binding::Filesystem).await,
            Err(Error::NotApplied(_))
        ));
        assert_eq!(scalar(&conn, "select text from notes").await, "before");
        let outcome = execute::apply_history(&conn, &history, binding)
            .await
            .unwrap();
        assert!(matches!(outcome[1], Application::Applied(_)));
        assert_eq!(
            scalar(&conn, "select text from notes").await,
            "after;still a string"
        );
        assert_ne!(
            scalar(&conn, "select database_epoch from _pyre_sync").await,
            epoch
        );
        let fresh = db.connect().unwrap();
        assert!(matches!(
            execute::inspect_outcome(&fresh, &finalized, binding)
                .await
                .unwrap(),
            Inspection::Applied(_)
        ));
        assert!(matches!(
            execute::apply_history(&fresh, &history, binding)
                .await
                .unwrap()[1],
            Application::AlreadyApplied(_)
        ));

        let mut draft = author::create_draft(&history, SYNCED.into(), vec![], None)
            .await
            .unwrap()
            .draft;
        draft.sql = "update notes set text = 'bad';".into();
        draft
            .assertions
            .insert("populated.sql".into(), "select 1 from notes".into());
        catalog
            .write_draft("assertion-fails", &draft, None)
            .await
            .unwrap();
        let finalized = catalog
            .finalize("App", "assertion-fails", None)
            .await
            .unwrap();
        let history = catalog.history("App").await.unwrap();
        assert!(
            matches!(execute::apply_history(&conn, &history, binding).await, Err(Error::RolledBack(reason)) if reason.contains("populated.sql"))
        );
        assert_eq!(
            scalar(&conn, "select text from notes").await,
            "after;still a string"
        );
        assert_eq!(
            execute::inspect_outcome(&fresh, &finalized, binding)
                .await
                .unwrap(),
            Inspection::NotApplied
        );
    }

    #[tokio::test]
    async fn executes_real_rows_and_reconciles_receipts_after_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(dir.path().join("app.db"))
            .build()
            .await
            .unwrap();
        exercise(&db).await;
    }

    #[tokio::test]
    async fn remote_libsql_executes_the_same_transactional_workflow() {
        let Ok(url) = std::env::var("PYRE_TEST_MIGRATION_SQLD_URL") else {
            return;
        };
        let db = libsql::Builder::new_remote(url, String::new())
            .build()
            .await
            .unwrap();
        exercise(&db).await;
    }

    #[test]
    fn rejects_sql_outside_the_transactional_and_read_only_boundary() {
        for sql_text in [
            "begin immediate",
            "pragma foreign_keys = off",
            "update `_pyre_sync` set id = 1",
            "create table temp.new_table (id integer)",
            "insert or rollback into notes values (1)",
            "attach database 'other.db' as other",
        ] {
            let err = sql::plan(&seal(None, sql_text)).unwrap_err();
            assert!(
                err.to_string().contains("migration.sql statement 1"),
                "{err}"
            );
        }
        let mut payload = files("");
        payload.push(file(
            "assertions/mutation.sql",
            "with candidate as (select 1) delete from notes where id in candidate returning id",
        ));
        let bundle = Bundle::seal(metadata(None), payload).unwrap();
        assert!(sql::plan(&bundle)
            .unwrap_err()
            .to_string()
            .contains("read-only SELECT"));
    }

    #[tokio::test]
    async fn baseline_requires_exact_structure_and_promotion_preserves_rows() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let history = root(SCHEMA).await;
        let head = history.head().unwrap();
        let binding = Binding::Filesystem;
        pyre::db::lifecycle::init_disposable(&conn, "App", SCHEMA, &[])
            .await
            .unwrap();
        conn.execute("insert into notes(id) values (7)", ())
            .await
            .unwrap();
        assert!(execute::promote(&conn, &history, binding).await.is_err());
        conn.execute("alter table notes add column extra text", ())
            .await
            .unwrap();
        assert!(execute::baseline(&conn, &history, head.hash(), binding)
            .await
            .is_err());
        assert!(execute::installed(&conn).await.unwrap().is_none());
        conn.execute("alter table notes drop column extra", ())
            .await
            .unwrap();
        execute::baseline(&conn, &history, head.hash(), binding)
            .await
            .unwrap();
        assert!(matches!(
            execute::apply_history(&conn, &history, binding)
                .await
                .unwrap()[0],
            Application::CoveredByBaseline
        ));
        assert_eq!(
            scalar(
                &conn,
                "select cast(count(*) as text) from _pyre_migration_receipts"
            )
            .await,
            "0"
        );
        assert_eq!(
            execute::promote(&conn, &history, binding).await.unwrap(),
            pyre::db::policy::Policy::Preserved
        );
        assert_eq!(
            scalar(&conn, "select cast(id as text) from notes").await,
            "7"
        );
        assert_eq!(
            pyre::server::schema::ensure_database(&conn, "App", SCHEMA)
                .await
                .unwrap(),
            pyre::server::schema::EnsureDatabaseOutcome::UpToDate
        );
        assert!(pyre::server::schema::ensure_database(
            &conn,
            "App",
            &SCHEMA.replace("@public", "@allow(*) { False }")
        )
        .await
        .is_err());
    }
}

#[cfg(all(
    feature = "database",
    feature = "filesystem",
    feature = "serve",
    feature = "lsp"
))]
#[test]
fn cli_finalizes_checks_seeds_applies_and_audits_without_the_working_schema() {
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path();
    std::fs::create_dir(work.join("pyre")).unwrap();
    std::fs::write(work.join("pyre/session.pyre"), "session {}\n").unwrap();
    std::fs::write(
        work.join("pyre/schema.pyre"),
        "@syncable(false)\nrecord Note {\n @public\n id Id.Int @id\n text String\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut command = assert_cmd::Command::cargo_bin("pyre").unwrap();
        command.current_dir(work).args(args).assert()
    };
    run(&["migration", "create", "z_root"]).success();
    run(&["db", "init", "app.db"]).success();
    run(&["migration", "create", "a_rewrite", "--draft"]).success();
    let draft = work.join("pyre/migrations/_default/a_rewrite");
    std::fs::write(
        draft.join("migration.sql"),
        "update notes set text = 'after';\n",
    )
    .unwrap();
    std::fs::create_dir(draft.join("assertions")).unwrap();
    std::fs::write(
        draft.join("assertions/rewrite.sql"),
        "select 1 from notes where text != 'after';",
    )
    .unwrap();
    std::fs::write(
        work.join("fixtures.json"),
        r#"{"notes":[{"id":1,"text":"before"}]}"#,
    )
    .unwrap();
    run(&[
        "migration",
        "check",
        "a_rewrite",
        "--fixtures",
        "fixtures.json",
    ])
    .success();
    run(&["migration", "finalize", "a_rewrite"]).success();
    std::fs::remove_file(work.join("pyre/schema.pyre")).unwrap();
    run(&["migrate", "app.db"])
        .success()
        .stdout(predicates::str::contains("Applied 1"));
    run(&["db", "audit", "app.db"]).success();
    run(&["migration", "unfinalize", "a_rewrite"])
        .failure()
        .stderr(predicates::str::contains("evidence"));
}

#[cfg(all(
    feature = "database",
    feature = "filesystem",
    feature = "serve",
    feature = "lsp"
))]
#[test]
fn cli_database_catalog_bootstrap_does_not_read_or_create_filesystem_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path();
    std::fs::create_dir(work.join("pyre")).unwrap();
    std::fs::write(work.join("pyre/session.pyre"), "session {}\n").unwrap();
    std::fs::write(work.join("pyre/schema.pyre"), SCHEMA).unwrap();
    std::fs::write(
        work.join("pyre/migrations"),
        "intentionally not a directory",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut command = assert_cmd::Command::cargo_bin("pyre").unwrap();
        command.current_dir(work).args(args).assert()
    };
    run(&["db", "init", "app.db", "--migration-catalog", "database"]).success();
    run(&["db", "init", "app.db", "--migration-catalog", "database"]).failure();
    run(&["migration", "check", "--database", "app.db"]).success();
    run(&["migrate", "app.db"]).success();
    assert_eq!(
        std::fs::read_to_string(work.join("pyre/migrations")).unwrap(),
        "intentionally not a directory"
    );
}

#[test]
fn canonical_format_has_a_stable_digest_and_preserves_exact_bytes() {
    let mut payload = files("");
    payload.push(file("assertions/check.sql", "select 1;\r\n"));
    let mut metadata = metadata(None);
    metadata.target_api_version = "1.0.0".into();
    let bundle = Bundle::seal(metadata, payload.clone()).unwrap();
    assert_eq!(
        bundle.hash().to_string(),
        "f94a373541ca25b363b7fec259c64a576ac96bbbb18a21c5183100bdcbd34323"
    );
    assert_eq!(bundle.migration_sql(), "");
    assert_eq!(bundle.schema_source(), SCHEMA);
    assert_eq!(bundle.grace_seconds(), DEFAULT_GRACE_SECONDS);
    assert_eq!(
        bundle.assertions().collect::<Vec<_>>(),
        [("check.sql", "select 1;\r\n")]
    );

    let reordered = format!(
        "{{\n  \"targetApiVersion\": \"1.0.0\", \"predecessor\": null, \"namespace\": \"App\", \"hash\": \"{}\", \"format\": 1, \"externalIdentities\": [], \"graceSeconds\": 86400\n}}",
        bundle.hash()
    );
    payload.reverse();
    let read = Bundle::verify("App", reordered.as_bytes(), payload).unwrap();
    assert_eq!(read.hash(), bundle.hash());
    assert_eq!(read.manifest_json(), bundle.manifest_json());
}

#[test]
fn identities_are_canonical_and_supply_the_standalone_schema() {
    let schema = "@syncable(false)\nrecord Note {\n    @public\n    id Id.Int @id\n    ownerId Auth.User.id\n    teamId Groups.Team.id\n}\n";
    let payload = || vec![file(MIGRATION_PATH, ""), file(SCHEMA_PATH, schema)];
    assert!(matches!(
        Bundle::seal(metadata(None), payload()),
        Err(BundleError::Schema(_))
    ));
    let mut metadata = metadata(None);
    metadata.external_identities = vec![
        ExternalIdentity {
            namespace: "Groups".into(),
            record: "Team".into(),
            field: "id".into(),
            kind: IdentityKind::Int,
        },
        ExternalIdentity {
            namespace: "Auth".into(),
            record: "User".into(),
            field: "id".into(),
            kind: IdentityKind::Uuid,
        },
    ];
    metadata.grace_seconds = Some(7_200);
    metadata.target_api_version = "1.2.3".into();
    let bundle = Bundle::seal(metadata, payload()).unwrap();
    let mut wire = manifest(&bundle);
    assert_eq!(wire["externalIdentities"][0]["namespace"], "Auth");
    assert_eq!(wire["externalIdentities"][1]["namespace"], "Groups");
    wire["externalIdentities"].as_array_mut().unwrap().reverse();
    let verified = Bundle::verify("App", &serde_json::to_vec(&wire).unwrap(), payload()).unwrap();
    assert_eq!(verified.manifest_json(), bundle.manifest_json());
    assert_eq!(verified.grace_seconds(), 7_200);
    assert_eq!(verified.target_api_version(), "1.2.3");
    let context = verified.load_target_schema().unwrap();
    assert_eq!(context.database.schemas.len(), 1);
    assert_eq!(context.database.schemas[0].namespace, "App");

    let mut identity_tamper = wire.clone();
    identity_tamper["externalIdentities"][0]["kind"] = json!("Id.Uuid");
    assert!(matches!(
        Bundle::verify(
            "App",
            &serde_json::to_vec(&identity_tamper).unwrap(),
            payload()
        ),
        Err(BundleError::HashMismatch { .. })
    ));
    let mut identity_extra = wire.clone();
    identity_extra["externalIdentities"][0]["extra"] = json!("unhashed");
    assert!(matches!(
        Bundle::verify(
            "App",
            &serde_json::to_vec(&identity_extra).unwrap(),
            payload()
        ),
        Err(BundleError::Json(_))
    ));
    let duplicate = wire["externalIdentities"][0].clone();
    wire["externalIdentities"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(matches!(
        Bundle::verify("App", &serde_json::to_vec(&wire).unwrap(), payload()),
        Err(BundleError::DuplicateIdentity { .. })
    ));
}

#[test]
fn detects_payload_and_manifest_tampering() {
    let mut payload = files("select 1;\n");
    payload.push(file("assertions/check.sql", "select 2;\r\n"));
    let bundle = Bundle::seal(metadata(None), payload.clone()).unwrap();
    let manifest_bytes = bundle.manifest_json();
    for (index, replacement) in [
        (0, file(MIGRATION_PATH, "select 1;\r\n")),
        (1, file(SCHEMA_PATH, &format!("{SCHEMA}\n"))),
        (2, file("assertions/renamed.sql", "select 2;\r\n")),
        (2, file("assertions/check.sql", "select 2;\n")),
    ] {
        let mut changed = payload.clone();
        changed[index] = replacement;
        assert!(matches!(
            Bundle::verify("App", &manifest_bytes, changed),
            Err(BundleError::HashMismatch { .. })
        ));
    }
    assert!(matches!(
        Bundle::verify("App", &manifest_bytes, files("select 1;\n")),
        Err(BundleError::HashMismatch { .. })
    ));
    for (key, value) in [
        ("targetApiVersion", json!("1.0.1")),
        ("predecessor", json!("1".repeat(64))),
        ("graceSeconds", json!(60)),
        ("hash", json!("2".repeat(64))),
    ] {
        let mut changed = manifest(&bundle);
        changed[key] = value;
        assert!(
            matches!(
                Bundle::verify(
                    "App",
                    &serde_json::to_vec(&changed).unwrap(),
                    payload.clone()
                ),
                Err(BundleError::HashMismatch { .. })
            ),
            "changed {key}"
        );
    }
    assert!(matches!(
        Bundle::verify("Another", &manifest_bytes, payload),
        Err(BundleError::NamespaceMismatch { .. })
    ));
}

#[test]
fn rejects_malformed_manifest_and_payload_layout() {
    let bundle = seal(None, "");
    let verify =
        |wire: Value| Bundle::verify("App", &serde_json::to_vec(&wire).unwrap(), files(""));
    assert!(matches!(
        verify(json!([1, "App", bundle.hash().to_string(), null, "1.0.0"])),
        Err(BundleError::ManifestNotObject)
    ));
    let repeated = String::from_utf8(bundle.manifest_json()).unwrap().replacen(
        "\"format\":1",
        "\"format\":1,\"format\":1",
        1,
    );
    assert!(matches!(
        Bundle::verify("App", repeated.as_bytes(), files("")),
        Err(BundleError::Json(_))
    ));
    for required in ["format", "namespace", "predecessor", "targetApiVersion"] {
        let mut wire = manifest(&bundle);
        wire.as_object_mut().unwrap().remove(required);
        assert!(
            matches!(verify(wire), Err(BundleError::Json(_))),
            "missing {required}"
        );
    }
    let mut wire = manifest(&bundle);
    wire.as_object_mut().unwrap().remove("hash");
    assert!(matches!(verify(wire), Err(BundleError::MissingHash)));
    for (field, invalid) in [
        ("hash", json!("xyz")),
        ("graceSeconds", Value::Null),
        ("other", json!(123)),
    ] {
        let mut wire = manifest(&bundle);
        wire[field] = invalid;
        assert!(
            matches!(verify(wire), Err(BundleError::Json(_))),
            "invalid {field}"
        );
    }
    let mut wire = manifest(&bundle);
    wire["format"] = json!(2);
    assert!(matches!(
        verify(wire),
        Err(BundleError::UnsupportedFormat(2))
    ));
    let mut wire = manifest(&bundle);
    wire["graceSeconds"] = json!(0);
    assert!(matches!(verify(wire), Err(BundleError::ZeroGrace)));
    for name in [
        "schema.diff",
        "assertions/.sql",
        "assertions/../check.sql",
        "assertions/x\\y.sql",
        "assertions/nested/check.sql",
    ] {
        let mut payload = files("");
        payload.push(file(name, ""));
        assert!(
            matches!(
                Bundle::seal(metadata(None), payload),
                Err(BundleError::UnexpectedPath(_))
            ),
            "invalid path {name}"
        );
    }
    assert!(matches!(
        Bundle::seal(metadata(None), [file(SCHEMA_PATH, SCHEMA)]),
        Err(BundleError::MissingPath(MIGRATION_PATH))
    ));
    assert!(matches!(
        Bundle::seal(metadata(None), [file(MIGRATION_PATH, "")]),
        Err(BundleError::MissingPath(SCHEMA_PATH))
    ));
    let mut payload = files("");
    payload.push(file(MIGRATION_PATH, ""));
    assert!(matches!(
        Bundle::seal(metadata(None), payload),
        Err(BundleError::DuplicatePath(_))
    ));
    let mut payload = files("");
    payload[0].1 = vec![0xff];
    assert!(matches!(
        Bundle::seal(metadata(None), payload),
        Err(BundleError::InvalidUtf8(_))
    ));
}

#[test]
fn rejects_missing_predecessors_forks_multiple_roots_duplicates_and_wrong_scope() {
    let root = seal(None, "");
    let left = seal(Some(root.hash()), "-- left");
    let right = seal(Some(root.hash()), "-- right");
    assert!(matches!(
        History::new("App", [left.clone()]),
        Err(HistoryError::MissingPredecessor { .. })
    ));
    assert!(matches!(
        History::new("App", [left, root.clone(), right]),
        Err(HistoryError::Fork { .. })
    ));
    assert!(matches!(
        History::new("App", [root.clone(), seal(None, "-- other root")]),
        Err(HistoryError::MultipleRoots { .. })
    ));
    assert!(matches!(
        History::new("App", [root.clone(), root.clone()]),
        Err(HistoryError::DuplicateBundle(_))
    ));
    assert!(matches!(
        History::new("Another", [root]),
        Err(HistoryError::NamespaceMismatch { .. })
    ));
}

#[cfg(feature = "filesystem")]
mod filesystem {
    use super::*;
    use pyre::db::bundle::{
        filesystem::{Error, FilesystemCatalog},
        MANIFEST_PATH,
    };
    use std::{fs, path::Path};

    fn store(path: &Path, bundle: &Bundle) {
        fs::create_dir_all(path).unwrap();
        for (relative, contents) in bundle.files() {
            let file = path.join(relative);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }
        fs::write(path.join(MANIFEST_PATH), bundle.manifest_json()).unwrap();
    }

    #[test]
    fn round_trips_in_chain_order_and_ignores_drafts_and_other_namespaces() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = FilesystemCatalog::new(directory.path());
        assert!(catalog.read_history("App").unwrap().head().is_none());
        assert!(matches!(
            catalog.read_history(".."),
            Err(Error::History(HistoryError::InvalidNamespace(_)))
        ));
        let root = seal(None, "");
        let mut payload = files("-- child\r\n");
        payload.extend([
            file("assertions/z.sql", "select 2;"),
            file("assertions/a.sql", "select 1;"),
        ]);
        let child = Bundle::seal(metadata(Some(root.hash())), payload).unwrap();
        let scope = directory.path().join("App");
        store(&scope.join("z_root"), &root);
        store(&scope.join("a_child"), &child);
        fs::create_dir_all(scope.join("draft")).unwrap();
        fs::write(scope.join("draft/migration.sql"), "draft").unwrap();
        let wrong_scope = directory.path().join("Other/copy");
        store(&wrong_scope, &child);

        let history = catalog.read_history("App").unwrap();
        assert_eq!(history.namespace(), "App");
        assert_eq!(
            history
                .bundles()
                .iter()
                .map(Bundle::hash)
                .collect::<Vec<_>>(),
            [root.hash(), child.hash()]
        );
        let head = history.head().unwrap();
        assert_eq!(head.manifest_json(), child.manifest_json());
        assert_eq!(head.files(), child.files());
        assert_eq!(
            head.assertions().map(|(name, _)| name).collect::<Vec<_>>(),
            ["a.sql", "z.sql"]
        );
        assert!(matches!(
            catalog.read_history("Other"),
            Err(Error::Bundle {
                source: BundleError::NamespaceMismatch { .. },
                ..
            })
        ));

        fs::remove_dir_all(scope.join("z_root")).unwrap();
        assert!(matches!(
            catalog.read_history("App"),
            Err(Error::History(HistoryError::MissingPredecessor { .. }))
        ));
    }

    #[test]
    fn finalized_directories_reject_extra_files_and_changed_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = FilesystemCatalog::new(directory.path());
        let path = directory.path().join("App/root");
        store(&path, &seal(None, ""));
        fs::write(path.join("schema.diff"), "legacy diff").unwrap();
        assert!(matches!(catalog.read_history("App"), Err(Error::Io { .. })));
        fs::remove_file(path.join("schema.diff")).unwrap();
        fs::write(path.join(MIGRATION_PATH), "-- substituted").unwrap();
        assert!(matches!(
            catalog.read_history("App"),
            Err(Error::Bundle {
                source: BundleError::HashMismatch { .. },
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_payload_even_if_it_resolves_to_matching_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = FilesystemCatalog::new(directory.path());
        let path = directory.path().join("App/root");
        store(&path, &seal(None, ""));
        fs::remove_file(path.join(MIGRATION_PATH)).unwrap();
        let outside = directory.path().join("other.sql");
        fs::write(&outside, "").unwrap();
        std::os::unix::fs::symlink(outside, path.join(MIGRATION_PATH)).unwrap();
        assert!(matches!(catalog.read_history("App"), Err(Error::Io { .. })));
    }
}
