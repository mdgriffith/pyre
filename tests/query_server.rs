#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::manifest::{BoundManifest, Manifest, PyreSession, QueryManifest};
use pyre::server::query;
use pyre::server::sync::{
    ConnectedSessions, ReplacementRequest, SyncFence, SyncServer, SyncSession,
};
use pyre::{ast, parser, typecheck};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

fn manifest_for(
    context: &pyre::typecheck::Context,
    query_source: &str,
    include_generated_crud: bool,
) -> Result<Manifest, Box<dyn std::error::Error>> {
    let mut query_list = if query_source.trim().is_empty() {
        pyre::ast::QueryList {
            queries: Vec::new(),
        }
    } else {
        pyre::parser::parse_query("query.pyre", query_source)
            .map_err(|err| format!("query parse failed: {:?}", err))?
    };

    if include_generated_crud {
        pyre::generated_queries::append_generated_crud_queries(&mut query_list, context);
    }

    let query_info = pyre::typecheck::check_queries(&query_list, context)
        .map_err(|errors| format!("query typecheck failed: {:?}", errors))?;
    let mut files = Vec::new();
    pyre::generate::manifest::generate_queries(context, &query_list, &query_info, &mut files);
    let manifest_file = files
        .into_iter()
        .find(|file| file.path == std::path::Path::new("manifest.json"))
        .ok_or("manifest file should be generated")?;

    Ok(serde_json::from_str(&manifest_file.contents)?)
}

fn only_query(manifest: &Manifest) -> &QueryManifest {
    manifest
        .queries
        .values()
        .next()
        .expect("manifest should contain a query")
}

fn bind(manifest: &Manifest, context: &pyre::typecheck::Context) -> BoundManifest {
    BoundManifest::new(manifest.clone(), context).unwrap()
}

#[tokio::test]
async fn generated_attachment_directives_use_verified_host_handles(
) -> Result<(), Box<dyn std::error::Error>> {
    use pyre::server::schema::ensure_database;
    let mut schemas = Vec::new();
    for (namespace, record) in [("App", "MainItem"), ("Archive", "ArchiveItem")] {
        let mut schema = ast::Schema {
            namespace: namespace.into(),
            ..Default::default()
        };
        parser::run(
            "schema.pyre",
            &format!("record {record} {{\n @public\n id Id.Uuid @id\n}}\n"),
            &mut schema,
        )
        .unwrap();
        schemas.push(schema);
    }
    let database = ast::Database { schemas };
    let context = typecheck::check_schema(&database).unwrap();
    let manifest = manifest_for(
        &context,
        "query Both { mainItem { id } archiveItem { id } }",
        false,
    )?;
    let query = only_query(&manifest);
    assert_eq!(query.attached_dbs.len(), 1);
    let attached = &query.attached_dbs[0];
    let directive = format!("attach $db_{attached} as {attached}");
    assert!(query.sql.iter().any(|statement| statement.sql == directive));
    let temp = tempfile::tempdir()?;
    let mut sources = HashMap::new();
    let mut paths = HashMap::new();
    for schema in &database.schemas {
        let source = pyre::generate::to_string::standalone_schema_to_string(&context, schema);
        let path = temp.path().join(format!("{}.db", schema.namespace));
        let db = libsql::Builder::new_local(&path).build().await?;
        let conn = db.connect()?;
        ensure_database(&conn, &schema.namespace, &source).await?;
        let table = if schema.namespace == "App" {
            "mainItems"
        } else {
            "archiveItems"
        };
        conn.execute(
            &format!("INSERT INTO {table}(id) VALUES ('01890f6c-7b80-7000-8000-000000000001')"),
            (),
        )
        .await?;
        sources.insert(schema.namespace.clone(), source);
        paths.insert(schema.namespace.clone(), path);
    }
    let db = libsql::Builder::new_local(&paths[&query.primary_db])
        .build()
        .await?;
    let conn = db.connect()?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    // No implicit paths or empty attached databases when the host omits a binding.
    assert!(query::run(&conn, &manifest, &query.id, json!({}), &session)
        .await
        .is_err());
    conn.execute(
        &format!("ATTACH DATABASE ? AS {attached}"),
        libsql::params![paths[attached].to_string_lossy().to_string()],
    )
    .await?;
    let bound = bind(&manifest, &context);
    for _ in 0..2 {
        let result = query::run(&conn, &manifest, &query.id, json!({}), &session).await?;
        let rows = json!([{"id":"01890f6c-7b80-7000-8000-000000000001"}]);
        assert_eq!(result.response, json!({"mainItem":rows,"archiveItem":rows}));
        query::run_with_revision(&conn, &bound, &query.id, json!({}), &session, true).await?;
    }
    let other = libsql::Builder::new_local(&paths[attached]).build().await?;
    ensure_database(
        &other.connect()?,
        attached,
        &sources[attached].replace("@public", "@allow(query, insert, update, delete) { False }"),
    )
    .await?;
    assert!(query::run(&conn, &manifest, &query.id, json!({}), &session)
        .await
        .is_err());
    assert!(
        query::run_with_revision(&conn, &bound, &query.id, json!({}), &session, false)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn publication_requires_execution_contract_not_cached_permissions(
) -> Result<(), Box<dyn std::error::Error>> {
    use pyre::server::schema::{ensure_database, load_schema_from_database};
    let source = "session {\n userId Int\n}\nrecord Note {\n id Id.Uuid @id\n ownerId Int\n body String\n @allow(query) { True }\n @allow(insert, update, delete) { True }\n}\n";
    let db = TestDatabase::new(source).await?;
    let conn = db.db.connect()?;
    let migrator = db.db.connect()?;
    ensure_database(
        &migrator,
        ast::DEFAULT_SCHEMANAME,
        &source.replace(
            "@allow(query) { True }",
            "@allow(query) { ownerId == Session.userId }",
        ),
    )
    .await?;
    conn.execute("INSERT INTO notes(id, ownerId, body) VALUES ('01890f6c-7b80-7000-8000-000000000001', 1, 'secret')", ()).await?;
    let loaded = load_schema_from_database(&conn).await?;
    let context = loaded.context()?;
    let manifest = manifest_for(
        context,
        "update Change { note { body = \"secret\" } }",
        false,
    )?;
    let bound = bind(&manifest, context);
    let id = &only_query(&manifest).id;
    let session = PyreSession::new(json!({"userId":1}), &manifest.session_schema)?;
    let sessions = ConnectedSessions::from([
        ("origin".into(), session.logical().clone()),
        ("allowed".into(), session.logical().clone()),
        (
            "denied".into(),
            HashMap::from([("userId".into(), pyre::sync::SessionValue::Integer(2))]),
        ),
    ]);
    for legacy in [false, true] {
        for matching in [false, true] {
            let (mut result, commit) = if legacy {
                (
                    query::run_sync(&conn, &manifest, id, json!({}), &session).await?,
                    None,
                )
            } else {
                query::run_with_revision(&conn, &bound, id, json!({}), &session, true).await?
            };
            assert!(!result.affected_rows.is_empty());
            let server = SyncServer::new(if matching { context } else { &db.context });
            let messages = if let Some(commit) = commit {
                server.calculate_committed_deltas(
                    &mut result,
                    &sessions,
                    "main",
                    Some("origin"),
                    &commit,
                )?
            } else {
                server
                    .calculate_deltas(&conn, &mut result, &sessions, "main", Some("origin"))
                    .await?
            };
            if matching {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].session_id, "allowed");
                assert_eq!(messages[0].message.type_, "delta");
                assert!(!messages[0].message.data.is_empty());
                assert_eq!(result.response["sync"]["type"], "delta");
            } else {
                assert_eq!(messages.len(), 2);
                for message in messages {
                    assert_eq!(message.message.type_, "syncRequired");
                    assert!(message.message.data.is_empty());
                    assert_eq!(message.message.reconciliation.unwrap()["invalidate"], true);
                }
                assert_eq!(result.response["sync"]["type"], "syncRequired");
                assert!(result.response["sync"].get("data").is_none());
            }
        }
        // An unverified result must not acquire authority from the server's context,
        // and an origin absent from the registry still gets a row-free hint.
        let mut unverified = query::QueryResult::default();
        unverified.response = json!({});
        let server = SyncServer::new(context);
        let commit = query::CommittedRevision {
            database_epoch: "epoch".into(),
            revision: 99,
        };
        let messages = if legacy {
            server
                .calculate_deltas(
                    &conn,
                    &mut unverified,
                    &sessions,
                    "main",
                    Some("unregistered"),
                )
                .await?
        } else {
            server.calculate_committed_deltas(
                &mut unverified,
                &sessions,
                "main",
                Some("unregistered"),
                &commit,
            )?
        };
        assert_eq!(messages.len(), 3);
        assert!(messages
            .iter()
            .all(|message| message.message.data.is_empty()
                && message.message.type_ == "syncRequired"));
        assert_eq!(unverified.response["sync"]["type"], "syncRequired");
        assert!(unverified.response["sync"].get("data").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn persisted_authority_rejects_stale_handles_without_cache_refresh(
) -> Result<(), Box<dyn std::error::Error>> {
    use pyre::server::schema::{ensure_database, load_schema_from_database};
    let source = "record Item {\n id Id.Uuid @id\n value String\n @allow(query, insert, update, delete) { True }\n}\n";
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("authority.db");
    let db = libsql::Builder::new_local(&path).build().await?;
    let other_db = libsql::Builder::new_local(&path).build().await?;
    let conn = db.connect()?;
    let migrator = other_db.connect()?;
    let namespace = ast::DEFAULT_SCHEMANAME;
    ensure_database(&migrator, namespace, source).await?;
    let loaded = load_schema_from_database(&conn).await?;
    let context = loaded.context()?;
    let manifest = manifest_for(context,
        "query ReadItems { item { id value } }\nupdate ChangeItems { item { value = \"changed\" } }", false)?;
    let bound = bind(&manifest, context);
    let read = manifest
        .queries
        .values()
        .find(|q| q.operation == "query")
        .unwrap();
    let write = manifest
        .queries
        .values()
        .find(|q| q.operation == "update")
        .unwrap();
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    conn.execute(
        "INSERT INTO items(id, value) VALUES ('01890f6c-7b80-7000-8000-000000000001', 'original')",
        (),
    )
    .await?;
    let epoch: String = conn
        .query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ())
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    let fingerprint = bound.fingerprint().to_string();
    let binding = query::BatchBinding {
        database_id: "test",
        namespace,
        manifest: &fingerprint,
        instance: "tab",
        auth_generation: 0,
    };
    let mut batch = query::BatchRequest {
        version: 1,
        database_id: "test".into(),
        namespace: namespace.into(),
        manifest: fingerprint.clone(),
        instance: "tab".into(),
        auth_generation: 0,
        database_epoch: epoch.clone(),
        request_id: "batch".into(),
        sequence: 1,
        operations: vec![query::BatchOperation {
            operation: write.id.clone(),
            input: json!({}),
        }],
    };
    let replacement = ReplacementRequest {
        version: 1,
        request_id: "replacement".into(),
        target: 0,
        fence: SyncFence {
            database_id: "test".into(),
            namespace: namespace.into(),
            manifest: fingerprint.clone(),
            instance: "tab".into(),
            auth_generation: 0,
            database_epoch: epoch,
        },
    };
    assert_eq!(
        query::run(&conn, &manifest, &read.id, json!({}), &session)
            .await?
            .response["item"][0]["value"],
        "original"
    );
    SyncServer::new(context)
        .replacement(&conn, &bound, &binding, &replacement, &session)
        .await?;
    pyre::server::sync::catchup(&conn, context, &Default::default(), session.logical(), 100)
        .await?;
    // A semantic no-op migration remains compatible; no cache is refreshed.
    ensure_database(&migrator, namespace, &format!("// comment\n{source}")).await?;
    query::run_with_revision(&conn, &bound, &read.id, json!({}), &session, false).await?;
    migrator.execute("INSERT INTO _pyre_migrations(name, sql, schema, finished_at, error) VALUES ('failed', '', 'invalid', 1, 'failed'), ('pending', '', 'invalid', NULL, NULL)", ()).await?;
    query::run(&conn, &manifest, &read.id, json!({}), &session).await?;
    // Permission-only changes leave the physical table and sync lifetime intact.
    ensure_database(&migrator, namespace, &source.replace("True", "False")).await?;
    for sync_mode in [false, true] {
        for id in [&read.id, &write.id] {
            assert!(
                query::run_with_revision(&conn, &bound, id, json!({}), &session, sync_mode)
                    .await
                    .is_err()
            );
        }
    }
    assert!(query::run(&conn, &manifest, &read.id, json!({}), &session)
        .await
        .is_err());
    assert!(
        query::run_sync(&conn, &manifest, &write.id, json!({}), &session)
            .await
            .is_err()
    );
    assert!(query::run_batch(&conn, &bound, &binding, &batch, &session)
        .await
        .is_err());
    batch.operations.clear();
    assert!(query::run_batch(&conn, &bound, &binding, &batch, &session)
        .await
        .is_err());
    assert!(SyncServer::new(context)
        .replacement(&conn, &bound, &binding, &replacement, &session)
        .await
        .is_err());
    assert!(pyre::server::sync::catchup(
        &conn,
        context,
        &Default::default(),
        session.logical(),
        100
    )
    .await
    .is_err());
    let row = conn
        .query(
            "SELECT value, (SELECT server_revision FROM _pyre_sync WHERE id = 1) FROM items",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(row.get::<String>(0)?, "original");
    assert_eq!(row.get::<i64>(1)?, 0);
    drop(row);
    // No missing/invalid evidence fallback, even though the cached pair still binds.
    for case in 0..5 {
        match case {
            0..=2 => {
                let evidence = [None, Some(""), Some("not a schema")][case];
                migrator.execute("UPDATE _pyre_migrations SET schema = ? WHERE id = (SELECT max(id) FROM _pyre_migrations)", libsql::params![evidence]).await?;
            }
            3 => {
                migrator.execute("DELETE FROM _pyre_migrations", ()).await?;
            }
            _ => {
                migrator.execute("DROP TABLE _pyre_migrations", ()).await?;
            }
        }
        for id in [&read.id, &write.id] {
            assert!(query::run(&conn, &manifest, id, json!({}), &session)
                .await
                .is_err());
            assert!(query::run_sync(&conn, &manifest, id, json!({}), &session)
                .await
                .is_err());
            for sync_mode in [false, true] {
                assert!(query::run_with_revision(
                    &conn,
                    &bound,
                    id,
                    json!({}),
                    &session,
                    sync_mode
                )
                .await
                .is_err());
            }
        }
        assert!(query::run_batch(&conn, &bound, &binding, &batch, &session)
            .await
            .is_err());
        batch.operations.push(query::BatchOperation {
            operation: write.id.clone(),
            input: json!({}),
        });
        assert!(query::run_batch(&conn, &bound, &binding, &batch, &session)
            .await
            .is_err());
        batch.operations.clear();
        assert!(SyncServer::new(context)
            .replacement(&conn, &bound, &binding, &replacement, &session)
            .await
            .is_err());
        assert!(pyre::server::sync::catchup(
            &conn,
            context,
            &Default::default(),
            session.logical(),
            100
        )
        .await
        .is_err());
    }
    Ok(())
}

#[test]
fn non_unique_relationship_results_are_arrays() -> Result<(), Box<dyn std::error::Error>> {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "@syncable(false)\nrecord Parent {\n @public\n id Id.Int @id\n code String\n matches @link(code, Target.code)\n}\nrecord Target {\n @public\n id Id.Int @id\n code String\n}\n",
        &mut schema,
    )
    .unwrap();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();
    let manifest = manifest_for(
        &context,
        "query Parents { parent { id matches { id } } }",
        false,
    )?;
    let result = only_query(&manifest).result_schema.as_ref().unwrap();
    let pyre::server::manifest::ResultSchema::Object { fields } = result else {
        panic!("top-level query result should be an object");
    };
    let pyre::server::manifest::ResultSchema::Array { items } = &fields["parent"] else {
        panic!("top-level field should be an array");
    };
    let pyre::server::manifest::ResultSchema::Object { fields } = items.as_ref() else {
        panic!("parent rows should be objects");
    };
    assert!(matches!(
        &fields["matches"],
        pyre::server::manifest::ResultSchema::Array { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn revisioned_named_queries_stay_within_bound_namespace(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut main = ast::Schema {
        namespace: "Main".into(),
        ..Default::default()
    };
    parser::run(
        "Main/schema.pyre",
        "record MainItem {\n @public\n id Id.Uuid @id\n}\n",
        &mut main,
    )
    .unwrap();
    let mut archive = ast::Schema {
        namespace: "Archive".into(),
        ..Default::default()
    };
    parser::run(
        "Archive/schema.pyre",
        "record ArchiveItem {\n @public\n id Id.Uuid @id\n}\n",
        &mut archive,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![main, archive],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let manifest = manifest_for(
        &context,
        "query ReadMain { mainItem { id } }\nquery ReadArchive { archiveItem { id } }",
        false,
    )?;

    let standalone_source =
        pyre::generate::to_string::standalone_schema_to_string(&context, &database.schemas[0]);
    let loaded = pyre::db::introspect::from_raw(pyre::db::introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: pyre::db::introspect::MigrationState::NoMigrationTable,
        schema_source: standalone_source,
        links: vec![],
    });
    let pyre::db::introspect::SchemaResult::Success {
        context: standalone_context,
        ..
    } = loaded.schema
    else {
        panic!("standalone schema should load");
    };
    let bound = BoundManifest::new(manifest.clone(), &standalone_context)?;
    assert!(bound.authorizes_namespace("Main"));
    assert!(!bound.authorizes_namespace("Archive"));

    let archive_query = manifest
        .queries
        .values()
        .find(|query| query.primary_db == "Archive")
        .unwrap();
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let fingerprint = bound.fingerprint().to_string();
    let binding = query::BatchBinding {
        database_id: "main",
        namespace: "Archive",
        manifest: &fingerprint,
        instance: "test",
        auth_generation: 0,
    };
    let replacement = ReplacementRequest {
        version: 1,
        fence: SyncFence {
            database_id: "main".into(),
            instance: "test".into(),
            auth_generation: 0,
            namespace: "Archive".into(),
            manifest: fingerprint.clone(),
            database_epoch: "epoch".into(),
        },
        request_id: "replacement".into(),
        target: 0,
    };
    assert!(matches!(
        SyncServer::new(&standalone_context)
            .replacement(&conn, &bound, &binding, &replacement, &session)
            .await,
        Err(pyre::server::sync::Error::InvalidFence)
    ));
    let error =
        query::run_with_revision(&conn, &bound, &archive_query.id, json!({}), &session, false)
            .await
            .unwrap_err();
    assert!(matches!(error, query::Error::InvalidInput(_)));
    Ok(())
}

#[tokio::test]
async fn revisioned_named_queries_revalidate_sessions_against_the_bound_manifest(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        "@syncable(false)\nsession {\n userId Int\n}\nrecord Item {\n id Int @id\n ownerId Int\n @allow(*) { ownerId == Session.userId }\n}\n",
    )
    .await?;
    let manifest = manifest_for(&db.context, "query Items { item { id } }", false)?;
    let weak_schema = HashMap::from([(
        "userId".to_string(),
        serde_json::from_value(json!({
            "type": "String", "nullable": false, "omittable": false
        }))?,
    )]);
    let session = PyreSession::new(json!({ "userId": "7" }), &weak_schema)?;
    let conn = db.db.connect()?;

    let error = query::run_with_revision(
        &conn,
        &bind(&manifest, &db.context),
        &only_query(&manifest).id,
        json!({}),
        &session,
        false,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, query::Error::InvalidSession(_)));
    Ok(())
}

#[tokio::test]
async fn typed_json_session_enums_match_compiled_bundle_writes(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = format!(
        "{}\n{}",
        include_str!("../packages/server/fixtures/compiled-batch/session.pyre"),
        include_str!("../packages/server/fixtures/compiled-batch/schema.pyre")
    );
    let db = TestDatabase::new(&source).await?;
    let manifest = manifest_for(
        &db.context,
        include_str!("../packages/server/fixtures/compiled-batch/queries.pyre"),
        true,
    )?;
    let create = manifest
        .queries
        .values()
        .find(|query| {
            query
                .generated_edit
                .as_ref()
                .is_some_and(|edit| edit.kind == "create")
        })
        .unwrap();
    let lookup = manifest
        .queries
        .values()
        .find(|query| query.operation == "query")
        .unwrap();
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&create.primary_db, &fingerprint);
    let conn = db.db.connect()?;
    let details = json!({"_type":"Bundle","when":"2026-01-01T00:00:00Z","role":"Member","children":[{"_type":"Note","count":2,"enabled":false},{"_type":"Bundle","when":1767225600,"role":{"_type":"Admin"},"children":[],"byName":{},"note":null}],"byName":{"empty":{"_type":"Empty"}},"note":null});
    let mut session_value =
        json!({"userId":7,"role":"Member","unrelated":"value","context":details});
    let session = PyreSession::new(session_value.clone(), &manifest.session_schema)?;
    assert_eq!(
        create
            .generated_edit
            .as_ref()
            .unwrap()
            .create_uuid_input
            .as_deref(),
        Some("id")
    );
    let request = batch_request(&conn, &binding, vec![query::BatchOperation { operation:create.id.clone(), input:json!({"id":"01890f6c-7b80-7000-8000-000000000009","release":"00000000-0000-4000-8000-000000000002","enabled":true,"count":1,"role":"Member","details":details}) }]).await;
    for invalid_id in [
        Some(json!("01890f6c-7b80-4000-8000-000000000009")),
        Some(json!("01890F6C-7B80-7000-8000-000000000009")),
        Some(json!("01890f6c-7b80-7000-7000-000000000009")),
        Some(json!("not-a-uuid")),
        Some(json!(7)),
        None,
    ] {
        let mut invalid = request.clone();
        let input = invalid.operations[0].input.as_object_mut().unwrap();
        match invalid_id {
            Some(value) => {
                input.insert("id".into(), value);
            }
            None => {
                input.remove("id");
            }
        }
        assert!(query::run_batch(
            &conn,
            &bind(&manifest, &db.context),
            &binding,
            &invalid,
            &session,
        )
        .await
        .is_err());
    }
    let untouched = conn
        .query(
            "select (select count(*) from entries), server_revision from _pyre_sync",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(untouched.get::<i64>(0)?, 0);
    assert_eq!(untouched.get::<i64>(1)?, 0);
    query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await?;
    let row = conn
        .query(
            "select json(details) from entries where id = '01890f6c-7b80-7000-8000-000000000009'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&row.get::<String>(0)?)?;
    assert_eq!(stored["role"], json!({"_type":"Member"}));
    for role in [json!("Member"), json!({"_type":"Member"})] {
        session_value["role"] = role.clone();
        session_value["context"]["role"] = role;
        let session = PyreSession::new(session_value.clone(), &manifest.session_schema)?;
        assert_eq!(session.sql_args()["session_role"], json!("Member"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                session.sql_args()["session_context"].as_str().unwrap()
            )?,
            stored
        );
        let found = query::run(&conn, &manifest, &lookup.id, json!({}), &session).await?;
        assert_eq!(
            found.response,
            json!({"entry":[{"id":"01890f6c-7b80-7000-8000-000000000009"}]})
        );
    }
    session_value["context"]["role"] = json!("Admin");
    let different = PyreSession::new(session_value, &manifest.session_schema)?;
    let found = query::run(&conn, &manifest, &lookup.id, json!({}), &different).await?;
    assert_eq!(found.response, json!({"entry":[]}));
    Ok(())
}

#[tokio::test]
async fn nested_typed_json_session_matches_write_normalization(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Role = Member | Admin
type Details = Detail {
    when DateTime
    role Role
    enabled Bool
}
type Scope = Scoped { details Json<Details> } | Unscoped
session {
    scope Scope
}
record Entry {
    id Id.Int @id
    details Json<Details>
    @public
}
"#,
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateEntry($details: Json<Details>) {
    entry {
        details = $details
        id
    }
}
"#,
        false,
    )?;
    let create = manifest
        .queries
        .values()
        .find(|q| q.operation == "insert")
        .unwrap();
    let scope_schema = &manifest.session_schema["scope"];
    assert!(scope_schema.tagged_union_types.contains_key("Details"));
    assert!(scope_schema.tagged_union_variants["Scoped"]["details"]
        .tagged_union_types
        .is_empty());
    let details =
        json!({"_type":"Detail","when":"2026-01-01T00:00:00Z","role":"Member","enabled":true});
    let session_value = json!({"scope":{"_type":"Scoped","details":details}});
    let session = PyreSession::new(session_value.clone(), &manifest.session_schema)?;
    let conn = db.db.connect()?;
    query::run(
        &conn,
        &manifest,
        &create.id,
        json!({"details":details}),
        &session,
    )
    .await?;
    let row = conn
        .query("select id, json(details) from entries", ())
        .await?
        .next()
        .await?
        .unwrap();
    let id = row.get::<i64>(0)?;
    let stored: serde_json::Value = serde_json::from_str(&row.get::<String>(1)?)?;
    assert_eq!(
        stored,
        json!({"_type":"Detail","when":1767225600,"role":{"_type":"Member"},"enabled":true})
    );

    for role in [json!("Member"), json!({"_type":"Member","extra":"claim"})] {
        let mut claims = session_value.clone();
        claims["extra"] = json!("claim");
        claims["scope"]["extra"] = json!("claim");
        claims["scope"]["details"]["extra"] = json!("claim");
        claims["scope"]["details"]["role"] = role;
        claims["scope"]["details"]["enabled"] = json!(1);
        let session = PyreSession::new(claims.clone(), &manifest.session_schema)?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                session.sql_args()["session_scope__details"]
                    .as_str()
                    .unwrap()
            )?,
            stored
        );
        assert_eq!(
            session.logical()["scope__details"],
            pyre::sync::SessionValue::Text(stored.to_string())
        );
        // Nested JSON terminals are not supported in authored predicates; compare
        // the prepared binding directly against the generated query's stored value.
        let mut found = conn
            .query(
                "select id from entries where details = jsonb(?)",
                libsql::params![session.sql_args()["session_scope__details"]
                    .as_str()
                    .unwrap()],
            )
            .await?;
        assert_eq!(found.next().await?.unwrap().get::<i64>(0)?, id);
        assert!(found.next().await?.is_none());
        claims["scope"]["details"]["when"] = json!("2026-01-02T00:00:00Z");
        let different = PyreSession::new(claims, &manifest.session_schema)?;
        let mut found = conn
            .query(
                "select id from entries where details = jsonb(?)",
                libsql::params![different.sql_args()["session_scope__details"]
                    .as_str()
                    .unwrap()],
            )
            .await?;
        assert!(found.next().await?.is_none());
    }

    // Session coercions and ignored claims must not loosen write validation.
    for (field, value) in [
        ("enabled", json!(1)),
        ("extra", json!("claim")),
        ("role", json!({"_type":"Member","extra":"claim"})),
    ] {
        let mut invalid = details.clone();
        invalid[field] = value;
        assert!(query::run(
            &conn,
            &manifest,
            &create.id,
            json!({"details":invalid}),
            &session,
        )
        .await
        .is_err());
    }
    Ok(())
}

#[tokio::test]
async fn compiler_provenance_is_not_inferred_from_named_query_hashes(
) -> Result<(), Box<dyn std::error::Error>> {
    let db =
        TestDatabase::new("record Item {\n    id Id.Uuid @id\n    name String\n    @public\n}\n")
            .await?;
    let mut queries = ast::QueryList {
        queries: Vec::new(),
    };
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &db.context);
    let mut named = queries
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) if q.operation == ast::QueryOperation::Update => {
                Some(q.clone())
            }
            _ => None,
        })
        .unwrap();
    // Same name, contents, hashes, and absent locations as compiler CRUD, but explicitly named.
    named.generated_crud = false;
    let queries = ast::QueryList {
        queries: vec![ast::QueryDef::Query(named)],
    };
    let info = typecheck::check_queries(&queries, &db.context).unwrap();
    let mut files = Vec::new();
    pyre::generate::manifest::generate_queries(&db.context, &queries, &info, &mut files);
    let manifest: Manifest = serde_json::from_str(&files[0].contents)?;
    let named = only_query(&manifest);
    assert!(named.generated_edit.is_none());
    assert!(named.sql.iter().all(|sql| !sql.sql.contains("_pyreEditId")));
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&named.primary_db, &fingerprint);
    let conn = db.db.connect()?;
    let request = batch_request(
        &conn,
        &binding,
        vec![query::BatchOperation {
            operation: named.id.clone(),
            input: json!({"id":"00000000-0000-0000-0000-000000000999"}),
        }],
    )
    .await;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await?;
    assert_eq!(result.response["results"][0]["value"], json!({"item":[]}));
    Ok(())
}

#[tokio::test]
async fn manifest_fingerprint_covers_permissions_sql_and_codecs_and_matches_ts(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = "@syncable(false)\nrecord Item {\n    id Id.Int @id\n    name String\n    @allow(query) { True }\n    @allow(insert, update, delete) { True }\n}\n";
    let db = TestDatabase::new(source).await?;
    let query_source = "update Rename($name: String) {\n    item { name = $name }\n}\n";
    let manifest = manifest_for(&db.context, query_source, true)?;
    let fingerprint = manifest.fingerprint();
    for _ in 0..3 {
        assert_eq!(
            manifest_for(&db.context, query_source, true)?.fingerprint(),
            fingerprint
        );
    }
    let mut reordered = manifest.clone();
    let mut entries = reordered.queries.drain().collect::<Vec<_>>();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    reordered.queries.extend(entries);
    assert_eq!(reordered.fingerprint(), fingerprint);
    let id = manifest.queries.keys().next().unwrap();
    let mut changed = manifest.clone();
    changed.queries.get_mut(id).unwrap().sql[0]
        .sql
        .push_str(" /* changed */");
    assert_ne!(changed.fingerprint(), fingerprint);
    let mut changed = manifest.clone();
    changed
        .queries
        .get_mut(id)
        .unwrap()
        .compiled_contract
        .push('x');
    assert_ne!(changed.fingerprint(), fingerprint);
    let denied =
        TestDatabase::new(&source.replace("@allow(query) { True }", "@allow(query) { False }"))
            .await?;
    let denied_manifest = manifest_for(&denied.context, query_source, true)?;
    assert_ne!(denied_manifest.fingerprint(), fingerprint);
    assert_eq!(
        denied_manifest
            .queries
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        manifest
            .queries
            .keys()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert_ne!(
        manifest_for(&db.context, "", false)?.fingerprint(),
        manifest_for(&denied.context, "", false)?.fingerprint()
    );
    let nullable = TestDatabase::new(&source.replace("name String", "name String?")).await?;
    assert_ne!(
        manifest_for(&nullable.context, query_source, true)?.fingerprint(),
        fingerprint
    );
    let mut queries = parser::parse_query("query.pyre", query_source).unwrap();
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &db.context);
    let info = typecheck::check_queries(&queries, &db.context).unwrap();
    let mut files = Vec::new();
    pyre::generate::typescript::targets::server::generate_queries(
        &db.context,
        &info,
        &queries,
        Path::new("typescript"),
        &mut files,
    );
    let server = files
        .iter()
        .find(|file| file.path == Path::new("typescript/server.ts"))
        .unwrap();
    assert!(server.contents.contains(&format!(
        "export const manifestVersion = \"{}\";",
        fingerprint
    )));
    Ok(())
}

async fn batch_request(
    conn: &libsql::Connection,
    binding: &query::BatchBinding<'_>,
    operations: Vec<query::BatchOperation>,
) -> query::BatchRequest {
    let mut rows = conn
        .query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ())
        .await
        .unwrap();
    let epoch = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    query::BatchRequest {
        version: 1,
        database_id: "main".into(),
        namespace: binding.namespace.into(),
        manifest: binding.manifest.into(),
        instance: "test".into(),
        auth_generation: 1,
        database_epoch: epoch,
        request_id: "r1".into(),
        sequence: 1,
        operations,
    }
}

fn batch_binding<'a>(namespace: &'a str, fingerprint: &'a str) -> query::BatchBinding<'a> {
    query::BatchBinding {
        database_id: "main",
        namespace,
        manifest: fingerprint,
        instance: "test",
        auth_generation: 1,
    }
}

#[tokio::test]
async fn generated_nullable_boolean_keeps_explicit_null() -> Result<(), Box<dyn std::error::Error>>
{
    let db =
        TestDatabase::new("record Item {\n    id Id.Uuid @id\n    enabled Bool?\n    @public\n}\n")
            .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let create = manifest
        .queries
        .values()
        .find(|query| {
            query.operation == "insert"
                && query
                    .generated_edit
                    .as_ref()
                    .is_some_and(|edit| edit.kind == "create")
        })
        .unwrap();
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&create.primary_db, &fingerprint);
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let request = batch_request(
        &conn,
        &binding,
        vec![query::BatchOperation {
            operation: create.id.clone(),
            input: json!({"id":"01890f6c-7b80-7000-8000-000000000001","enabled":null}),
        }],
    )
    .await;
    query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await?;
    let mut rows = conn
        .query(
            "SELECT enabled IS NULL FROM items WHERE id='01890f6c-7b80-7000-8000-000000000001'",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
    Ok(())
}

#[tokio::test]
async fn batch_generated_edits_are_ordered_atomic_and_use_direct_cardinality(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Item {
    id Id.Uuid @id
    name String
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let update = query_by_operation(&manifest, "update");
    let delete = query_by_operation(&manifest, "delete");
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&create.primary_db, &fingerprint);
    assert_eq!(
        create
            .generated_edit
            .as_ref()
            .unwrap()
            .create_uuid_input
            .as_deref(),
        Some("id")
    );
    assert_eq!(
        update
            .generated_edit
            .as_ref()
            .unwrap()
            .write_statement_indices,
        vec![0]
    );
    assert_eq!(
        update.generated_edit.as_ref().unwrap().writable_inputs,
        vec!["name"]
    );
    let op = |q: &QueryManifest, input| query::BatchOperation {
        operation: q.id.clone(),
        input,
    };
    let request = batch_request(
        &conn,
        &binding,
        vec![
            op(
                create,
                json!({"id":"01890f6c-7b80-7000-8000-000000000001","name":"first"}),
            ),
            op(
                create,
                json!({"id":"01890f6c-7b80-7000-8000-000000000002","name":"second"}),
            ),
            op(
                update,
                json!({"id":"01890f6c-7b80-7000-8000-000000000001","name":"final"}),
            ),
        ],
    )
    .await;
    // Trigger-side writes do not inflate direct write cardinality.
    conn.execute("CREATE TABLE audit (message TEXT)", ())
        .await?;
    conn.execute("CREATE TRIGGER item_audit AFTER UPDATE ON items BEGIN INSERT INTO audit VALUES ('a'); INSERT INTO audit VALUES ('b'); END", ()).await?;
    let result = query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await?;
    assert_eq!(result.response["commitRevision"], 1);
    assert_eq!(
        result.response["results"],
        json!([
            {"index":0,"operation":create.id,"value":{"id":"01890f6c-7b80-7000-8000-000000000001"}},
            {"index":1,"operation":create.id,"value":{"id":"01890f6c-7b80-7000-8000-000000000002"}},
            {"index":2,"operation":update.id,"value":{"id":"01890f6c-7b80-7000-8000-000000000001"}},
        ])
    );
    let mut request = request;
    request.operations = vec![
        op(
            update,
            json!({"id":"01890f6c-7b80-7000-8000-000000000001","name":"rolled back"}),
        ),
        op(delete, json!({"id":"00000000-0000-0000-0000-000000000999"})),
    ];
    let error = query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("operation 1"));
    assert!(error.to_string().contains("TargetNotWritable"));
    let mut rows = conn.query("SELECT name, (SELECT server_revision FROM _pyre_sync WHERE id=1) FROM items WHERE id='01890f6c-7b80-7000-8000-000000000001'", ()).await?;
    let row = rows.next().await?.unwrap();
    assert_eq!(row.get::<String>(0)?, "final");
    assert_eq!(row.get::<i64>(1)?, 1);
    drop(rows);
    conn.execute(
        "UPDATE items SET updatedAt = 1 WHERE id = '01890f6c-7b80-7000-8000-000000000001'",
        (),
    )
    .await?;
    request.operations = vec![op(
        update,
        json!({"id":"01890f6c-7b80-7000-8000-000000000001","name":"final"}),
    )];
    assert_eq!(
        query::run_batch(
            &conn,
            &bind(&manifest, &db.context),
            &binding,
            &request,
            &session
        )
        .await?
        .response["commitRevision"],
        2
    );
    let mut rows = conn
        .query(
            "SELECT updatedAt FROM items WHERE id = '01890f6c-7b80-7000-8000-000000000001'",
            (),
        )
        .await?;
    assert!(rows.next().await?.unwrap().get::<i64>(0)? > 1);
    drop(rows);
    request.operations = vec![op(
        update,
        json!({"id":"01890f6c-7b80-7000-8000-000000000001","name":"protected","updatedAt":1}),
    )];
    assert_eq!(
        query::run_batch(
            &conn,
            &bind(&manifest, &db.context),
            &binding,
            &request,
            &session
        )
        .await
        .unwrap_err()
        .code(),
        "InvalidRequest"
    );
    request.operations = vec![op(
        update,
        json!({"id":"01890f6c-7b80-7000-8000-000000000001"}),
    )];
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("InvalidEdit"));
    request.operations.clear();
    let empty = query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await?;
    assert_eq!(empty.response["results"], json!([]));
    assert!(empty.response.get("commitRevision").is_none());
    assert!(SyncServer::new(&db.context)
        .replacement_messages(&empty, &std::collections::HashMap::new())
        .is_empty());

    Ok(())
}

#[tokio::test]
async fn batch_wildcard_delete_matches_compiled_result_schema(
) -> Result<(), Box<dyn std::error::Error>> {
    wildcard_delete_matches_compiled_result_schema(true).await
}

#[tokio::test]
async fn revisioned_wildcard_delete_matches_compiled_result_schema(
) -> Result<(), Box<dyn std::error::Error>> {
    wildcard_delete_matches_compiled_result_schema(false).await
}

async fn wildcard_delete_matches_compiled_result_schema(
    batch: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    const DELETED_ID: &str = "01890f6c-7b80-7000-8000-000000000001";
    const RETAINED_ID: &str = "01890f6c-7b80-7000-8000-000000000002";
    const UPDATED_AT: i64 = 1_767_225_600;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct DeletedItem {
        id: String,
        #[serde(rename = "updatedAt")]
        updated_at: i64,
        #[serde(alias = "label")]
        name: String,
        #[serde(alias = "active")]
        enabled: bool,
        #[serde(alias = "data")]
        metadata: serde_json::Value,
        #[serde(alias = "state")]
        status: serde_json::Value,
        note: Option<String>,
    }

    for selection in [
        "*",
        "* *",
        "* id name",
        "* label: name active: enabled",
        "label: name active: enabled * *",
        "* data: metadata state: status *",
    ] {
        let db = TestDatabase::new(
            r#"
type Status = Running | Stopped
record Item {
    @public
    id Id.Uuid @id
    name String
    enabled Bool
    metadata Json<List<Int>>
    status Status
    note String?
}
"#,
        )
        .await?;
        let conn = db.db.connect()?;
        conn.execute(
            "INSERT INTO items (id, name, enabled, metadata, status, note, updatedAt) VALUES (?1, 'deleted', 1, '[1,2]', 'Running', NULL, ?3), (?2, 'retained', 0, '[]', 'Stopped', 'keep', ?3)",
            libsql::params![DELETED_ID, RETAINED_ID, UPDATED_AT],
        )
        .await?;
        let manifest = manifest_for(
            &db.context,
            &format!(
                "delete Remove($id: Item.id) {{ removed: item {{ @where {{ id == $id }} {selection} }} }}"
            ),
            false,
        )?;
        let named = only_query(&manifest);
        // JSON decoding alone would hide duplicate keys from repeated wildcards.
        assert_eq!(named.sql[0].sql.matches("'note',").count(), 1);
        let bound = bind(&manifest, &db.context);
        let fingerprint = manifest.fingerprint();
        let binding = batch_binding(&named.primary_db, &fingerprint);
        let session = PyreSession::new(json!({}), &manifest.session_schema)?;
        let mut expected_row = json!({
            "id": DELETED_ID,
            "updatedAt": UPDATED_AT,
            "name": "deleted",
            "enabled": true,
            "metadata": [1, 2],
            "status": {"_type": "Running"},
            "note": null
        });
        if selection.contains("label:") {
            let fields = expected_row.as_object_mut().unwrap();
            let name = fields.remove("name").unwrap();
            let enabled = fields.remove("enabled").unwrap();
            fields.insert("label".into(), name);
            fields.insert("active".into(), enabled);
        }
        if selection.contains("data:") {
            let fields = expected_row.as_object_mut().unwrap();
            let metadata = fields.remove("metadata").unwrap();
            let status = fields.remove("status").unwrap();
            fields.insert("data".into(), metadata);
            fields.insert("state".into(), status);
        }

        // SELECT and DELETE must agree on wildcard expansion and explicit aliases.
        let read_manifest = manifest_for(
            &db.context,
            &format!(
                "query Read($id: Item.id) {{ removed: item {{ @where {{ id == $id }} {selection} }} }}"
            ),
            false,
        )?;
        let selected = query::run(
            &conn,
            &read_manifest,
            &only_query(&read_manifest).id,
            json!({"id": DELETED_ID}),
            &session,
        )
        .await?;
        assert_eq!(
            selected.response,
            json!({"removed": [expected_row.clone()]})
        );

        // The second delete is a successful named no-op and still commits a revision.
        for revision in 1..=2 {
            let response = if batch {
                let request = batch_request(
                    &conn,
                    &binding,
                    vec![query::BatchOperation {
                        operation: named.id.clone(),
                        input: json!({"id": DELETED_ID}),
                    }],
                )
                .await;
                let result = query::run_batch(&conn, &bound, &binding, &request, &session).await?;
                assert_eq!(result.response["commitRevision"], revision);
                assert_eq!(result.response["results"].as_array().unwrap().len(), 1);
                result.response["results"][0]["value"].clone()
            } else {
                let (result, committed) = query::run_with_revision(
                    &conn,
                    &bound,
                    &named.id,
                    json!({"id": DELETED_ID}),
                    &session,
                    false,
                )
                .await?;
                assert_eq!(committed.unwrap().revision, revision);
                result.response
            };
            let expected = if revision == 1 {
                json!({"removed": [expected_row.clone()]})
            } else {
                json!({"removed": []})
            };
            assert_eq!(response, expected, "selection: {selection}");
            let decoded: Vec<DeletedItem> = serde_json::from_value(response["removed"].clone())?;
            if revision == 1 {
                assert_eq!(
                    decoded,
                    vec![DeletedItem {
                        id: DELETED_ID.into(),
                        updated_at: UPDATED_AT,
                        name: "deleted".into(),
                        enabled: true,
                        metadata: json!([1, 2]),
                        status: json!({"_type": "Running"}),
                        note: None,
                    }]
                );
            } else {
                assert!(decoded.is_empty());
            }

            // A separate connection observes both the deletion and its committed revision.
            let persisted = db.db.connect()?;
            let row = persisted
                .query(
                    "SELECT (SELECT count(*) FROM items WHERE id = ?1), (SELECT name FROM items WHERE id = ?2), server_revision FROM _pyre_sync WHERE id = 1",
                    libsql::params![DELETED_ID, RETAINED_ID],
                )
                .await?
                .next()
                .await?
                .unwrap();
            assert_eq!(row.get::<i64>(0)?, 0);
            assert_eq!(row.get::<String>(1)?, "retained");
            assert_eq!(row.get::<i64>(2)?, revision);
        }
    }
    Ok(())
}

#[tokio::test]
async fn named_batch_results_are_validated_before_revision_and_commit(
) -> Result<(), Box<dyn std::error::Error>> {
    let db =
        TestDatabase::new("record Item {\n id Id.Uuid @id\n name String\n @public\n}\n").await?;
    let conn = db.db.connect()?;
    let mut manifest = manifest_for(
        &db.context,
        "insert Named($id: Item.id, $name: String) { item { id = $id name = $name } }",
        true,
    )?;
    let create = manifest
        .queries
        .values()
        .find(|query| {
            query.operation == "insert"
                && query
                    .generated_edit
                    .as_ref()
                    .is_some_and(|edit| edit.kind == "create")
        })
        .unwrap();
    let create_id = create.id.clone();
    let namespace = create.primary_db.clone();
    let named_id = manifest
        .queries
        .values()
        .find(|query| query.generated_edit.is_none())
        .unwrap()
        .id
        .clone();
    manifest.queries.get_mut(&named_id).unwrap().result_schema =
        Some(pyre::server::manifest::ResultSchema::Array {
            items: Box::new(pyre::server::manifest::ResultSchema::Object {
                fields: std::collections::BTreeMap::new(),
            }),
        });
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&namespace, &fingerprint);
    let request = batch_request(
        &conn,
        &binding,
        vec![
            query::BatchOperation {
                operation: create_id,
                input: json!({"id":"01890f6c-7b80-7000-8000-000000000003","name":"prefix"}),
            },
            query::BatchOperation {
                operation: named_id,
                input: json!({"id":"00000000-0000-0000-0000-000000000004","name":"named"}),
            },
        ],
    )
    .await;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let error = query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "InvalidRequest");
    assert_eq!(error.operation_index(), Some(1));
    let row = conn
        .query(
            "SELECT count(*), (SELECT server_revision FROM _pyre_sync) FROM items",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(row.get::<i64>(0)?, 0);
    assert_eq!(row.get::<i64>(1)?, 0);
    Ok(())
}

#[tokio::test]
async fn revisioned_named_results_are_validated_before_revision_and_commit(
) -> Result<(), Box<dyn std::error::Error>> {
    let db =
        TestDatabase::new("record Item {\n id Id.Uuid @id\n name String\n @public\n}\n").await?;
    let conn = db.db.connect()?;
    let mut manifest = manifest_for(
        &db.context,
        "insert Named($id: Item.id, $name: String) { item { id = $id name = $name } }",
        false,
    )?;
    let named_id = only_query(&manifest).id.clone();
    manifest.queries.get_mut(&named_id).unwrap().result_schema =
        Some(pyre::server::manifest::ResultSchema::Array {
            items: Box::new(pyre::server::manifest::ResultSchema::Object {
                fields: std::collections::BTreeMap::new(),
            }),
        });
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let error = query::run_with_revision(
        &conn,
        &bind(&manifest, &db.context),
        &named_id,
        json!({
            "id": "01890f6c-7b80-7000-8000-000000000004",
            "name": "rolled back"
        }),
        &session,
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "InvalidRequest");
    let row = conn
        .query(
            "SELECT (SELECT count(*) FROM items), server_revision FROM _pyre_sync WHERE id = 1",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(row.get::<i64>(0)?, 0);
    assert_eq!(row.get::<i64>(1)?, 0);
    Ok(())
}

#[tokio::test]
async fn js_protocol_counters_reject_values_above_max_safe_integer(
) -> Result<(), Box<dyn std::error::Error>> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

    let db =
        TestDatabase::new("record Item {\n id Id.Uuid @id\n name String\n @public\n}\n").await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let create = query_by_operation(&manifest, "insert");
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&create.primary_db, &fingerprint);
    let bound = bind(&manifest, &db.context);
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let mut request = batch_request(&conn, &binding, Vec::new()).await;

    request.sequence = MAX_SAFE_INTEGER + 1;
    assert!(
        query::run_batch(&conn, &bound, &binding, &request, &session)
            .await
            .is_err()
    );
    request.sequence = MAX_SAFE_INTEGER;
    assert!(
        query::run_batch(&conn, &bound, &binding, &request, &session)
            .await
            .is_ok()
    );

    request.auth_generation = MAX_SAFE_INTEGER + 1;
    let unsafe_binding = query::BatchBinding {
        auth_generation: MAX_SAFE_INTEGER + 1,
        ..binding
    };
    assert!(
        query::run_batch(&conn, &bound, &unsafe_binding, &request, &session)
            .await
            .is_err()
    );

    let replacement = ReplacementRequest {
        version: 1,
        fence: SyncFence {
            database_id: unsafe_binding.database_id.into(),
            instance: unsafe_binding.instance.into(),
            auth_generation: MAX_SAFE_INTEGER + 1,
            namespace: unsafe_binding.namespace.into(),
            manifest: unsafe_binding.manifest.into(),
            database_epoch: request.database_epoch.clone(),
        },
        request_id: "replacement".into(),
        target: 0,
    };
    assert!(matches!(
        SyncServer::new(&db.context)
            .replacement(&conn, &bound, &unsafe_binding, &replacement, &session)
            .await,
        Err(pyre::server::sync::Error::InvalidFence)
    ));
    let recipients = HashMap::from([("unsafe".into(), replacement.fence)]);
    assert!(SyncServer::new(&db.context)
        .committed_replacement_messages(
            &query::CommittedRevision {
                database_epoch: request.database_epoch,
                revision: 1,
            },
            unsafe_binding.database_id,
            unsafe_binding.namespace,
            unsafe_binding.manifest,
            &recipients,
        )
        .is_empty());
    Ok(())
}

#[tokio::test]
async fn batch_preflight_rejects_invalid_members_scopes_and_limits(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Item {
    id Id.Uuid @id
    name String
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let create = query_by_operation(&manifest, "insert");
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&create.primary_db, &fingerprint);
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let operation = query::BatchOperation {
        operation: create.id.clone(),
        input: json!({"id":"01890f6c-7b80-7000-8000-000000000005","name":"ok"}),
    };
    let mut request = batch_request(&conn, &binding, vec![operation.clone()]).await;
    let mut invalid = operation.clone();
    invalid.input = json!({"id":"01890f6c-7b80-7000-8000-000000000006","name":"bad","unknown":7});
    request.operations.push(invalid);
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .is_err());
    request.operations = vec![operation.clone(); 101];
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .is_err());
    request.operations = vec![operation.clone()];
    request.operations[0].input = json!({"name":"a".repeat(query::MAX_BATCH_PAYLOAD_BYTES)});
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .is_err());
    request.operations = vec![operation];
    request.namespace = "Unauthorized".into();
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .is_err());
    request.namespace = binding.namespace.into();
    let mut attached = manifest.clone();
    attached.queries.get_mut(&create.id).unwrap().attached_dbs = vec!["Other".into()];
    let attached_fingerprint = attached.fingerprint();
    let attached_binding = batch_binding(binding.namespace, &attached_fingerprint);
    request.manifest = attached_fingerprint.clone();
    assert!(query::run_batch(
        &conn,
        &bind(&attached, &db.context),
        &attached_binding,
        &request,
        &session
    )
    .await
    .is_err());
    let forged_binding = batch_binding(binding.namespace, "forged-version");
    request.manifest = "forged-version".into();
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &forged_binding,
        &request,
        &session
    )
    .await
    .is_err());
    request.manifest = fingerprint.clone();
    conn.execute("ATTACH DATABASE ':memory:' AS other", ())
        .await?;
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .is_err());
    conn.execute("DETACH DATABASE other", ()).await?;
    request.database_epoch = "stale".into();
    assert!(query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session
    )
    .await
    .is_err());
    let mut rows = conn
        .query(
            "SELECT (SELECT count(*) FROM items), server_revision FROM _pyre_sync WHERE id=1",
            (),
        )
        .await?;
    let row = rows.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 0);
    assert_eq!(row.get::<i64>(1)?, 0);
    Ok(())
}

#[tokio::test]
async fn batch_hidden_identities_named_noops_and_many_targets(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Item {
    id Id.Uuid @id
    name String
    @allow(query) { False }
    @allow(insert, update, delete) { True }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
update RenameAll($allName: String) {
    item { name = $allName }
}
update Noop($noopName: String, $missingId: Item.id) {
    item { @where { id == $missingId } name = $noopName }
}
"#,
        true,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let update = manifest
        .queries
        .values()
        .find(|q| q.operation == "update" && q.generated_edit.is_some())
        .unwrap();
    let all = query_by_input_names(&manifest, &["allName"]);
    let noop = query_by_input_names(&manifest, &["noopName", "missingId"]);
    assert!(all.generated_edit.is_none());
    let fingerprint = manifest.fingerprint();
    let binding = batch_binding(&create.primary_db, &fingerprint);
    let op = |q: &QueryManifest, input| query::BatchOperation {
        operation: q.id.clone(),
        input,
    };
    let mut request = batch_request(
        &conn,
        &binding,
        vec![
            op(
                create,
                json!({"id":"01890f6c-7b80-7000-8000-000000000007","name":"a"}),
            ),
            op(
                create,
                json!({"id":"01890f6c-7b80-7000-8000-000000000008","name":"b"}),
            ),
            op(
                update,
                json!({"id":"01890f6c-7b80-7000-8000-000000000007","name":"unreadable"}),
            ),
            op(all, json!({"allName":"both"})),
            op(
                noop,
                json!({"noopName":"unused", "missingId":"00000000-0000-0000-0000-000000000999"}),
            ),
        ],
    )
    .await;
    let result = query::run_batch(
        &conn,
        &bind(&manifest, &db.context),
        &binding,
        &request,
        &session,
    )
    .await?;
    assert_eq!(
        result.response["results"][2]["value"],
        json!({"id":"01890f6c-7b80-7000-8000-000000000007"})
    );
    assert_eq!(
        result.response["results"][3]["value"],
        json!({"item":[{"name":"both"},{"name":"both"}]})
    );
    assert_eq!(result.response["results"][4]["value"], json!({"item":[]}));
    assert_eq!(result.response["reconciliation"]["minimumSafeRevision"], 1);
    request.operations = vec![op(
        noop,
        json!({"noopName":"unused", "missingId":"00000000-0000-0000-0000-000000000999"}),
    )];
    assert_eq!(
        query::run_batch(
            &conn,
            &bind(&manifest, &db.context),
            &binding,
            &request,
            &session
        )
        .await?
        .response["commitRevision"],
        2
    );
    // Simulate a faulty compiled target predicate: read visibility and trigger totals must not mask >1.
    let mut broad = manifest.clone();
    broad.queries.get_mut(&update.id).unwrap().sql[0].sql =
        "UPDATE items SET name = 'bad' RETURNING id AS _pyreEditId".into();
    request.operations = vec![op(
        update,
        json!({"id":"01890f6c-7b80-7000-8000-000000000007","name":"bad"}),
    )];
    let broad_fingerprint = broad.fingerprint();
    let broad_binding = batch_binding(&create.primary_db, &broad_fingerprint);
    request.manifest = broad_fingerprint.clone();
    let error = query::run_batch(
        &conn,
        &bind(&broad, &db.context),
        &broad_binding,
        &request,
        &session,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "TargetNotWritable");
    assert_eq!(error.operation_index(), Some(0));
    let mut rows = conn
        .query("SELECT count(*) FROM items WHERE name = 'both'", ())
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 2);
    Ok(())
}

#[tokio::test]
async fn run_insert_roundtrips_nested_zero_field_union() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type VoteHandState
   = HandLowered
   | HandRaised

type EventPayload
   = PlayerVoteHandStateChanged {
        authorParticipantId Participant.id
        voteId String
        state VoteHandState
     }

record Participant {
    id Id.Int @id
    @public
}

record Event {
    id Id.Int @id
    payload EventPayload
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateEvent($payload: EventPayload) {
    event {
        payload = $payload
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let payload = json!({
        "_type": "PlayerVoteHandStateChanged",
        "authorParticipantId": 1,
        "voteId": "vote-1",
        "state": { "_type": "HandRaised" }
    });

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "payload": payload.clone() }),
        &session,
    )
    .await?;

    assert_eq!(result.response["event"][0]["payload"], payload);
    for invalid in [
        json!({"_type":"Unknown"}),
        json!({"_type":"PlayerVoteHandStateChanged","authorParticipantId":1.5,"voteId":"v","state":{"_type":"HandRaised"}}),
        json!({"_type":"PlayerVoteHandStateChanged","authorParticipantId":1,"voteId":"v","state":{"_type":"Invalid"}}),
    ] {
        assert!(query::run(
            &conn,
            &manifest,
            &only_query(&manifest).id,
            json!({"payload":invalid}),
            &session
        )
        .await
        .is_err());
    }
    Ok(())
}

#[tokio::test]
async fn run_query_accepts_uuid_record_id_parameter() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record ClocktowerGame {
    @public
    id Id.Uuid @id
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let id = "ab47fc00-f638-4ffd-8b08-4773181d6c3f";
    let manifest = manifest_for(
        &db.context,
        r#"
query ClocktowerGameKeystone($id: ClocktowerGame.id) {
    clocktowerGame {
        @where { id == $id }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": id }),
        &session,
    )
    .await?;

    assert!(result.response["clocktowerGame"].is_array());
    Ok(())
}

fn query_by_operation<'a>(manifest: &'a Manifest, operation: &str) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| query.operation == operation)
        .expect("query should exist for operation")
}

fn query_by_input_type<'a>(
    manifest: &'a Manifest,
    input_name: &str,
    input_type: &str,
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query
                .input_schema
                .get(input_name)
                .is_some_and(|schema| schema.type_ == input_type)
        })
        .expect("query should have an input with the expected type")
}

fn query_by_input_names<'a>(manifest: &'a Manifest, names: &[&str]) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.input_schema.len() == names.len()
                && names
                    .iter()
                    .all(|name| query.input_schema.contains_key(*name))
        })
        .expect("query should have the expected input fields")
}

fn query_by_operation_and_input_names<'a>(
    manifest: &'a Manifest,
    operation: &str,
    names: &[&str],
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.operation == operation
                && query.input_schema.len() == names.len()
                && names
                    .iter()
                    .all(|name| query.input_schema.contains_key(*name))
        })
        .expect("query should have the expected operation and inputs")
}

fn query_by_input_signature<'a>(
    manifest: &'a Manifest,
    names: &[&str],
    typed_name: &str,
    typed_value: &str,
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.input_schema.len() == names.len()
                && query
                    .input_schema
                    .get(typed_name)
                    .is_some_and(|schema| schema.type_ == typed_value)
                && names
                    .iter()
                    .all(|name| query.input_schema.contains_key(*name))
        })
        .expect("query should have the expected typed input fields")
}

fn query_by_nullable_input<'a>(
    manifest: &'a Manifest,
    name: &str,
    type_: &str,
    nullable: bool,
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.input_schema.len() == 1
                && query
                    .input_schema
                    .get(name)
                    .is_some_and(|schema| schema.type_ == type_ && schema.nullable == nullable)
        })
        .expect("query should have the expected nullable input")
}

#[tokio::test]
async fn generated_manifest_distinguishes_bool_from_unit_enum(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Status
    = Running
    | Stopped

record Game {
    @public
    id Id.Int @id
    isAdmin Bool
    status Status
}
"#,
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateGame($isAdmin: Bool, $status: Status) {
    game {
        isAdmin = $isAdmin
        status = $status
    }
}
"#,
        false,
    )?;
    let query = only_query(&manifest);
    assert!(!query.input_schema["isAdmin"].is_enum);
    assert!(query.input_schema["isAdmin"].enum_variants.is_empty());
    assert!(query.input_schema["status"].is_enum);
    assert_eq!(
        query.input_schema["status"].enum_variants,
        vec!["Running", "Stopped"]
    );

    Ok(())
}

fn generated_rust_server(
    context: &pyre::typecheck::Context,
    query_source: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let query_list = pyre::parser::parse_query("query.pyre", query_source).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{error:?}"))
    })?;
    let mut files = Vec::new();
    pyre::generate::server::rust::generate_queries(
        context,
        &query_list,
        Path::new("rust"),
        &mut files,
    );

    files
        .into_iter()
        .find(|file| file.path == Path::new("rust/server.rs"))
        .map(|file| file.contents)
        .ok_or_else(|| "generated Rust server file is missing".into())
}

#[test]
fn generated_rust_crud_omits_immutable_update_input_but_returns_field() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
@syncable(false)
record Document {
    @public
    id      Int @id
    ownerId Int @immutable
    title   String
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let mut query_list = ast::QueryList { queries: vec![] };
    pyre::generated_queries::append_generated_crud_queries(&mut query_list, &context);
    typecheck::check_queries(&query_list, &context).expect("CRUD typechecks");
    let mut files = Vec::new();
    pyre::generate::server::rust::generate_queries(
        &context,
        &query_list,
        Path::new("rust"),
        &mut files,
    );
    let generated = files
        .into_iter()
        .find(|file| file.path == Path::new("rust/server.rs"))
        .expect("generated Rust server")
        .contents;
    let create = generated
        .split("pub mod document_create {")
        .nth(1)
        .and_then(|rest| rest.split("pub type DocumentCreateInput").next())
        .expect("document_create module");
    let update = generated
        .split("pub mod document_update {")
        .nth(1)
        .and_then(|rest| rest.split("pub type DocumentUpdateInput").next())
        .expect("document_update module");
    let update_input = update
        .split("impl Input")
        .next()
        .expect("update Input struct");

    assert!(create.contains("#[serde(rename = \"ownerId\")]"));
    assert!(create.contains("pub owner_id: i64"));
    assert!(!update_input.contains("owner_id"));
    assert!(update.contains("pub owner_id: i64"));
    assert!(generated.contains("pub type DocumentCreateOutput = document_create::Output;"));
    assert!(generated.contains("pub type DocumentUpdateOutput = document_update::Output;"));
}

fn compile_and_run_generated_rust(
    server: &str,
    responses: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir)?;
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"
[package]
name = "pyre-generated-rust-test"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1.0.203", features = ["derive"] }
serde_json = "1.0.117"
serde_path_to_error = "0.1.20"
"#,
    )?;
    fs::write(src_dir.join("server.rs"), server)?;
    fs::write(
        src_dir.join("main.rs"),
        r#"
mod server;

fn main() {
    use std::convert::TryFrom;

    let game_running = server::create_clocktower_game::Input {
        status: server::ClocktowerGameStatus::ClocktowerRunning,
    };
    assert_eq!(
        game_running.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerRunning" } })
    );
    let game_stopped = server::create_clocktower_game::Input {
        status: server::ClocktowerGameStatus::ClocktowerStopped,
    };
    assert_eq!(
        game_stopped.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerStopped" } })
    );
    let participant_approved = server::create_clocktower_participant::Input {
        status: server::ClocktowerParticipantStatus::ClocktowerParticipantApproved,
    };
    assert_eq!(
        participant_approved.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerParticipantApproved" } })
    );
    let participant_rejected = server::create_clocktower_participant::Input {
        status: server::ClocktowerParticipantStatus::ClocktowerParticipantRejected,
    };
    assert_eq!(
        participant_rejected.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerParticipantRejected" } })
    );
    let scene_hidden = server::create_scene::Input {
        visibility: server::Visibility::Hidden,
    };
    assert_eq!(
        scene_hidden.into_json(),
        serde_json::json!({ "visibility": { "_type": "Hidden" } })
    );
    let scene_users = server::create_scene::Input {
        visibility: server::Visibility::Users { user_id: 7 },
    };
    assert_eq!(
        scene_users.into_json(),
        serde_json::json!({ "visibility": { "_type": "Users", "userId": 7 } })
    );
    let task_create = server::create_task::Input {
        action: server::Action::Create { title: "draft".to_string() },
    };
    assert_eq!(
        task_create.into_json(),
        serde_json::json!({ "action": { "_type": "Create", "title": "draft" } })
    );
    let task_delete = server::create_task::Input {
        action: server::Action::Delete { id: 7 },
    };
    assert_eq!(
        task_delete.into_json(),
        serde_json::json!({ "action": { "_type": "Delete", "id": 7 } })
    );
    let message = server::create_message::Input {
        envelope: server::Envelope::Wrapped {
            detail: server::Detail::Note { message: None },
        },
    };
    assert_eq!(
        message.into_json(),
        serde_json::json!({
            "envelope": { "_type": "Wrapped", "detail": { "_type": "Note", "message": null } }
        })
    );
    let archive = server::create_archive::Input {
        actions: vec![
            server::Action::Create { title: "first".to_string() },
            server::Action::Delete { id: 3 },
        ],
        actions_by_name: std::collections::HashMap::from([
            ("draft".to_string(), server::Action::Create { title: "second".to_string() }),
            ("removed".to_string(), server::Action::Delete { id: 4 }),
        ]),
    };
    assert_eq!(
        archive.into_json(),
        serde_json::json!({
            "actions": [
                { "_type": "Create", "title": "first" },
                { "_type": "Delete", "id": 3 }
            ],
            "actionsByName": {
                "draft": { "_type": "Create", "title": "second" },
                "removed": { "_type": "Delete", "id": 4 }
            }
        })
    );
    let update_task = server::update_task::Input {
        id: 1,
        action: server::Action::Delete { id: 9 },
    };
    assert_eq!(
        update_task.into_json(),
        serde_json::json!({ "id": 1, "action": { "_type": "Delete", "id": 9 } })
    );
    let delete_task = server::delete_task::Input { id: 2 };
    assert_eq!(delete_task.into_json(), serde_json::json!({ "id": 2 }));
    let event_seconds = server::create_event::Input {
        starts_at: server::DateTime::UnixSeconds(123),
    };
    assert_eq!(event_seconds.into_json(), serde_json::json!({ "startsAt": 123 }));
    let event_text = server::create_event::Input {
        starts_at: server::DateTime::Text("2026-01-02T03:04:05Z".to_string()),
    };
    assert_eq!(
        event_text.into_json(),
        serde_json::json!({ "startsAt": "2026-01-02T03:04:05Z" })
    );
    let uuid_game = server::get_uuid_game::Input {
        id: "00000000-0000-0000-0000-000000000001".to_string(),
    };
    assert_eq!(
        uuid_game.into_json(),
        serde_json::json!({ "id": "00000000-0000-0000-0000-000000000001" })
    );
    let delete_game = server::delete_clocktower_game::Input { game_id: 2 };
    assert_eq!(delete_game.into_json(), serde_json::json!({ "gameId": 2 }));
    let nullable_game = server::create_nullable_game::Input {
        status: Some(server::ClocktowerGameStatus::ClocktowerRunning),
    };
    assert_eq!(
        nullable_game.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerRunning" } })
    );
    let nullable_game = server::create_nullable_game::Input { status: None };
    assert_eq!(nullable_game.into_json(), serde_json::json!({ "status": null }));
    let update_game = server::update_clocktower_game::Input {
        id: 1,
        status: server::ClocktowerGameStatus::ClocktowerStopped,
    };
    assert_eq!(
        update_game.into_json(),
        serde_json::json!({ "id": 1, "status": { "_type": "ClocktowerStopped" } })
    );
    let replace_task: server::ReplaceTaskInput = server::replace_task::Input {
        id: 1,
        action: server::Action::Create { title: "replacement".to_string() },
    };
    assert_eq!(
        replace_task.into_json(),
        serde_json::json!({
            "id": 1,
            "action": { "_type": "Create", "title": "replacement" }
        })
    );
    assert_eq!(server::query_ids::REPLACE_TASK, server::replace_task::ID);

    let responses: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(std::env::args().nth(1).expect("response path"))
            .expect("read responses"),
    )
    .expect("parse responses");
    let response = |name| responses.get(name).cloned().expect("named response");
    let game_running = server::create_clocktower_game::Output::try_from(response("game_running"))
        .expect("generated running game output decodes");
    assert!(matches!(
        &game_running.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerRunning
    ));
    let game_stopped = server::create_clocktower_game::Output::try_from(response("game_stopped"))
        .expect("generated stopped game output decodes");
    assert!(matches!(
        &game_stopped.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerStopped
    ));
    let participant_approved =
        server::create_clocktower_participant::Output::try_from(response("participant_approved"))
            .expect("generated approved participant output decodes");
    assert!(matches!(
        &participant_approved.clocktower_participant[0].status,
        server::ClocktowerParticipantStatus::ClocktowerParticipantApproved
    ));
    let participant_rejected =
        server::create_clocktower_participant::Output::try_from(response("participant_rejected"))
            .expect("generated rejected participant output decodes");
    assert!(matches!(
        &participant_rejected.clocktower_participant[0].status,
        server::ClocktowerParticipantStatus::ClocktowerParticipantRejected
    ));
    let scene_hidden = server::create_scene::Output::try_from(response("scene_hidden"))
        .expect("generated hidden scene output decodes");
    assert!(matches!(
        &scene_hidden.scene[0].visibility,
        server::Visibility::Hidden
    ));
    let scene_users = server::create_scene::Output::try_from(response("scene_users"))
        .expect("generated users scene output decodes");
    match &scene_users.scene[0].visibility {
        server::Visibility::Users { user_id } => assert_eq!(*user_id, 7),
        _ => panic!("generated users scene output decoded the wrong variant"),
    }
    let task_create = server::create_task::Output::try_from(response("task_create"))
        .expect("generated create task output decodes");
    match &task_create.task[0].action {
        server::Action::Create { title } => assert_eq!(title, "draft"),
        _ => panic!("generated create task output decoded the wrong variant"),
    }
    let task_delete = server::create_task::Output::try_from(response("task_delete"))
        .expect("generated delete task output decodes");
    match &task_delete.task[0].action {
        server::Action::Delete { id } => assert_eq!(*id, 7),
        _ => panic!("generated delete task output decoded the wrong variant"),
    }
    let message = server::create_message::Output::try_from(response("message_null"))
        .expect("generated message output decodes");
    match &message.message[0].envelope {
        server::Envelope::Wrapped {
            detail: server::Detail::Note { message },
        } => assert_eq!(message, &None),
        _ => panic!("generated message output decoded the wrong nested variant"),
    }
    let archive = server::create_archive::Output::try_from(response("archive"))
        .expect("generated archive output decodes");
    assert!(matches!(
        &archive.archive[0].actions[..],
        [server::Action::Create { title }, server::Action::Delete { id: 3 }] if title == "first"
    ));
    match &archive.archive[0].actions_by_name["draft"] {
        server::Action::Create { title } => assert_eq!(title, "second"),
        _ => panic!("generated archive dictionary decoded the wrong variant"),
    }
    let updated_task = server::update_task::Output::try_from(response("task_updated"))
        .expect("generated updated task output decodes");
    assert!(matches!(
        &updated_task.task[0].action,
        server::Action::Delete { id: 9 }
    ));
    let deleted_task = server::delete_task::Output::try_from(response("task_deleted"))
        .expect("generated deleted task output decodes");
    assert!(matches!(
        &deleted_task.task[0].action,
        server::Action::Delete { id: 7 }
    ));
    let listed_games = server::list_clocktower_games::Output::try_from(response("games_listed"))
        .expect("generated game query output decodes");
    assert!(matches!(
        &listed_games.clocktower_game[..],
        [
            server::list_clocktower_games::ClocktowerGame { status: server::ClocktowerGameStatus::ClocktowerRunning },
            server::list_clocktower_games::ClocktowerGame { status: server::ClocktowerGameStatus::ClocktowerStopped },
            server::list_clocktower_games::ClocktowerGame { status: server::ClocktowerGameStatus::ClocktowerRunning }
        ]
    ));
    let updated_game = server::update_clocktower_game::Output::try_from(response("game_updated"))
        .expect("generated unit enum update output decodes");
    assert!(matches!(
        &updated_game.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerStopped
    ));
    let event = server::create_event::Output::try_from(response("event_seconds"))
        .expect("generated event output decodes");
    assert!(matches!(
        &event.event[0].starts_at,
        server::DateTime::UnixSeconds(123)
    ));
    let uuid_game = server::get_uuid_game::Output::try_from(response("uuid_game"))
        .expect("generated UUID game output decodes");
    assert_eq!(uuid_game.uuid_game[0].id, "00000000-0000-0000-0000-000000000001");
    let event_text = server::create_event::Output::try_from(response("event_text"))
        .expect("generated text event output decodes");
    assert!(matches!(
        &event_text.event[0].starts_at,
        server::DateTime::UnixSeconds(1767323045)
    ));
    let deleted_game = server::delete_clocktower_game::Output::try_from(response("game_deleted"))
        .expect("generated unit enum delete output decodes");
    assert!(matches!(
        &deleted_game.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerStopped
    ));
    let nullable_game = server::create_nullable_game::Output::try_from(response("nullable_game_running"))
        .expect("generated nullable unit enum output decodes");
    assert!(matches!(
        &nullable_game.nullable_game[0].status,
        Some(server::ClocktowerGameStatus::ClocktowerRunning)
    ));
    let nullable_game = server::create_nullable_game::Output::try_from(response("nullable_game_null"))
        .expect("generated null unit enum output decodes");
    assert!(nullable_game.nullable_game[0].status.is_none());
    let replaced: server::ReplaceTaskOutput =
        server::replace_task::Output::try_from(response("task_replaced"))
            .expect("generated transaction output decodes");
    assert!(matches!(
        &replaced.changed_task[0].action,
        server::Action::Create { title } if title == "replacement"
    ));
    assert!(matches!(
        &replaced.created_task[0].action,
        server::Action::Create { title } if title == "replacement"
    ));
    assert!(matches!(
        &replaced.removed_task[0].action,
        server::Action::Create { title } if title == "replacement"
    ));
}
"#,
    )?;
    fs::write(
        temp_dir.path().join("responses.json"),
        serde_json::to_string(responses)?,
    )?;

    let output = Command::new("cargo")
        .args(["run", "--offline", "--quiet", "--manifest-path"])
        .arg(temp_dir.path().join("Cargo.toml"))
        .arg("--")
        .arg(temp_dir.path().join("responses.json"))
        .output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(std::io::Error::other(format!(
        "generated Rust client failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
    .into())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "_type")]
enum ClocktowerGameStatus {
    ClocktowerRunning,
    ClocktowerStopped,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "_type")]
enum ClocktowerParticipantStatus {
    ClocktowerParticipantApproved,
    ClocktowerParticipantRejected,
}

#[derive(Serialize)]
struct ClocktowerGameCreateInput {
    status: ClocktowerGameStatus,
}

#[derive(Serialize)]
struct ClocktowerParticipantCreateInput {
    status: ClocktowerParticipantStatus,
}

#[derive(Deserialize)]
struct ClocktowerGame {
    status: ClocktowerGameStatus,
}

#[derive(Deserialize)]
struct ClocktowerParticipant {
    status: ClocktowerParticipantStatus,
}

#[derive(Deserialize)]
struct ClocktowerGameCreateOutput {
    #[serde(rename = "clocktowerGame")]
    clocktower_game: Vec<ClocktowerGame>,
}

#[derive(Deserialize)]
struct ClocktowerParticipantCreateOutput {
    #[serde(rename = "clocktowerParticipant")]
    clocktower_participant: Vec<ClocktowerParticipant>,
}

#[tokio::test]
async fn run_mutations_roundtrip_tagged_union_variants() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type ClocktowerGameStatus
    = ClocktowerRunning
    | ClocktowerStopped

type ClocktowerParticipantStatus
    = ClocktowerParticipantApproved
    | ClocktowerParticipantRejected

type Visibility
    = Hidden
    | Users { userId Int }

type Action
    = Create { title String }
    | Delete { id Int }

type Detail
    = Note { message String? }

type Envelope
    = Wrapped { detail Detail }

record ClocktowerGame {
    @public
    id Id.Int @id
    status ClocktowerGameStatus
}

record ClocktowerParticipant {
    @public
    id Id.Int @id
    status ClocktowerParticipantStatus
}

record Scene {
    @public
    id Id.Int @id
    visibility Visibility
}

record Task {
    @public
    id Id.Int @id
    action Action
}

record Message {
    @public
    id Id.Int @id
    envelope Envelope
}

record Archive {
    @public
    id Id.Int @id
    actions Json<List<Action>>
    actionsByName Json<Dict<Action>>
}

record Event {
    @public
    id Id.Int @id
    startsAt DateTime
}

record UuidGame {
    @public
    id Id.Uuid @id
}

record NullableGame {
    @public
    id Id.Int @id
    status ClocktowerGameStatus?
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let query_source = r#"
insert CreateClocktowerGame($status: ClocktowerGameStatus) {
    clocktowerGame {
        status = $status
    }
}

insert CreateClocktowerParticipant($status: ClocktowerParticipantStatus) {
    clocktowerParticipant {
        status = $status
    }
}

query ListClocktowerGames {
    clocktowerGame {
        status
    }
}

update UpdateClocktowerGame($id: ClocktowerGame.id, $status: ClocktowerGameStatus) {
    clocktowerGame {
        @where { id == $id }
        status = $status
    }
}

insert CreateScene($visibility: Visibility) {
    scene {
        visibility = $visibility
    }
}

insert CreateTask($action: Action) {
    task {
        action = $action
    }
}

insert CreateMessage($envelope: Envelope) {
    message {
        envelope = $envelope
    }
}

insert CreateArchive($actions: Json<List<Action>>, $actionsByName: Json<Dict<Action>>) {
    archive {
        actions = $actions
        actionsByName = $actionsByName
    }
}

update UpdateTask($id: Task.id, $action: Action) {
    task {
        @where { id == $id }
        action = $action
    }
}

delete DeleteTask($id: Task.id) {
    task {
        @where { id == $id }
        action
    }
}

insert CreateEvent($startsAt: DateTime) {
    event {
        startsAt = $startsAt
    }
}

query GetUuidGame($id: UuidGame.id) {
    uuidGame {
        @where { id == $id }
        id
    }
}

delete DeleteClocktowerGame($gameId: ClocktowerGame.id) {
    clocktowerGame {
        @where { id == $gameId }
        status
    }
}

insert CreateNullableGame($status: ClocktowerGameStatus?) {
    nullableGame {
        status = $status
    }
}

transaction ReplaceTask($id: Task.id, $action: Action) {
    update changedTask: task {
        @where { id == $id }
        action = $action
        id
    }
    insert createdTask: task {
        action = $action
        id
    }
    delete removedTask: task {
        @where { id == $id }
        id
        action
    }
}
"#;
    let manifest = manifest_for(&db.context, query_source, false)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let mut responses = serde_json::Map::new();

    for (name, status, tag) in [
        (
            "game_running",
            ClocktowerGameStatus::ClocktowerRunning,
            "ClocktowerRunning",
        ),
        (
            "game_stopped",
            ClocktowerGameStatus::ClocktowerStopped,
            "ClocktowerStopped",
        ),
    ] {
        let input = serde_json::to_value(ClocktowerGameCreateInput {
            status: status.clone(),
        })?;
        assert_eq!(input["status"]["_type"], json!(tag));
        let result = query::run(
            &conn,
            &manifest,
            &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", false).id,
            input,
            &session,
        )
        .await?;
        let output: ClocktowerGameCreateOutput = serde_json::from_value(result.response.clone())?;
        assert_eq!(output.clocktower_game[0].status, status);
        responses.insert(name.to_string(), result.response);
    }

    let mut rows = conn
        .query("select status from clocktowerGames order by id", ())
        .await?;
    let mut stored_game_statuses = Vec::new();
    while let Some(row) = rows.next().await? {
        stored_game_statuses.push(row.get::<String>(0)?);
    }
    assert_eq!(
        stored_game_statuses,
        vec!["ClocktowerRunning", "ClocktowerStopped"]
    );

    for invalid_status in [
        json!({}),
        json!({ "_type": 1 }),
        json!({ "_type": "Unknown" }),
    ] {
        assert!(
            query::run(
                &conn,
                &manifest,
                &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", false).id,
                json!({ "status": invalid_status }),
                &session,
            )
            .await
            .is_err(),
            "invalid enum input should be rejected"
        );
    }

    let legacy_result = query::run(
        &conn,
        &manifest,
        &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", false).id,
        json!({ "status": "ClocktowerRunning" }),
        &session,
    )
    .await?;
    assert_eq!(
        legacy_result.response["clocktowerGame"][0]["status"],
        json!({ "_type": "ClocktowerRunning" })
    );

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &[]).id,
        json!({}),
        &session,
    )
    .await?;
    let listed_game_response = result.response;

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_signature(
            &manifest,
            &["id", "status"],
            "status",
            "ClocktowerGameStatus",
        )
        .id,
        json!({ "id": 1, "status": { "_type": "ClocktowerStopped" } }),
        &session,
    )
    .await?;
    let updated_game_response = result.response;

    for (name, status, tag) in [
        (
            "participant_approved",
            ClocktowerParticipantStatus::ClocktowerParticipantApproved,
            "ClocktowerParticipantApproved",
        ),
        (
            "participant_rejected",
            ClocktowerParticipantStatus::ClocktowerParticipantRejected,
            "ClocktowerParticipantRejected",
        ),
    ] {
        let input = serde_json::to_value(ClocktowerParticipantCreateInput {
            status: status.clone(),
        })?;
        assert_eq!(input["status"]["_type"], json!(tag));
        let result = query::run(
            &conn,
            &manifest,
            &query_by_input_type(&manifest, "status", "ClocktowerParticipantStatus").id,
            input,
            &session,
        )
        .await?;
        let output: ClocktowerParticipantCreateOutput =
            serde_json::from_value(result.response.clone())?;
        assert_eq!(output.clocktower_participant[0].status, status);
        responses.insert(name.to_string(), result.response);
    }

    let mut rows = conn
        .query("select status from clocktowerParticipants order by id", ())
        .await?;
    let mut stored_statuses = Vec::new();
    while let Some(row) = rows.next().await? {
        stored_statuses.push(row.get::<String>(0)?);
    }
    assert_eq!(
        stored_statuses,
        vec![
            "ClocktowerParticipantApproved",
            "ClocktowerParticipantRejected"
        ]
    );

    for (name, visibility) in [
        ("scene_hidden", json!({ "_type": "Hidden" })),
        ("scene_users", json!({ "_type": "Users", "userId": 7 })),
    ] {
        let result = query::run(
            &conn,
            &manifest,
            &query_by_input_type(&manifest, "visibility", "Visibility").id,
            json!({ "visibility": visibility.clone() }),
            &session,
        )
        .await?;
        assert_eq!(result.response["scene"][0]["visibility"], visibility);
        responses.insert(name.to_string(), result.response);
    }

    for (name, action) in [
        (
            "task_create",
            json!({ "_type": "Create", "title": "draft" }),
        ),
        ("task_delete", json!({ "_type": "Delete", "id": 7 })),
    ] {
        let result = query::run(
            &conn,
            &manifest,
            &query_by_input_names(&manifest, &["action"]).id,
            json!({ "action": action.clone() }),
            &session,
        )
        .await?;
        assert_eq!(result.response["task"][0]["action"], action);
        responses.insert(name.to_string(), result.response);
    }

    let envelope = json!({
        "_type": "Wrapped",
        "detail": { "_type": "Note", "message": null }
    });
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "envelope", "Envelope").id,
        json!({ "envelope": envelope.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["message"][0]["envelope"], envelope);
    responses.insert("message_null".to_string(), result.response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "startsAt", "DateTime").id,
        json!({ "startsAt": "2026-01-02T03:04:05Z" }),
        &session,
    )
    .await?;
    responses.insert("event_text".to_string(), result.response);

    let actions = json!([
        { "_type": "Create", "title": "first" },
        { "_type": "Delete", "id": 3 }
    ]);
    let actions_by_name = json!({
        "draft": { "_type": "Create", "title": "second" },
        "removed": { "_type": "Delete", "id": 4 }
    });
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "actions", "Json<List<Action>>").id,
        json!({ "actions": actions.clone(), "actionsByName": actions_by_name.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["archive"][0]["actions"], actions);
    assert_eq!(
        result.response["archive"][0]["actionsByName"],
        actions_by_name
    );
    responses.insert("archive".to_string(), result.response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &["gameId"]).id,
        json!({ "gameId": 2 }),
        &session,
    )
    .await?;
    responses.insert("game_deleted".to_string(), result.response);

    for (name, status) in [
        (
            "nullable_game_running",
            json!({ "_type": "ClocktowerRunning" }),
        ),
        ("nullable_game_null", serde_json::Value::Null),
    ] {
        let result = query::run(
            &conn,
            &manifest,
            &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", true).id,
            json!({ "status": status }),
            &session,
        )
        .await?;
        responses.insert(name.to_string(), result.response);
    }

    let updated_action = json!({ "_type": "Delete", "id": 9 });
    let result = query::run(
        &conn,
        &manifest,
        &query_by_operation_and_input_names(&manifest, "update", &["id", "action"]).id,
        json!({ "id": 1, "action": updated_action.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["task"][0]["action"], updated_action);
    responses.insert("task_updated".to_string(), result.response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_signature(&manifest, &["id"], "id", "Id.Int").id,
        json!({ "id": 2 }),
        &session,
    )
    .await?;
    assert_eq!(
        result.response["task"][0]["action"],
        json!({ "_type": "Delete", "id": 7 })
    );
    responses.insert("task_deleted".to_string(), result.response);

    responses.insert("games_listed".to_string(), listed_game_response);
    responses.insert("game_updated".to_string(), updated_game_response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "startsAt", "DateTime").id,
        json!({ "startsAt": 123 }),
        &session,
    )
    .await?;
    responses.insert("event_seconds".to_string(), result.response);

    let uuid = "00000000-0000-0000-0000-000000000001";
    conn.execute(
        "insert into uuidGames (id) values (?)",
        libsql::params![uuid],
    )
    .await?;
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "id", "Id.Uuid").id,
        json!({ "id": uuid }),
        &session,
    )
    .await?;
    responses.insert("uuid_game".to_string(), result.response);

    let replacement_action = json!({ "_type": "Create", "title": "replacement" });
    let transaction = query_by_operation(&manifest, "transaction");
    let result = query::run(
        &conn,
        &manifest,
        &transaction.id,
        json!({ "id": 1, "action": replacement_action.clone() }),
        &session,
    )
    .await?;
    assert_eq!(
        result.response["changedTask"][0]["action"],
        replacement_action
    );
    assert_eq!(
        result.response["createdTask"][0]["action"],
        replacement_action
    );
    assert_eq!(
        result.response["removedTask"][0]["action"],
        replacement_action
    );
    responses.insert("task_replaced".to_string(), result.response);

    compile_and_run_generated_rust(
        &generated_rust_server(&db.context, query_source)?,
        &responses,
    )?;

    Ok(())
}

#[tokio::test]
async fn run_query_binds_tagged_enum_session_values() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Role
    = Admin
    | Member

session {
    role Role
}

record Note {
    id Id.Int @id
    role Role
    body String
    @allow(*) { role == Session.role }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into notes (id, role, body) values (1, 'Admin', 'one'), (2, 'Member', 'two');",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query ListNotes {
    note {
        id
    }
}

update UpdateNote($id: Note.id, $body: String) {
    note {
        @where { id == $id }
        body = $body
    }
}

delete DeleteNote($id: Note.id) {
    note {
        @where { id == $id }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(
        json!({ "role": { "_type": "Admin" } }),
        &manifest.session_schema,
    )?;
    assert_eq!(session.sql_args()["session_role"], json!("Admin"));

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &[]).id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(result.response["note"], json!([{ "id": 1 }]));

    query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "update").id,
        json!({ "id": 1, "body": "updated" }),
        &session,
    )
    .await?;
    let mut rows = conn
        .query("select body from notes where id = 1", ())
        .await?;
    assert_eq!(
        rows.next()
            .await?
            .expect("admin note exists")
            .get::<String>(0)?,
        "updated"
    );
    query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "delete").id,
        json!({ "id": 1 }),
        &session,
    )
    .await?;

    let member_session = PyreSession::new(
        json!({ "role": { "_type": "Member" } }),
        &manifest.session_schema,
    )?;
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &[]).id,
        json!({}),
        &member_session,
    )
    .await?;
    assert_eq!(result.response["note"], json!([{ "id": 2 }]));
    assert!(PyreSession::new(
        json!({ "role": { "_type": "Unknown" } }),
        &manifest.session_schema,
    )
    .is_err());

    Ok(())
}

#[tokio::test]
async fn run_query_binds_tagged_union_session_paths() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type SessionScope
    = Workspace {
        id Int
    }
    | Account {
        accountId Int
    }

session {
    scope SessionScope
}

record Resource {
    id Id.Int @id
    workspaceId Int
    @allow(query) { workspaceId == Session.scope.Workspace.id }
    @allow(insert, update, delete) { False }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into resources (id, workspaceId) values (1, 7), (2, 8);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query ListResources {
    resource {
        id
    }
}
"#,
        false,
    )?;
    let query_manifest = only_query(&manifest);
    assert_eq!(query_manifest.session_args, vec!["scope", "scope__id"]);
    assert!(manifest.session_schema["scope"]
        .tagged_union_variants
        .contains_key("Workspace"));

    let workspace_session = PyreSession::new(
        json!({ "scope": { "_type": "Workspace", "id": 7 } }),
        &manifest.session_schema,
    )?;
    assert_eq!(
        workspace_session.sql_args()["session_scope"],
        json!("Workspace")
    );
    assert_eq!(workspace_session.sql_args()["session_scope__id"], json!(7));
    let result = query::run(
        &conn,
        &manifest,
        &query_manifest.id,
        json!({}),
        &workspace_session,
    )
    .await?;
    assert_eq!(result.response["resource"], json!([{ "id": 1 }]));

    let account_session = PyreSession::new(
        json!({ "scope": { "_type": "Account", "accountId": 7 } }),
        &manifest.session_schema,
    )?;
    assert_eq!(
        account_session.sql_args()["session_scope"],
        json!("Account")
    );
    assert_eq!(account_session.sql_args()["session_scope__id"], json!(null));
    let result = query::run(
        &conn,
        &manifest,
        &query_manifest.id,
        json!({}),
        &account_session,
    )
    .await?;
    assert_eq!(result.response["resource"], json!([]));

    assert!(PyreSession::new(
        json!({ "scope": { "_type": "Workspace" } }),
        &manifest.session_schema,
    )
    .is_err());
    assert!(PyreSession::new(
        json!({ "scope": { "_type": "Unknown", "id": 7 } }),
        &manifest.session_schema,
    )
    .is_err());

    Ok(())
}

#[tokio::test]
async fn run_query_binds_recursive_tagged_union_session_paths(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type SessionScope
    = Leaf {
        id Int
    }
    | Next {
        next SessionScope?
    }

session {
    root SessionScope
}

record Resource {
    id Id.Int @id
    workspaceId Int
    @allow(query) { workspaceId == Session.root.Next.next.Leaf.id }
    @allow(insert, update, delete) { False }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into resources (id, workspaceId) values (1, 7);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        "query ListResources { resource { id } }",
        false,
    )?;
    let query_manifest = only_query(&manifest);
    assert!(manifest.session_schema["root"]
        .tagged_union_types
        .contains_key("SessionScope"));

    let session = PyreSession::new(
        json!({
            "root": {
                "_type": "Next",
                "next": { "_type": "Leaf", "id": 7 }
            }
        }),
        &manifest.session_schema,
    )?;
    assert_eq!(session.sql_args()["session_root__next__id"], json!(7));
    let result = query::run(&conn, &manifest, &query_manifest.id, json!({}), &session).await?;
    assert_eq!(result.response["resource"], json!([{ "id": 1 }]));

    let inactive = PyreSession::new(
        json!({ "root": { "_type": "Leaf", "id": 7 } }),
        &manifest.session_schema,
    )?;
    let result = query::run(&conn, &manifest, &query_manifest.id, json!({}), &inactive).await?;
    assert_eq!(result.response["resource"], json!([]));

    Ok(())
}

#[tokio::test]
async fn generated_update_roundtrips_omittable_unit_enum_input(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Status
    = Running
    | Stopped

record Game {
    @public
    id Id.Int @id
    status Status?
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into games (id, status) values (1, 'Running');")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query ListGames {
    game {
        id
        status
    }
}
"#,
        true,
    )?;
    let update = query_by_operation(&manifest, "update");
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    query::run(&conn, &manifest, &update.id, json!({ "id": 1 }), &session).await?;
    let mut rows = conn
        .query("select status from games where id = 1", ())
        .await?;
    assert_eq!(
        rows.next().await?.expect("game exists").get::<String>(0)?,
        "Running"
    );

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": 1, "status": { "_type": "Stopped" } }),
        &session,
    )
    .await?;
    let mut rows = conn
        .query("select status from games where id = 1", ())
        .await?;
    assert_eq!(
        rows.next().await?.expect("game exists").get::<String>(0)?,
        "Stopped"
    );

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": 1, "status": null }),
        &session,
    )
    .await?;
    let mut rows = conn
        .query("select status from games where id = 1", ())
        .await?;
    assert!(rows
        .next()
        .await?
        .expect("game exists")
        .get::<Option<String>>(0)?
        .is_none());

    Ok(())
}

#[tokio::test]
async fn run_mutation_roundtrips_recursive_union_json_payloads(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Action
    = Create { title String }
    | Delete { id Int }

type Tree
    = Leaf { action Action }
    | Branch { children Json<List<Tree>>, actions Json<Dict<Action>> }

record TreeDocument {
    @public
    id Id.Int @id
    tree Tree
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateTreeDocument($tree: Tree) {
    treeDocument {
        tree = $tree
    }
}
"#,
        false,
    )?;
    let tree = json!({
        "_type": "Branch",
        "children": [{
            "_type": "Leaf",
            "action": { "_type": "Create", "title": "child" }
        }],
        "actions": {
            "deleted": { "_type": "Delete", "id": 7 }
        }
    });
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "tree": tree.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["treeDocument"][0]["tree"], tree);

    Ok(())
}

#[tokio::test]
async fn run_select_query_formats_response() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, body, updatedAt) values (1, 'one', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await?;

    assert_eq!(result.response["note"][0]["id"], json!(1));
    assert_eq!(result.response["note"][0]["body"], json!("one"));
    assert!(result.affected_rows.is_empty());

    Ok(())
}

#[tokio::test]
async fn run_insert_mutation_returns_result_and_extracts_affected_rows_in_sync_mode(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Id.Uuid @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateNote($id: Note.id, $body: String) {
    note {
        id = $id
        body = $body
        updatedAt = 10
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": "00000000-0000-0000-0000-000000000101", "body": "one" }),
        &session,
    )
    .await?;

    assert_eq!(
        result.response["note"][0]["id"],
        json!("00000000-0000-0000-0000-000000000101")
    );
    assert_eq!(result.response["note"][0]["body"], json!("one"));
    assert_eq!(result.affected_rows.len(), 1);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(result.affected_rows[0].rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn run_transaction_returns_all_steps_and_aggregates_affected_rows(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Id.Uuid @id
    body String
    updatedAt Int
    @public
}
record Counter {
    id Id.Uuid @id
    value Int
    updatedAt Int
    @public
}
record Pending {
    id Id.Uuid @id
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into counters (id, value, updatedAt) values ('00000000-0000-0000-0000-000000000102', 0, 10);\n\
         insert into pendings (id, updatedAt) values ('00000000-0000-0000-0000-000000000103', 10);",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
transaction Apply($noteId: Note.id, $counterId: Counter.id, $pendingId: Pending.id, $body: String, $value: Int) {
    insert created: note {
        id = $noteId
        body = $body
        updatedAt = 10
    }
    update changed: counter {
        @where { id == $counterId }
        value = $value
        id
    }
    delete removed: pending {
        @where { id == $pendingId }
        id
    }
}
"#,
        false,
    )?;
    let transaction = only_query(&manifest);
    assert_eq!(transaction.operation, "transaction");
    query::validate_remote_manifest(&manifest)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &transaction.id,
        json!({
            "noteId": "00000000-0000-0000-0000-000000000104",
            "counterId": "00000000-0000-0000-0000-000000000102",
            "pendingId": "00000000-0000-0000-0000-000000000103",
            "body": "one",
            "value": 2
        }),
        &session,
    )
    .await?;

    assert_eq!(result.response["created"][0]["body"], json!("one"));
    assert_eq!(result.response["changed"][0]["value"], json!(2));
    assert_eq!(
        result.response["removed"][0]["id"],
        json!("00000000-0000-0000-0000-000000000103")
    );
    assert_eq!(result.affected_rows.len(), 3);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(result.affected_rows[1].table_name, "counters");
    assert_eq!(result.affected_rows[2].table_name, "pendings");
    Ok(())
}

#[tokio::test]
async fn run_transaction_preserves_aliases_zero_rows_and_shared_session_permissions(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(*) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into notes (id, ownerId, body, updatedAt) values (1, 7, 'old', 10);\n\
         insert into notes (id, ownerId, body, updatedAt) values (2, 8, 'private', 10);",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
transaction ChangeNotes($body: String) {
    update changed: note {
        @where { id == 1 }
        body = $body
        id
    }
    delete denied: note {
        @where { id == 2 }
        id
    }
    insert created: note {
        ownerId = Session.userId
        body = $body
        updatedAt = 10
        id
    }
}
"#,
        false,
    )?;
    let transaction = only_query(&manifest);
    assert_eq!(transaction.session_args, vec!["userId"]);
    let session = PyreSession::new(json!({ "userId": 7 }), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &transaction.id,
        json!({ "body": "changed" }),
        &session,
    )
    .await?;

    assert_eq!(result.response["changed"][0]["body"], json!("changed"));
    assert_eq!(result.response["denied"], json!([]));
    assert_eq!(result.response["created"][0]["ownerId"], json!(7));
    let mut rows = conn
        .query("select body from notes where id = 2", ())
        .await?;
    assert_eq!(
        rows.next()
            .await?
            .expect("denied row should remain")
            .get::<String>(0)?,
        "private"
    );
    Ok(())
}

#[tokio::test]
async fn run_transaction_rolls_back_on_late_unique_constraint_failure(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Note {
    id Int @id
    body String @unique
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute(
        "insert into notes (id, body, updatedAt) values (1, 'taken', 10)",
        (),
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
transaction CreateNotes {
    insert first: note {
        body = "one"
        updatedAt = 10
        id
    }
    insert duplicate: note {
        body = "taken"
        updatedAt = 10
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await
    .expect_err("duplicate insert should fail the transaction");
    let mut rows = conn.query("select body from notes order by id", ()).await?;
    let row = rows.next().await?.expect("original row should remain");
    assert_eq!(row.get::<String>(0)?, "taken");
    assert!(rows.next().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn failed_nested_transaction_rolls_back_without_sync_publication(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Parent {
    id Id.Uuid @id
    name String
    updatedAt Int
    children @link(Child.parentId)
    @public
}

record Child {
    id Id.Uuid @id
    parentId Parent.id
    slug String @unique
    updatedAt Int
    parent @link(parentId, Parent.id)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into parents (id, name, updatedAt) values ('00000000-0000-0000-0000-000000000105', 'baseline', 10);\n\
         insert into children (id, parentId, slug, updatedAt) values ('00000000-0000-0000-0000-000000000106', '00000000-0000-0000-0000-000000000105', 'taken', 10);",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
transaction CreateParents($firstParentId: Parent.id, $firstChildId: Child.id, $secondParentId: Parent.id, $secondChildId: Child.id) {
    insert first: parent {
        id = $firstParentId
        name = "first"
        updatedAt = 10
        children {
            id = $firstChildId
            slug = "ok"
            updatedAt = 10
        }
    }
    insert second: parent {
        id = $secondParentId
        name = "second"
        updatedAt = 10
        children {
            id = $secondChildId
            slug = "taken"
            updatedAt = 10
        }
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let connected_sessions = ConnectedSessions::from([("client".to_string(), SyncSession::new())]);

    let messages = match query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({
            "firstParentId": "00000000-0000-0000-0000-000000000107",
            "firstChildId": "00000000-0000-0000-0000-000000000108",
            "secondParentId": "00000000-0000-0000-0000-000000000109",
            "secondChildId": "00000000-0000-0000-0000-000000000110"
        }),
        &session,
    )
    .await
    {
        Ok(mut result) => {
            SyncServer::new(&db.context)
                .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
                .await?
        }
        Err(_) => Vec::new(),
    };

    assert!(messages.is_empty());
    let mut rows = conn
        .query("select name from parents order by id", ())
        .await?;
    assert_eq!(
        rows.next()
            .await?
            .expect("baseline parent")
            .get::<String>(0)?,
        "baseline"
    );
    assert!(rows.next().await?.is_none());
    let mut rows = conn
        .query("select slug from children order by id", ())
        .await?;
    assert_eq!(
        rows.next()
            .await?
            .expect("baseline child")
            .get::<String>(0)?,
        "taken"
    );
    assert!(rows.next().await?.is_none());
    let mut rows = conn
        .query("select server_revision from _pyre_sync where id = 1", ())
        .await?;
    assert_eq!(rows.next().await?.expect("sync row").get::<i64>(0)?, 0);
    Ok(())
}

#[tokio::test]
async fn run_nested_insert_is_atomic() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(&format!(
        "@syncable(false)\n{}",
        helpers::schema::full_schema()
    ))
    .await?;
    let conn = db.db.connect()?;
    let query_source = r#"
insert CreateUserWithPost($name: String, $status: Status) {
    user {
        name = $name
        status = $status
        posts {
            title = "First Post"
            content = "Body"
        }
    }
}
"#;
    let manifest = manifest_for(&db.context, query_source, false)?;
    let query_id = only_query(&manifest).id.clone();
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let result = query::run(
        &conn,
        &manifest,
        &query_id,
        json!({ "name": "Alice", "status": { "_type": "Active" } }),
        &session,
    )
    .await?;

    assert_eq!(result.response["user"][0]["name"], json!("Alice"));
    let mut rows = conn
        .query("select count(*) from posts where title = 'First Post'", ())
        .await?;
    assert_eq!(rows.next().await?.expect("count row").get::<i64>(0)?, 1);

    let remote_error = query::validate_remote_manifest(&manifest)
        .expect_err("nested inserts should be rejected for remote libSQL");
    assert!(remote_error.to_string().contains(&query_id));
    assert!(remote_error.to_string().contains("nested inserts"));

    Ok(())
}

#[tokio::test]
async fn run_nested_insert_rolls_back_parent_after_child_failure(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(&format!(
        "@syncable(false)\n{}",
        helpers::schema::full_schema()
    ))
    .await?;
    let conn = db.db.connect()?;
    let query_source = r#"
insert CreateUserWithPost($name: String, $status: Status) {
    user {
        name = $name
        status = $status
        posts {
            title = "First Post"
            content = "Body"
        }
    }
}
"#;
    let mut manifest = manifest_for(&db.context, query_source, false)?;
    let query_id = only_query(&manifest).id.clone();
    let mutation = manifest
        .queries
        .get_mut(&query_id)
        .expect("nested insert manifest");
    let child_insert = mutation
        .sql
        .iter_mut()
        .find(|statement| statement.sql.trim_start().starts_with("insert into posts"))
        .expect("generated child insert");
    child_insert.sql = "insert into missing_nested_insert_table values (1)".to_string();
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    query::run(
        &conn,
        &manifest,
        &query_id,
        json!({ "name": "Alice", "status": { "_type": "Active" } }),
        &session,
    )
    .await
    .expect_err("child insert should fail");

    let mut rows = conn.query("select count(*) from users", ()).await?;
    assert_eq!(rows.next().await?.expect("count row").get::<i64>(0)?, 0);

    Ok(())
}

#[tokio::test]
async fn run_query_applies_json_and_session_args() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Id.Uuid @id
    ownerId Int
    attrs Json
    updatedAt Int
    @allow(*) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateNote($id: Note.id, $attrs: Json) {
    note {
        id = $id
        ownerId = Session.userId
        attrs = $attrs
        updatedAt = 10
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({ "userId": 7 }), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({
            "id": "00000000-0000-0000-0000-000000000111",
            "attrs": { "theme": "forest" }
        }),
        &session,
    )
    .await?;

    assert_eq!(result.affected_rows[0].rows.len(), 1);
    let mut rows = conn
        .query("select json(attrs) from notes where ownerId = 7", ())
        .await?;
    let row = rows.next().await?.expect("inserted row should exist");
    let attrs = serde_json::from_str::<serde_json::Value>(&row.get::<String>(0)?)?;
    assert_eq!(attrs, json!({ "theme": "forest" }));

    Ok(())
}

#[tokio::test]
async fn generated_update_respects_omitted_vs_null_optional_args(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Note {
    id Int @id
    body String?
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, body, updatedAt) values (1, 'old', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        true,
    )?;
    let update = query_by_operation(&manifest, "update");
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    query::run(&conn, &manifest, &update.id, json!({ "id": 1 }), &session).await?;
    let after_omitted = query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "query").id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(after_omitted.response["note"][0]["body"], json!("old"));

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": 1, "body": null }),
        &session,
    )
    .await?;
    let after_null = query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "query").id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(after_null.response["note"][0]["body"], json!(null));

    Ok(())
}

#[tokio::test]
async fn run_query_reports_unknown_and_invalid_input() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNote($id: Int) {
    note {
        @where { id == $id }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let unknown = query::run(&conn, &manifest, "missing", json!({}), &session)
        .await
        .expect_err("unknown query should fail");
    assert_eq!(unknown.to_string(), "unknown query: missing");

    let invalid = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": "nope" }),
        &session,
    )
    .await
    .expect_err("invalid input should fail");
    assert_eq!(
        invalid.to_string(),
        "invalid input: input field 'id' must be Int"
    );

    Ok(())
}

#[tokio::test]
async fn run_query_reports_invalid_session() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
    @allow(insert, update, delete) { False }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        false,
    )?;
    let empty_schema = Default::default();
    let session = PyreSession::new(json!({}), &empty_schema)?;

    let err = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await
    .expect_err("missing session arg should fail");

    assert_eq!(
        err.to_string(),
        "invalid session: missing session field 'userId'"
    );

    Ok(())
}

#[tokio::test]
async fn run_query_handles_parameter_names_with_shared_prefixes(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Note {
    id Int @id
    id2 Int
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, id2, body, updatedAt) values (1, 2, 'one', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNote($id: Int, $id2: Int) {
    note {
        @where { id == $id && id2 == $id2 }
        id
        id2
        body
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": 1, "id2": 2 }),
        &session,
    )
    .await?;

    assert_eq!(result.response["note"][0]["body"], json!("one"));

    Ok(())
}

#[tokio::test]
async fn run_delete_mutation_extracts_affected_rows_in_sync_mode(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Id.Uuid @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, body, updatedAt) values ('00000000-0000-0000-0000-000000000112', 'one', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
delete DeleteNote($id: Note.id) {
    note {
        @where { id == $id }
        id
        body
        updatedAt
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": "00000000-0000-0000-0000-000000000112" }),
        &session,
    )
    .await?;

    assert_eq!(result.affected_rows.len(), 1);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(
        result.affected_rows[0].rows[0][0],
        json!("00000000-0000-0000-0000-000000000112")
    );
    let mut rows = conn.query("select count(*) from notes", ()).await?;
    let row = rows.next().await?.expect("count row should exist");
    assert_eq!(row.get::<i64>(0)?, 0);

    Ok(())
}

#[tokio::test]
async fn generated_crud_create_and_delete_run_through_manifest_runtime(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Id.Uuid @id
    body String
    updatedAt DateTime @default(now)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        true,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let delete = query_by_operation(&manifest, "delete");
    assert_eq!(
        create
            .generated_edit
            .as_ref()
            .unwrap()
            .create_uuid_input
            .as_deref(),
        Some("id")
    );

    let created = query::run(
        &conn,
        &manifest,
        &create.id,
        json!({
            "id": "01890f6c-7b80-7000-8000-000000000010",
            "body": "generated"
        }),
        &session,
    )
    .await?;
    assert_eq!(created.response["note"][0]["body"], json!("generated"));
    assert!(created.affected_rows.is_empty());

    let deleted = query::run_sync(
        &conn,
        &manifest,
        &delete.id,
        json!({ "id": created.response["note"][0]["id"] }),
        &session,
    )
    .await?;
    assert_eq!(deleted.affected_rows.len(), 1);
    let remaining = query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "query").id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(remaining.response["note"], json!([]));

    Ok(())
}

#[tokio::test]
async fn generated_crud_runtime_excludes_immutable_update_inputs(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Document {
    id      Int @id
    ownerId Int @immutable
    title   String
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let update = query_by_operation(&manifest, "update");

    assert!(create.input_schema.contains_key("ownerId"));
    assert!(!update.input_schema.contains_key("ownerId"));
    assert!(!update.optional_input_args.contains(&"ownerId".to_string()));

    let created = query::run(
        &conn,
        &manifest,
        &create.id,
        json!({ "ownerId": 7, "title": "draft" }),
        &session,
    )
    .await?;
    let id = created.response["document"][0]["id"].clone();
    assert_eq!(created.response["document"][0]["ownerId"], json!(7));

    let updated = query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": id, "title": "published" }),
        &session,
    )
    .await?;
    assert_eq!(updated.response["document"][0]["ownerId"], json!(7));
    assert_eq!(updated.response["document"][0]["title"], json!("published"));

    let error = query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": id, "ownerId": 9 }),
        &session,
    )
    .await
    .expect_err("immutable update input should be unknown");
    assert!(error.to_string().contains("unknown input field 'ownerId'"));

    Ok(())
}

#[tokio::test]
async fn generated_crud_creates_and_returns_immutable_tagged_union(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type ActionState
   = Open { startedAt String }
   | Completed { completedAt String }

record Action {
    @public
    id    Id.Uuid @id
    state ActionState @immutable
    title String
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let update = query_by_operation(&manifest, "update");

    assert!(create.input_schema.contains_key("state"));
    assert_eq!(
        create
            .generated_edit
            .as_ref()
            .unwrap()
            .create_uuid_input
            .as_deref(),
        Some("id")
    );
    assert_eq!(create.json_input_args, vec!["state"]);
    assert!(!update.input_schema.contains_key("state"));
    assert!(!update.json_input_args.contains(&"state".to_string()));

    let created = query::run_sync(
        &conn,
        &manifest,
        &create.id,
        json!({
            "id": "01890f6c-7b80-7000-8000-000000000011",
            "state": { "_type": "Open", "startedAt": "now" },
            "title": "first"
        }),
        &session,
    )
    .await?;
    assert_eq!(
        created.response["action"][0]["state"],
        json!({ "_type": "Open", "startedAt": "now" })
    );

    let mut rows = conn
        .query("select state, state__startedAt from actions", ())
        .await?;
    let row = rows.next().await?.expect("created action exists");
    assert_eq!(row.get::<String>(0)?, "Open");
    assert_eq!(row.get::<String>(1)?, "now");
    Ok(())
}

#[tokio::test]
async fn nested_insert_populates_immutable_parent_foreign_key(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Parent {
    @public
    id       Id.Uuid @id
    name     String
    children @link(Child.parentId)
}

record Child {
    @public
    id       Id.Uuid @id
    parentId Parent.id @immutable
    body     String
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateParent($parentId: Parent.id, $childId: Child.id, $name: String, $body: String) {
    parent {
        id = $parentId
        name = $name
        children {
            id = $childId
            body = $body
        }
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let inserted = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({
            "parentId": "00000000-0000-0000-0000-000000000113",
            "childId": "00000000-0000-0000-0000-000000000114",
            "name": "parent",
            "body": "child"
        }),
        &session,
    )
    .await?;
    let parent_id = inserted.response["parent"][0]["id"]
        .as_str()
        .expect("parent id is returned");

    let mut rows = conn
        .query("select parentId, body from children", ())
        .await?;
    let row = rows.next().await?.expect("nested child exists");
    assert_eq!(row.get::<String>(0)?, parent_id);
    assert_eq!(row.get::<String>(1)?, "child");
    Ok(())
}

#[tokio::test]
async fn generated_crud_create_enforces_insert_permission() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(
        r#"
@syncable(false)
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    @allow(query, insert, update, delete) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let session = PyreSession::new(json!({ "userId": 1 }), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");

    query::run(
        &conn,
        &manifest,
        &create.id,
        json!({ "ownerId": 2, "body": "denied" }),
        &session,
    )
    .await?;

    let mut rows = conn.query("select count(*) from notes", ()).await?;
    let row = rows.next().await?.expect("count row should exist");
    assert_eq!(row.get::<i64>(0)?, 0);
    Ok(())
}

#[tokio::test]
async fn run_insert_mutation_binds_repeated_json_union_parameter(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Visibility
   = Hidden
   | Everyone
   | Users {
        userId   Int
        metadata Json<Dict<String>>
     }

record Scene {
    id Int @id
    visibility Visibility
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateScene($visibility: Visibility) {
    scene {
        visibility = $visibility
        updatedAt = 10
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    assert_eq!(only_query(&manifest).json_input_args, vec!["visibility"]);

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({
            "visibility": {
                "_type": "Users",
                "userId": 42,
                "metadata": { "source": "import" }
            }
        }),
        &session,
    )
    .await?;

    assert_eq!(result.response["scene"][0]["id"], json!(1));

    let mut rows = conn
        .query(
            "select visibility, visibility__userId, visibility__metadata from scenes",
            (),
        )
        .await?;
    let row = rows.next().await?.expect("scene row should exist");
    assert_eq!(row.get::<String>(0)?, "Users");
    assert_eq!(row.get::<i64>(1)?, 42);
    assert_eq!(row.get::<String>(2)?, r#"{"source":"import"}"#);

    Ok(())
}

#[tokio::test]
async fn literal_union_insert_binds_null_for_omitted_nullable_fields(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Lifecycle
   = Running
   | Finished {
        reason  String
        metadata Json<Dict<String>>?
     }

record Session {
    @public
    id        Int @id
    lifecycle Lifecycle
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateSession {
    session {
        lifecycle = Finished {
            reason = "done"
        }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await?;

    assert_eq!(
        result.response["session"][0]["lifecycle"]["_type"],
        "Finished"
    );
    assert_eq!(result.response["session"][0]["lifecycle"]["reason"], "done");
    let mut rows = conn
        .query("select lifecycle__metadata from sessions", ())
        .await?;
    let row = rows.next().await?.expect("session row should exist");
    assert!(matches!(row.get_value(0)?, libsql::Value::Null));

    Ok(())
}

#[tokio::test]
async fn nullable_typed_json_null_roundtrips_and_matches_predicates(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Ended
   = Ended {
        reason String
     }

record ClocktowerLifecycle {
    @public
    id     Id.Uuid @id
    end    Json<Ended>?
    status String
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let session = PyreSession::new(
        json!({}),
        &manifest_for(&db.context, "", true)?.session_schema,
    )?;
    let id = "7f4d3a18-8b6c-4e2f-a901-5c7d9e3b2f60";

    let insert_manifest = manifest_for(
        &db.context,
        r#"
insert SeedLifecycle($id: ClocktowerLifecycle.id, $end: Json<Ended>?) {
    clocktowerLifecycle {
        id = $id
        end = $end
        status = "running"
    }
}
"#,
        false,
    )?;
    query::run(
        &conn,
        &insert_manifest,
        &only_query(&insert_manifest).id,
        json!({ "id": id, "end": null }),
        &session,
    )
    .await?;

    let mut storage_rows = conn
        .query("select typeof(\"end\") from clocktowerLifecycles", ())
        .await?;
    let storage_row = storage_rows
        .next()
        .await?
        .expect("inserted row should exist");
    assert_eq!(storage_row.get::<String>(0)?, "null");

    let read_manifest = manifest_for(
        &db.context,
        r#"
query GetLifecycles {
    clocktowerLifecycle {
        id
        end
        status
    }
}
"#,
        false,
    )?;
    let read = query::run(
        &conn,
        &read_manifest,
        &only_query(&read_manifest).id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(
        read.response["clocktowerLifecycle"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(read.response["clocktowerLifecycle"][0]["end"].is_null());

    let filtered_manifest = manifest_for(
        &db.context,
        r#"
query GetNullLifecycles {
    clocktowerLifecycle {
        @where { end == null }
        id
    }
}
"#,
        false,
    )?;
    assert!(only_query(&filtered_manifest).sql[0]
        .sql
        .contains(".\"end\" is null"));
    let filtered = query::run(
        &conn,
        &filtered_manifest,
        &only_query(&filtered_manifest).id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(
        filtered.response["clocktowerLifecycle"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let update_manifest = manifest_for(
        &db.context,
        r#"
update EndLifecycle {
    clocktowerLifecycle {
        @where { end == null }
        status = "ended"
        id
    }
}
"#,
        false,
    )?;
    let updated = query::run_sync(
        &conn,
        &update_manifest,
        &only_query(&update_manifest).id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(updated.affected_rows.len(), 1);
    assert_eq!(updated.affected_rows[0].rows.len(), 1);

    let mut status_rows = conn
        .query("select status from clocktowerLifecycles where id = ?", [id])
        .await?;
    let status_row = status_rows.next().await?.expect("updated row should exist");
    assert_eq!(status_row.get::<String>(0)?, "ended");

    Ok(())
}

#[tokio::test]
async fn generated_crud_reuses_shared_tagged_union_columns(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type ActionState
   = Open {
        startedAt String?
     }
   | Ongoing
   | Completed {
        startedAt   String?
        completedAt String
     }

record Action {
    @public
    id    Id.Uuid @id
    state ActionState
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(&db.context, "", true)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let update = query_by_operation(&manifest, "update");

    assert_eq!(create.json_input_args, vec!["state"]);
    assert_eq!(
        create
            .generated_edit
            .as_ref()
            .unwrap()
            .create_uuid_input
            .as_deref(),
        Some("id")
    );
    assert_eq!(update.json_input_args, vec!["state"]);
    assert!(!create.sql[0]
        .sql
        .contains("state__startedAt, state__startedAt"));
    assert_eq!(update.sql[0].sql.matches("state__startedAt =").count(), 1);

    let created = query::run_sync(
        &conn,
        &manifest,
        &create.id,
        json!({
            "id": "01890f6c-7b80-7000-8000-000000000012",
            "state": { "_type": "Open", "startedAt": "first" }
        }),
        &session,
    )
    .await?;
    assert_eq!(created.affected_rows.len(), 1);
    assert_eq!(
        created.affected_rows[0]
            .headers
            .iter()
            .filter(|header| header.as_str() == "state__startedAt")
            .count(),
        1
    );

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({
            "id": "01890f6c-7b80-7000-8000-000000000012",
            "state": {
                "_type": "Completed",
                "startedAt": "first",
                "completedAt": "second"
            }
        }),
        &session,
    )
    .await?;

    let mut rows = conn
        .query(
            "select state, state__startedAt, state__completedAt from actions",
            (),
        )
        .await?;
    let row = rows.next().await?.expect("action row should exist");
    assert_eq!(row.get::<String>(0)?, "Completed");
    assert_eq!(row.get::<String>(1)?, "first");
    assert_eq!(row.get::<String>(2)?, "second");

    Ok(())
}

#[tokio::test]
async fn run_multi_top_level_query_formats_all_response_keys(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record User {
    id Int @id
    name String
    updatedAt Int
    @public
}

record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into users (id, name, updatedAt) values (1, 'Ada', 10); insert into notes (id, body, updatedAt) values (1, 'one', 10);",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query Dashboard {
    user {
        id
        name
    }
    note {
        id
        body
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await?;

    assert_eq!(result.response["user"][0]["name"], json!("Ada"));
    assert_eq!(result.response["note"][0]["body"], json!("one"));

    Ok(())
}

#[cfg(feature = "filesystem")]
#[test]
fn manifest_load_reads_generated_manifest_file() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Manifest {
        replacement_contracts: Default::default(),
        compiled_contract: String::new(),
        version: 1,
        session_schema: Default::default(),
        queries: Default::default(),
    };
    let dir = tempfile::TempDir::new()?;
    let path = dir.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_string(&manifest)?)?;

    let loaded = Manifest::load(&path)?;

    assert_eq!(loaded.version, 1);
    assert!(loaded.queries.is_empty());

    Ok(())
}
#[test]
fn generated_manifest_validates_integer_session_foreign_keys() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
@syncable(false)
session {
    userId User.id
}

record User {
    @public
    id Id.Int @id
}

record Item {
    id Int @id
    ownerId User.id
    @allow(query) { ownerId == Session.userId }
    @allow(insert, update, delete) { False }
}
"#,
        &mut schema,
    )
    .unwrap();
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).unwrap();
    let mut files = Vec::new();
    pyre::generate::manifest::generate_schema(&context, &mut files);
    let manifest: Manifest = serde_json::from_str(
        &files
            .iter()
            .find(|file| file.path.ends_with("manifest.json"))
            .expect("generated manifest")
            .contents,
    )
    .unwrap();

    assert!(manifest.session_schema["userId"]
        .type_
        .starts_with("Id.Int"));
    assert!(PyreSession::new(json!({ "userId": 7 }), &manifest.session_schema).is_ok());
    assert!(PyreSession::new(json!({ "userId": 7.5 }), &manifest.session_schema).is_err());
}

#[test]
fn session_validation_preserves_extra_claims_and_supported_codec_values() {
    let schema: std::collections::HashMap<String, pyre::server::manifest::FieldSchema> =
        serde_json::from_value(json!({
            "count": {"type":"Int", "nullable":false, "omittable":false},
            "enabled": {"type":"Bool", "nullable":false, "omittable":false},
            "date": {"type":"Date", "nullable":false, "omittable":false},
            "timestamp": {"type":"DateTime", "nullable":false, "omittable":false},
            "ids": {"type":"Json<List<Int>>", "nullable":true, "omittable":true},
            "scope": {"type":"Scope", "nullable":false, "omittable":false,
                "tagged_union_variants": {"Member": {
                    "id": {"type":"Int", "nullable":false, "omittable":false},
                    "note": {"type":"String", "nullable":true, "omittable":false}
                }}
            }
        }))
        .unwrap();
    let input = json!({"count":1.0,"enabled":1.0,"date":"date-valued-string", "timestamp":1.0,
        "scope":{"_type":"Member","id":7,"extra":"ignored"}, "applicationClaim":"not a SQL arg"});
    let session = PyreSession::new(input.clone(), &schema).unwrap();
    assert_eq!(session.sql_args()["session_count"], json!(1));
    assert_eq!(session.sql_args()["session_enabled"], json!(1));
    assert_eq!(session.sql_args()["session_timestamp"], json!(1));
    assert_eq!(session.sql_args()["session_scope__note"], json!(null));
    assert!(!session.sql_args().contains_key("session_applicationClaim"));
    assert!(!session.sql_args().contains_key("session_scope__extra"));
    assert!(session.revalidate(&schema).is_ok());
    for (key, value) in [
        ("count", json!(1.5)),
        ("count", json!(9_007_199_254_740_992_i64)),
        ("enabled", json!(2)),
        ("date", json!(1)),
        ("timestamp", json!(8_640_000_000_001_i64)),
        ("timestamp", json!("2016-12-31T23:59:60Z")),
        ("ids", json!([1, "bad"])),
        ("scope", json!({"_type":"Member"})),
    ] {
        let mut invalid = input.clone();
        invalid[key] = value;
        assert!(PyreSession::new(invalid, &schema).is_err(), "invalid {key}");
    }
}
