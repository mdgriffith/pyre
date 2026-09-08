use pyre::server::schema::{ensure_database, EnsureDatabaseOutcome};
use pyre::server::sync::rotate_database_epoch;

const SCHEMA: &str = "record Note {\n    @public\n    id Id.Int @id\n    body String\n}\n";

#[tokio::test]
async fn catchup_fence_cannot_pass_an_uncommitted_timestamped_writer() {
    let directory = tempfile::tempdir().unwrap();
    let db = libsql::Builder::new_local(directory.path().join("fence.db")).build().await.unwrap();
    let writer = db.connect().unwrap();
    writer.execute_batch("pragma journal_mode = wal").await.unwrap();
    ensure_database(&writer, "Db", SCHEMA).await.unwrap();
    let loaded = pyre::server::schema::load_context_from_database(&writer).await.unwrap();
    let server = pyre::server::sync::SyncServer::new(loaded.context().unwrap());
    let reader = db.connect().unwrap();
    reader.execute_batch("pragma busy_timeout = 0").await.unwrap();
    let tx = writer.transaction_with_behavior(libsql::TransactionBehavior::Immediate).await.unwrap();
    tx.execute("insert into notes (id, body, updatedAt) values (1, 'in flight', 10)", ()).await.unwrap();
    assert!(server.catchup_durable(&reader, &Default::default(), &Default::default(), 1, "main", None).await.is_err());
    tx.commit().await.unwrap();
    let result = server.catchup_durable(&reader, &Default::default(), &Default::default(), 1, "main", None).await.unwrap();
    assert_eq!(result["tables"]["notes"]["changes"][0]["row"]["body"], "in flight");
    assert!(result["snapshotTimestamp"].as_i64().unwrap() >= 10);
    // The page transaction released its writer lock.
    writer.execute("update notes set body = 'after snapshot'", ()).await.unwrap();
}

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap()
}

#[tokio::test]
async fn tombstones_are_typed_durable_and_transactional() {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", SCHEMA).await.unwrap();
    conn.execute(
        "insert into notes (id, body) values (1, 'private contents')",
        (),
    )
    .await
    .unwrap();
    let tx = conn.transaction().await.unwrap();
    tx.execute("delete from notes", ()).await.unwrap();
    assert_eq!(
        scalar(&tx, "select count(*) from _pyre_sync_tombstones").await,
        1
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        scalar(&conn, "select count(*) from _pyre_sync_tombstones").await,
        0
    );
    conn.execute("delete from notes", ()).await.unwrap();
    let mut rows = conn.query("select sequence, table_name, primary_key, typeof(primary_key) from _pyre_sync_tombstones", ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 2);
    assert_eq!(row.get::<String>(1).unwrap(), "notes");
    assert_eq!(row.get::<i64>(2).unwrap(), 1);
    assert_eq!(row.get::<String>(3).unwrap(), "integer");
    assert_eq!(
        scalar(
            &conn,
            "select count(*) from pragma_table_info('_pyre_sync_tombstones')"
        )
        .await,
        3
    );
    drop(rows);
    assert_eq!(
        ensure_database(&conn, "Db", SCHEMA).await.unwrap(),
        EnsureDatabaseOutcome::UpToDate
    );
    assert_eq!(
        scalar(&conn, "select count(*) from _pyre_sync_tombstones").await,
        1
    );
    conn.execute("insert into notes (id, body) values (1, 'reinserted')", ())
        .await
        .unwrap();
    conn.execute("delete from notes", ()).await.unwrap();
    assert_eq!(
        scalar(&conn, "select max(sequence) from _pyre_sync_tombstones").await,
        4
    );
}

#[tokio::test]
async fn cascades_capture_tombstones_without_row_permissions() {
    let schema = "record Parent {\n    @public\n    id Id.Int @id\n}\nrecord Child {\n    @allow(query, insert, update, delete) { False }\n    id Id.Uuid @id\n    parentId Int\n}\n";
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", schema).await.unwrap();
    // Recreate with a cascade to verify SQLite-triggered deletes, not only
    // explicit generated DELETE statements. Reinstall the generated triggers.
    conn.execute_batch("pragma foreign_keys = on; drop table children; create table children (id text primary key, parentId integer references parents(id) on delete cascade, updatedAt integer);").await.unwrap();
    let mut parsed = pyre::ast::Schema {
        namespace: "Db".into(),
        ..Default::default()
    };
    pyre::parser::run("schema.pyre", schema, &mut parsed).unwrap();
    let context = pyre::typecheck::check_schema(&pyre::ast::Database {
        schemas: vec![parsed.clone()],
    })
    .unwrap();
    for statement in pyre::db::migrate::sync_tombstone_trigger_sql(&context, &parsed) {
        if let pyre::generate::sql::to_sql::SqlAndParams::Sql(sql) = statement {
            conn.execute(&sql, ()).await.unwrap();
        }
    }
    conn.execute_batch("insert into parents (id) values (7); insert into children (id, parentId) values ('007', 7); delete from parents where id = 7;").await.unwrap();
    assert_eq!(
        scalar(&conn, "select count(*) from _pyre_sync_tombstones").await,
        2
    );
    let mut rows = conn.query("select primary_key, typeof(primary_key) from _pyre_sync_tombstones where table_name = 'children'", ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "007");
    assert_eq!(row.get::<String>(1).unwrap(), "text");
}

#[tokio::test]
async fn epoch_rotation_rolls_back_when_tombstone_clear_fails() {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", SCHEMA).await.unwrap();
    conn.execute_batch("insert into notes (id, body) values (1, 'a'); delete from notes; update _pyre_sync set database_epoch = 'before', server_revision = 23; create trigger fail_clear before delete on _pyre_sync_tombstones begin select raise(abort, 'test failure'); end;").await.unwrap();
    assert!(rotate_database_epoch(&conn).await.is_err());
    assert_eq!(scalar(&conn, "select count(*) from _pyre_sync where database_epoch = 'before' and server_revision = 23").await, 1);
    assert_eq!(
        scalar(&conn, "select count(*) from _pyre_sync_tombstones").await,
        1
    );
    conn.execute("drop trigger fail_clear", ()).await.unwrap();
    let epoch = rotate_database_epoch(&conn).await.unwrap();
    assert_ne!(epoch, "before");
    assert_eq!(
        scalar(&conn, "select server_revision from _pyre_sync").await,
        0
    );
    assert_eq!(
        scalar(&conn, "select count(*) from _pyre_sync_tombstones").await,
        0
    );
    conn.execute_batch("insert into notes (id, body) values (2, 'b'); delete from notes;")
        .await
        .unwrap();
    assert_eq!(
        scalar(&conn, "select sequence from _pyre_sync_tombstones").await,
        2
    );
}

#[tokio::test]
async fn query_only_schema_has_no_delete_triggers() {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", &format!("@syncable(false)\n{SCHEMA}"))
        .await
        .unwrap();
    assert_eq!(scalar(&conn, "select count(*) from sqlite_master where type = 'trigger' and name like '_pyre_delete_%'").await, 0);
}

#[tokio::test]
async fn tombstones_survive_reopening_the_physical_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.db");
    {
        let db = libsql::Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        ensure_database(&conn, "Db", SCHEMA).await.unwrap();
        conn.execute_batch("insert into notes (id, body) values (9, 'a'); delete from notes;")
            .await
            .unwrap();
    }
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();
    assert_eq!(
        scalar(&conn, "select primary_key from _pyre_sync_tombstones").await,
        9
    );
    assert_eq!(scalar(&conn, "select count(*) from notes").await, 0);
}

#[test]
fn trigger_generation_is_scoped_to_one_physical_schema() {
    let mut first = pyre::ast::Schema {
        namespace: "First".into(),
        ..Default::default()
    };
    let mut second = pyre::ast::Schema {
        namespace: "Second".into(),
        ..Default::default()
    };
    pyre::parser::run("first.pyre", SCHEMA, &mut first).unwrap();
    pyre::parser::run(
        "second.pyre",
        "record Secret {\n    @public\n    id Id.Int @id\n}\n",
        &mut second,
    )
    .unwrap();
    let context = pyre::typecheck::check_schema(&pyre::ast::Database {
        schemas: vec![first.clone(), second],
    })
    .unwrap();
    let sql = pyre::db::migrate::sync_tombstone_trigger_sql(&context, &first);
    let rendered = serde_json::to_string(&sql).unwrap();
    assert!(rendered.contains("notes"));
    assert!(!rendered.contains("secrets"));
    assert!(pyre::sync_v2::statement(&context, &Default::default(), &Default::default(), 1).is_err());
}

#[tokio::test]
async fn missing_sync_metadata_aborts_hard_deletes() {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", SCHEMA).await.unwrap();
    conn.execute_batch("insert into notes (id, body) values (1, 'retained'); delete from _pyre_sync;").await.unwrap();
    assert!(conn.execute("delete from notes", ()).await.is_err());
    assert_eq!(scalar(&conn, "select count(*) from notes").await, 1);
}

#[tokio::test]
async fn durable_snapshot_binds_permissions_before_overlapping_timestamp_cursors() {
    use serde_json::json;
    let schema = "session {\n    userId Int\n}\nrecord Note {\n    id Id.Int @id\n    ownerId Int\n    body String\n    @allow(*) { ownerId == Session.userId }\n}\n";
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", schema).await.unwrap();
    let loaded = pyre::server::schema::load_context_from_database(&conn).await.unwrap();
    let server = pyre::server::sync::SyncServer::new(loaded.context().unwrap());
    let session = std::collections::HashMap::from([("userId".into(), pyre::sync::SessionValue::Integer(1))]);
    conn.execute_batch("insert into notes (id, ownerId, body) values (1, 1, 'visible'), (2, 2, 'hidden');").await.unwrap();
    let first = server.catchup_durable(&conn, &Default::default(), &session, 10, "main", None).await.unwrap();
    assert_eq!(first["tables"]["notes"]["changes"].as_array().unwrap().len(), 1);
    conn.execute_batch("update notes set body = 'changed' where id = 1; delete from notes where id = 2;").await.unwrap();
    let mut overlap = cursor(&first);
    overlap.get_mut("notes").unwrap().last_seen_primary_key = None;
    let second = server.catchup_durable(&conn, &overlap, &session, 10, "main", first["databaseEpoch"].as_str()).await.unwrap();
    let changes = second["tables"]["notes"]["changes"].as_array().unwrap();
    assert_eq!(changes[0]["row"]["body"], "changed");
    assert_eq!(changes[1], json!({"op":"delete", "id":2}));
    assert!(!second.to_string().contains("hidden"));
}

async fn page(conn: &libsql::Connection, cursor: &pyre::sync::SyncCursor, size: usize, epoch: Option<&str>) -> serde_json::Value {
    let context = pyre::server::schema::load_context_from_database(conn).await.unwrap();
    pyre::server::sync::SyncServer::new(context.context().unwrap()).catchup_durable(conn, cursor, &Default::default(), size, "main", epoch).await.unwrap()
}

fn cursor(page: &serde_json::Value) -> pyre::sync::SyncCursor {
    serde_json::from_value(page["tables"].clone()).unwrap()
}

#[tokio::test]
async fn durable_catchup_pages_delete_reinsert_and_unknown_keys_without_timestamp_gaps() {
    use serde_json::json;
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", SCHEMA).await.unwrap();
    conn.execute("insert into notes (id, body, updatedAt) values (1, 'old', 10)", ()).await.unwrap();
    let first = page(&conn, &Default::default(), 1, None).await;
    let epoch = first["databaseEpoch"].as_str();
    assert_eq!(page(&conn, &cursor(&first), 1, None).await["type"], "reset");
    assert_eq!(first["tables"]["notes"]["changes"][0]["row"]["body"], "old");
    conn.execute_batch("delete from notes; insert into notes (id, body, updatedAt) values (1, 'new', 10); insert into notes (id, body) values (99, 'never cached'); delete from notes where id = 99;").await.unwrap();
    let deleted = page(&conn, &cursor(&first), 1, epoch).await;
    assert_eq!(deleted["tables"]["notes"]["changes"][0], json!({ "op": "delete", "id": 1 }));
    assert_eq!(deleted["tables"]["notes"]["changes"][1]["row"]["body"], "new");
    assert_eq!(deleted["has_more"], true);
    let unknown = page(&conn, &cursor(&deleted), 1, epoch).await;
    assert_eq!(unknown["tables"]["notes"]["changes"], json!([{ "op": "delete", "id": 99 }]));
    let complete = page(&conn, &cursor(&unknown), 1, epoch).await;
    assert_eq!(complete["tables"]["notes"]["changes"], json!([]));
    assert_eq!(complete["tables"]["notes"]["last_seen_updated_at"], unknown["tables"]["notes"]["last_seen_updated_at"]);
    let old_epoch = first["databaseEpoch"].as_str().unwrap();
    let new_epoch = rotate_database_epoch(&conn).await.unwrap();
    let reset = page(&conn, &cursor(&complete), 1, Some(old_epoch)).await;
    assert_eq!(reset["type"], "reset");
    assert_eq!(reset["databaseEpoch"], new_epoch);
    let baseline = page(&conn, &Default::default(), 1, Some(&new_epoch)).await;
    assert_eq!(baseline["tables"]["notes"]["changes"][0]["row"]["body"], "new");
    assert_eq!(baseline["tables"]["notes"]["last_seen_updated_at"], 10);
}

#[tokio::test]
async fn durable_deletes_do_not_apply_row_permissions_and_preserve_text_keys() {
    use serde_json::json;
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    let schema = "record Secret {\n    id Id.Uuid @id\n    body String\n    @allow(query, insert, update, delete) { False }\n}\n";
    ensure_database(&conn, "Db", schema).await.unwrap();
    conn.execute("insert into secrets (id, body) values ('007', 'must not leak')", ()).await.unwrap();
    let before = page(&conn, &Default::default(), 10, None).await;
    assert_eq!(before["tables"]["secrets"]["changes"], json!([]));
    assert!(before["tables"]["secrets"]["last_seen_updated_at"].is_null());
    assert!(before["tables"]["secrets"]["last_seen_primary_key"].is_null());
    conn.execute("delete from secrets", ()).await.unwrap();
    let after = page(&conn, &cursor(&before), 10, before["databaseEpoch"].as_str()).await;
    assert_eq!(after["tables"]["secrets"]["changes"], json!([{ "op": "delete", "id": "007" }]));
    assert!(!after.to_string().contains("must not leak"));
}

#[tokio::test]
async fn epoch_rotation_preserves_timestamp_pagination_hidden_rows_and_text_keys() {
    use serde_json::json;
    let schema = "session {\n    userId Int\n}\nrecord Note {\n    id Id.Int @id\n    ownerId Int\n    @allow(*) { ownerId == Session.userId }\n}\nrecord Token {\n    @public\n    id Id.Uuid @id\n}\n";
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    ensure_database(&conn, "Db", schema).await.unwrap();
    conn.execute_batch("insert into notes (id, ownerId) values (-1, 1), (0, 2), (1, 1); insert into tokens (id) values ('007'), ('010'), ('100');").await.unwrap();
    let epoch = rotate_database_epoch(&conn).await.unwrap();
    let loaded = pyre::server::schema::load_context_from_database(&conn).await.unwrap();
    let server = pyre::server::sync::SyncServer::new(loaded.context().unwrap());
    let session = std::collections::HashMap::from([("userId".into(), pyre::sync::SessionValue::Integer(1))]);
    let mut position = Default::default();
    for (index, (note, token)) in [(-1, "007"), (1, "010"), (1, "100")].into_iter().enumerate() {
        let result = server.catchup_durable(&conn, &position, &session, 1, "main", Some(&epoch)).await.unwrap();
        assert_eq!(result["serverRevision"], 0);
        assert_eq!(result["has_more"], index < 2);
        assert!(result["tables"]["notes"]["last_seen_updated_at"].as_i64().unwrap() > 0);
        assert_eq!(result["tables"]["notes"]["last_seen_primary_key"], note);
        let changes = &result["tables"]["notes"]["changes"];
        if index == 2 {
            assert_eq!(*changes, json!([]));
        } else {
            assert_eq!(changes[0]["id"], note);
        }
        assert_eq!(result["tables"]["tokens"]["changes"][0]["id"], token);
        position = cursor(&result);
    }
    conn.execute_batch("delete from notes where id = -1; insert into notes (id, ownerId) values (-1, 1); delete from notes where id = -1; insert into notes (id, ownerId) values (-1, 1);").await.unwrap();
    for op in ["delete", "delete"] {
        let result = server.catchup_durable(&conn, &position, &session, 1, "main", Some(&epoch)).await.unwrap();
        assert_eq!(result["tables"]["notes"]["changes"][0]["op"], op);
        assert_eq!(result["tables"]["notes"]["changes"][0]["id"], -1);
        assert_eq!(result["tables"]["notes"]["changes"][1]["op"], "row");
        assert_eq!(result["tables"]["tokens"]["changes"], json!([]));
        position = cursor(&result);
    }
}
