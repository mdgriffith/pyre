#![cfg(feature = "json")]

use pyre::ast::{ExternalIdentity, IdentityKind};
use pyre::db::bundle::{
    Bundle, BundleError, BundleHash, BundleMetadata, History, HistoryError, DEFAULT_GRACE_SECONDS,
    MIGRATION_PATH, SCHEMA_PATH,
};
use serde_json::{json, Value};

const SCHEMA: &str = "@syncable(false)\nrecord Note {\n    @public\n    id Id.Int @id\n}\n";

fn metadata(predecessor: Option<&BundleHash>) -> BundleMetadata {
    BundleMetadata {
        namespace: "App".into(),
        predecessor: predecessor.cloned(),
        target_api_version: "1.0.0".into(),
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
fn canonical_format_has_a_stable_digest_and_preserves_exact_bytes() {
    let mut payload = files("");
    payload.push(file("assertions/check.sql", "select 1;\r\n"));
    let bundle = Bundle::seal(metadata(None), payload.clone()).unwrap();
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
    metadata.target_api_version = "1.2.3+exact".into();
    let bundle = Bundle::seal(metadata, payload()).unwrap();
    let mut wire = manifest(&bundle);
    assert_eq!(wire["externalIdentities"][0]["namespace"], "Auth");
    assert_eq!(wire["externalIdentities"][1]["namespace"], "Groups");
    wire["externalIdentities"].as_array_mut().unwrap().reverse();
    let verified = Bundle::verify("App", &serde_json::to_vec(&wire).unwrap(), payload()).unwrap();
    assert_eq!(verified.manifest_json(), bundle.manifest_json());
    assert_eq!(verified.grace_seconds(), 7_200);
    assert_eq!(verified.target_api_version(), "1.2.3+exact");
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
