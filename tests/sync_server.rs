#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::manifest::{FieldSchema, PyreSession};
use pyre::server::query::QueryResult;
use pyre::server::schema::{
    load_context_from_database, load_schema_from_database, Error as SchemaError,
};
use pyre::server::sync::{
    catchup, ConnectedSessions, DeltaMessage, SyncServer, SyncSession,
    MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES, MAX_LIVE_SYNC_DELTA_ROWS, MAX_LIVE_SYNC_FANOUT_RECIPIENTS,
};
use pyre::sync::{SyncCursor, TableCursor, TableSyncData};
use pyre::sync_deltas::AffectedRowTableGroup;
use serde_json::json;
use std::collections::HashMap;

fn query_result(affected_rows: Vec<AffectedRowTableGroup>) -> QueryResult {
    QueryResult {
        response: json!({}),
        affected_rows,
    }
}

async fn extract_affected_rows(
    result_sets: Vec<libsql::Rows>,
) -> Result<Vec<AffectedRowTableGroup>, Box<dyn std::error::Error>> {
    for mut rows_set in result_sets {
        let column_count = rows_set.column_count();

        for index in 0..column_count {
            if rows_set.column_name(index) != Some("_affectedRows") {
                continue;
            }

            let Some(row) = rows_set.next().await? else {
                return Ok(Vec::new());
            };
            let raw = row.get::<String>(index as i32)?;
            let value = serde_json::from_str::<serde_json::Value>(&raw)?;
            let groups = value
                .as_array()
                .ok_or("_affectedRows should be an array")?
                .iter()
                .map(|group| {
                    let group_value = match group {
                        serde_json::Value::String(raw) => serde_json::from_str(raw)?,
                        value => value.clone(),
                    };

                    serde_json::from_value::<AffectedRowTableGroup>(group_value)
                })
                .collect::<Result<Vec<_>, serde_json::Error>>()?;

            return Ok(groups);
        }
    }

    Err("missing _affectedRows result set".into())
}

#[tokio::test]
async fn catchup_paginates_and_advances_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
        r#"
insert into notes (id, body, updatedAt) values (1, 'one', 10);
insert into notes (id, body, updatedAt) values (2, 'two', 20);
insert into notes (id, body, updatedAt) values (3, 'three', 30);
"#,
    )
    .await?;

    let session = HashMap::new();
    let first = catchup(&conn, &db.context, &SyncCursor::new(), &session, 2).await?;
    let notes = first.tables.get("notes").expect("notes should sync");

    assert!(first.has_more);
    assert_eq!(notes.rows.len(), 2);
    assert_eq!(notes.rows[0]["id"], json!(1));
    assert_eq!(notes.rows[1]["id"], json!(2));
    assert_eq!(notes.last_seen_updated_at, Some(20));

    let mut cursor = SyncCursor::new();
    cursor.insert(
        "notes".to_string(),
        TableCursor {
            last_seen_updated_at: notes.last_seen_updated_at,
            permission_hash: notes.permission_hash.clone(),
        },
    );

    let second = catchup(&conn, &db.context, &cursor, &session, 10).await?;
    let notes = second
        .tables
        .get("notes")
        .expect("remaining note should sync");

    assert!(!second.has_more);
    assert_eq!(notes.rows.len(), 1);
    assert_eq!(notes.rows[0]["id"], json!(3));
    assert_eq!(notes.last_seen_updated_at, Some(30));

    cursor.insert(
        "notes".to_string(),
        TableCursor {
            last_seen_updated_at: notes.last_seen_updated_at,
            permission_hash: notes.permission_hash.clone(),
        },
    );

    let empty = catchup(&conn, &db.context, &cursor, &session, 10).await?;
    assert!(!empty.has_more);
    assert!(empty.tables.is_empty());

    Ok(())
}

#[test]
fn table_sync_data_serializes_empty_rows() {
    let data = TableSyncData {
        rows: Vec::new(),
        permission_hash: "permission-hash".to_string(),
        last_seen_updated_at: None,
    };

    let serialized = serde_json::to_value(data).expect("table sync data should serialize");

    assert_eq!(serialized["rows"], json!([]));
    assert_eq!(serialized["permission_hash"], json!("permission-hash"));
    assert_eq!(serialized["last_seen_updated_at"], serde_json::Value::Null);
}

#[tokio::test]
async fn catchup_filters_rows_by_session_permissions() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        r#"
insert into notes (id, ownerId, body, updatedAt) values (1, 1, 'one', 10);
insert into notes (id, ownerId, body, updatedAt) values (2, 2, 'two', 20);
insert into notes (id, ownerId, body, updatedAt) values (3, 1, 'three', 30);
"#,
    )
    .await?;

    let server = SyncServer::new(&db.context);
    let mut session = HashMap::new();
    session.insert("userId".to_string(), pyre::sync::SessionValue::Integer(1));

    let result = server
        .catchup(&conn, &SyncCursor::new(), &session, 10, "main")
        .await?;
    let notes = result.tables.get("notes").expect("notes should sync");
    let ids = notes
        .rows
        .iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec![json!(1), json!(3)]);
    assert_eq!(result.database_id.as_deref(), Some("main"));
    assert_eq!(result.server_revision, Some(0));
    assert_eq!(notes.last_seen_updated_at, Some(30));

    session.insert("userId".to_string(), pyre::sync::SessionValue::Integer(2));
    let result = server
        .catchup(&conn, &SyncCursor::new(), &session, 10, "main")
        .await?;
    let notes = result.tables.get("notes").expect("notes should sync");
    let ids = notes
        .rows
        .iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec![json!(2)]);
    assert_eq!(notes.last_seen_updated_at, Some(20));

    Ok(())
}

#[tokio::test]
async fn catchup_binds_string_session_permissions() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    tenant String
}

record Note {
    id Int @id
    tenant String
    body String
    updatedAt Int
    @allow(query) { tenant == Session.tenant }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        r#"
insert into notes (id, tenant, body, updatedAt) values (1, 'safe', 'one', 10);
insert into notes (id, tenant, body, updatedAt) values (2, 'x'' OR 1=1 --', 'two', 20);
"#,
    )
    .await?;

    let mut session = HashMap::new();
    session.insert(
        "tenant".to_string(),
        pyre::sync::SessionValue::Text("x' OR 1=1 --".to_string()),
    );

    let result = catchup(&conn, &db.context, &SyncCursor::new(), &session, 10).await?;
    let notes = result.tables.get("notes").expect("notes should sync");

    assert_eq!(notes.rows.len(), 1);
    assert_eq!(notes.rows[0]["id"], json!(2));

    Ok(())
}

#[tokio::test]
async fn catchup_shapes_json_and_custom_type_columns() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type TileFormat
   = Png
   | Webp

type Tiling
   = Tiling {
        tileRootKey String
        tileWidth Int
        format TileFormat
     }

record Map {
    id Int @id
    name String
    attrs Json
    tiling Tiling?
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        r#"
insert into maps (
    id,
    name,
    attrs,
    tiling,
    tiling__tileRootKey,
    tiling__tileWidth,
    tiling__format,
    updatedAt
) values (
    1,
    'World',
    json_object('theme', 'forest', 'danger', 4),
    'Tiling',
    'tiles/root',
    256,
    'Png',
    10
);
"#,
    )
    .await?;

    let result = catchup(&conn, &db.context, &SyncCursor::new(), &HashMap::new(), 10).await?;
    let maps = result.tables.get("maps").expect("maps should sync");

    assert_eq!(maps.rows.len(), 1);
    assert_eq!(
        maps.rows[0]["attrs"],
        json!({ "theme": "forest", "danger": 4 })
    );
    assert_eq!(
        maps.rows[0]["tiling"],
        json!({
            "_type": "Tiling",
            "tileRootKey": "tiles/root",
            "tileWidth": 256,
            "format": { "_type": "Png" }
        })
    );

    Ok(())
}

#[tokio::test]
async fn catchup_rejects_zero_page_size() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let err = match catchup(&conn, &db.context, &SyncCursor::new(), &HashMap::new(), 0).await {
        Ok(_) => panic!("zero page size should fail"),
        Err(err) => err,
    };

    assert_eq!(err.to_string(), "page_size must be greater than zero");

    Ok(())
}

#[tokio::test]
async fn calculate_deltas_sends_public_rows_to_all_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![vec![json!(1), json!("one"), json!(10)]],
    }];
    let connected_sessions = ConnectedSessions::from([
        ("a".to_string(), SyncSession::new()),
        ("b".to_string(), SyncSession::new()),
    ]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].session_id, "a");
    assert_eq!(messages[0].message.type_, "delta");
    assert_eq!(messages[0].message.data[0].table_name, "notes");
    assert_eq!(
        messages[0].message.data[0].rows[0],
        vec![json!(1), json!("one"), json!(10)]
    );
    assert_eq!(messages[1].session_id, "b");

    Ok(())
}

#[tokio::test]
async fn native_sync_revision_persists_in_pyre_sync() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![vec![json!(1), json!("one"), json!(10)]],
    }];
    let connected_sessions = ConnectedSessions::from([
        ("a".to_string(), SyncSession::new()),
        ("b".to_string(), SyncSession::new()),
    ]);
    let server = SyncServer::new(&db.context);

    let mut first_result = query_result(affected_rows.clone());
    let mut second_result = query_result(affected_rows);
    let first = server
        .calculate_deltas(&conn, &mut first_result, &connected_sessions, "main", None)
        .await?;
    let second = server
        .calculate_deltas(
            &conn,
            &mut second_result,
            &connected_sessions,
            "main",
            Some("a"),
        )
        .await?;

    assert_eq!(first[0].message.server_revision, Some(1));
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].session_id, "b");
    assert_eq!(second[0].message.server_revision, Some(2));
    assert_eq!(second_result.response["serverRevision"], json!(2));
    assert_eq!(second_result.response["sync"]["serverRevision"], json!(2));

    let catchup_result = server
        .catchup(&conn, &SyncCursor::new(), &SyncSession::new(), 10, "main")
        .await?;
    assert_eq!(catchup_result.server_revision, Some(2));

    Ok(())
}

#[tokio::test]
async fn calculate_deltas_filters_rows_by_session_permissions(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "ownerId".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![
            vec![json!(1), json!(1), json!("one"), json!(10)],
            vec![json!(2), json!(2), json!("two"), json!(20)],
        ],
    }];
    let connected_sessions = ConnectedSessions::from([
        (
            "user-1".to_string(),
            SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(1))]),
        ),
        (
            "user-2".to_string(),
            SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(2))]),
        ),
    ]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].session_id, "user-1");
    assert_eq!(messages[0].message.database_id.as_deref(), Some("main"));
    assert_eq!(messages[0].message.data[0].rows[0][0], json!(1));
    assert_eq!(messages[1].session_id, "user-2");
    assert_eq!(messages[1].message.data[0].rows[0][0], json!(2));

    Ok(())
}

#[tokio::test]
async fn calculate_deltas_reshapes_custom_type_columns() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type TileFormat
   = Png
   | Webp

type Tiling
   = Tiling {
        tileRootKey String
        tileWidth Int
        format TileFormat
     }

record Map {
    id Int @id
    tiling Tiling?
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "maps".to_string(),
        headers: vec![
            "id".to_string(),
            "tiling".to_string(),
            "tiling__tileRootKey".to_string(),
            "tiling__tileWidth".to_string(),
            "tiling__format".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![vec![
            json!(1),
            json!("Tiling"),
            json!("tiles/root"),
            json!(256),
            json!("Png"),
            json!(10),
        ]],
    }];
    let connected_sessions = ConnectedSessions::from([("a".to_string(), SyncSession::new())]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].message.data[0].headers,
        vec!["id", "tiling", "updatedAt"]
    );
    assert_eq!(
        messages[0].message.data[0].rows[0],
        vec![
            json!(1),
            json!({
                "_type": "Tiling",
                "tileRootKey": "tiles/root",
                "tileWidth": 256,
                "format": { "_type": "Png" }
            }),
            json!(10),
        ]
    );

    Ok(())
}

#[tokio::test]
async fn calculate_deltas_returns_no_messages_without_rows_or_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![vec![json!(1), json!("one"), json!(10)]],
    }];
    let connected_sessions = ConnectedSessions::from([("a".to_string(), SyncSession::new())]);

    let server = SyncServer::new(&db.context);
    let mut empty_result = query_result(Vec::new());
    let mut no_sessions_result = query_result(affected_rows);
    assert!(server
        .calculate_deltas(&conn, &mut empty_result, &connected_sessions, "main", None)
        .await?
        .is_empty());
    assert!(server
        .calculate_deltas(
            &conn,
            &mut no_sessions_result,
            &ConnectedSessions::new(),
            "main",
            None
        )
        .await?
        .is_empty());
    assert_eq!(no_sessions_result.response["serverRevision"], json!(1));

    Ok(())
}

#[tokio::test]
async fn generated_insert_affected_rows_feed_native_deltas(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let insert_query = r#"
insert CreateNote($body: String) {
    note {
        body = $body
        updatedAt = 10
        id
    }
}
"#;
    let result_sets = db
        .execute_insert_with_params(
            insert_query,
            HashMap::from([("body".to_string(), libsql::Value::Text("one".to_string()))]),
        )
        .await?;
    let affected_rows = extract_affected_rows(result_sets).await?;
    let connected_sessions = ConnectedSessions::from([
        ("a".to_string(), SyncSession::new()),
        ("b".to_string(), SyncSession::new()),
    ]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;
    assert_eq!(affected_rows.len(), 1);
    assert_eq!(affected_rows[0].table_name, "notes");
    assert!(affected_rows[0].headers.contains(&"id".to_string()));
    assert!(affected_rows[0].headers.contains(&"body".to_string()));
    assert!(affected_rows[0].headers.contains(&"updatedAt".to_string()));
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message.type_, "delta");
    assert_eq!(messages[0].message.data[0].table_name, "notes");
    assert_eq!(messages[0].message.data[0].rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn native_deltas_send_sync_required_when_row_count_exceeds_cap(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: (0..=MAX_LIVE_SYNC_DELTA_ROWS)
            .map(|index| vec![json!(index), json!("one"), json!(10)])
            .collect(),
    }];
    let connected_sessions = ConnectedSessions::from([("a".to_string(), SyncSession::new())]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message.type_, "syncRequired");
    assert!(messages[0].message.data.is_empty());

    Ok(())
}

#[tokio::test]
async fn native_deltas_send_sync_required_when_payload_exceeds_cap(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![vec![
            json!(1),
            json!("x".repeat(MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES)),
            json!(10),
        ]],
    }];
    let server = SyncServer::new(&db.context);
    let connected_sessions = ConnectedSessions::from([("a".to_string(), SyncSession::new())]);

    let mut result = query_result(affected_rows.clone());
    let messages = server
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message.type_, "syncRequired");
    assert_eq!(messages[0].message.database_id.as_deref(), Some("main"));
    assert!(messages[0].message.data.is_empty());

    Ok(())
}

#[tokio::test]
async fn native_deltas_send_sync_required_when_recipient_count_exceeds_cap(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let affected_rows = vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec![
            "id".to_string(),
            "body".to_string(),
            "updatedAt".to_string(),
        ],
        rows: vec![vec![json!(1), json!("one"), json!(10)]],
    }];
    let connected_sessions = (0..=MAX_LIVE_SYNC_FANOUT_RECIPIENTS)
        .map(|index| (format!("s{}", index), SyncSession::new()))
        .collect::<ConnectedSessions>();

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(messages.len(), MAX_LIVE_SYNC_FANOUT_RECIPIENTS + 1);
    assert!(messages
        .iter()
        .all(|message| message.message.type_ == "syncRequired"));
    assert!(messages
        .iter()
        .all(|message| message.message.data.is_empty()));

    Ok(())
}

#[tokio::test]
async fn generated_update_affected_rows_feed_permission_filtered_native_deltas(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
    @allow(insert, update) { ownerId == 1 }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into notes (id, ownerId, body, updatedAt) values (1, 1, 'old', 10);",
    )
    .await?;
    let update_query = r#"
update UpdateNote {
    note {
        @where { id == 1 }
        body = "new"
        updatedAt = 20
        id
        ownerId
    }
}
"#;
    let result_sets = db.execute_query(update_query).await?;
    let affected_rows = extract_affected_rows(result_sets).await?;
    let connected_sessions = ConnectedSessions::from([
        (
            "user-1".to_string(),
            SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(1))]),
        ),
        (
            "user-2".to_string(),
            SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(2))]),
        ),
    ]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(affected_rows.len(), 1);
    assert_eq!(affected_rows[0].table_name, "notes");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].session_id, "user-1");
    assert_eq!(messages[0].message.data[0].rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn generated_delete_affected_rows_feed_permission_filtered_native_deltas(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
    @allow(delete) { ownerId == 1 }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into notes (id, ownerId, body, updatedAt) values (1, 1, 'doomed', 10);",
    )
    .await?;
    let delete_query = r#"
delete RemoveNote {
    note {
        @where { id == 1 }
        id
        ownerId
        body
        updatedAt
    }
}
"#;
    let result_sets = db.execute_query(delete_query).await?;
    let affected_rows = extract_affected_rows(result_sets).await?;
    let connected_sessions = ConnectedSessions::from([
        (
            "user-1".to_string(),
            SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(1))]),
        ),
        (
            "user-2".to_string(),
            SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(2))]),
        ),
    ]);

    let mut result = query_result(affected_rows.clone());
    let messages = SyncServer::new(&db.context)
        .calculate_deltas(&conn, &mut result, &connected_sessions, "main", None)
        .await?;

    assert_eq!(affected_rows.len(), 1);
    assert_eq!(affected_rows[0].table_name, "notes");
    assert_eq!(affected_rows[0].rows.len(), 1);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].session_id, "user-1");
    assert_eq!(messages[0].message.data[0].rows[0][0], json!(1));

    Ok(())
}

#[tokio::test]
async fn permission_hash_change_forces_full_resync() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        r#"
insert into notes (id, ownerId, body, updatedAt) values (1, 1, 'one', 10);
insert into notes (id, ownerId, body, updatedAt) values (2, 2, 'two', 20);
"#,
    )
    .await?;
    let session_one =
        SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(1))]);
    let first = catchup(&conn, &db.context, &SyncCursor::new(), &session_one, 10).await?;
    let first_notes = first.tables.get("notes").expect("notes should sync");

    let cursor = SyncCursor::from([(
        "notes".to_string(),
        TableCursor {
            last_seen_updated_at: first_notes.last_seen_updated_at,
            permission_hash: first_notes.permission_hash.clone(),
        },
    )]);
    let session_two =
        SyncSession::from([("userId".to_string(), pyre::sync::SessionValue::Integer(2))]);
    let second = catchup(&conn, &db.context, &cursor, &session_two, 10).await?;
    let second_notes = second
        .tables
        .get("notes")
        .expect("permission change should resync");

    assert_eq!(
        first_notes
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1)]
    );
    assert_eq!(
        second_notes
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2)]
    );
    assert_ne!(first_notes.permission_hash, second_notes.permission_hash);

    Ok(())
}

#[tokio::test]
async fn linked_tables_have_parent_before_child_sync_layers(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record User {
    id Int @id
    name String
    updatedAt Int
    @public
}

record Post {
    id Int @id
    authorId Int
    title String
    author @link(authorId, User.id)
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let user = db
        .context
        .tables
        .values()
        .find(|table| table.record.name == "User")
        .expect("User table should exist");
    let post = db
        .context
        .tables
        .values()
        .find(|table| table.record.name == "Post")
        .expect("Post table should exist");

    assert!(user.sync_layer < post.sync_layer);

    Ok(())
}

#[tokio::test]
async fn load_schema_from_database_returns_typechecked_context(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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

    let loaded = load_schema_from_database(&conn).await?;
    let context = loaded.context()?;

    assert!(context
        .tables
        .values()
        .any(|table| table.record.name == "Note"));
    assert_eq!(loaded.schema()?.files.len(), 1);

    Ok(())
}

#[tokio::test]
async fn loaded_context_can_drive_catchup() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    let loaded = load_context_from_database(&conn).await?;
    let server = SyncServer::new(loaded.context()?);

    let result = server
        .catchup(&conn, &SyncCursor::new(), &HashMap::new(), 10, "main")
        .await?;
    let notes = result.tables.get("notes").expect("notes should sync");

    assert_eq!(notes.rows.len(), 1);
    assert_eq!(notes.rows[0]["body"], json!("one"));

    Ok(())
}

#[tokio::test]
async fn load_schema_from_uninitialized_database_fails_clearly(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::TempDir::new()?;
    let db_path = temp_dir.path().join("empty.db");
    let db = libsql::Builder::new_local(db_path.to_str().ok_or("invalid db path")?)
        .build()
        .await?;
    let conn = db.connect()?;
    conn.execute_batch(
        "create table notes (id integer primary key, body text, updatedAt integer);",
    )
    .await?;

    let err = match load_schema_from_database(&conn).await {
        Ok(_) => panic!("uninitialized database should not load a Pyre schema"),
        Err(err) => err,
    };

    assert_eq!(err.to_string(), "database does not contain a Pyre schema");

    Ok(())
}

#[tokio::test]
async fn load_schema_reports_parse_errors() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    conn.execute(
        "update _pyre_schema set schema = ?",
        libsql::params_from_iter(vec![libsql::Value::Text("record {".to_string())]),
    )
    .await?;

    let err = match load_schema_from_database(&conn).await {
        Ok(_) => panic!("invalid schema should not load"),
        Err(err) => err,
    };

    assert!(matches!(err, SchemaError::SchemaParse { .. }));

    Ok(())
}

#[tokio::test]
async fn load_schema_reports_typecheck_errors() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
    conn.execute(
        "update _pyre_schema set schema = ?",
        libsql::params_from_iter(vec![libsql::Value::Text(
            r#"
record Note {
    id Int @id
    missing MissingType
    @public
}
"#
            .to_string(),
        )]),
    )
    .await?;

    let err = match load_schema_from_database(&conn).await {
        Ok(_) => panic!("typecheck-invalid schema should not load"),
        Err(err) => err,
    };

    assert!(matches!(err, SchemaError::SchemaTypecheck { .. }));

    Ok(())
}

#[tokio::test]
async fn sync_payloads_serialize_to_client_contract() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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

    let catchup_result =
        catchup(&conn, &db.context, &SyncCursor::new(), &HashMap::new(), 10).await?;
    let catchup_json = serde_json::to_value(&catchup_result)?;

    assert_eq!(catchup_json["has_more"], json!(false));
    assert_eq!(
        catchup_json["tables"]["notes"]["rows"][0]["body"],
        json!("one")
    );
    assert!(catchup_json["tables"]["notes"]["permission_hash"].is_string());
    assert_eq!(
        catchup_json["tables"]["notes"]["last_seen_updated_at"],
        json!(10)
    );

    let message = DeltaMessage::delta(vec![AffectedRowTableGroup {
        table_name: "notes".to_string(),
        headers: vec!["id".to_string(), "body".to_string()],
        rows: vec![vec![json!(1), json!("one")]],
    }]);
    let message_json = serde_json::to_value(&message)?;

    assert_eq!(
        message_json,
        json!({
            "type": "delta",
            "data": [
                {
                    "table_name": "notes",
                    "headers": ["id", "body"],
                    "rows": [[1, "one"]]
                }
            ]
        })
    );

    Ok(())
}

#[tokio::test]
async fn session_value_permissions_handle_strings_booleans_and_nulls(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    role String
    enabled Int
    region String?
}

record Feature {
    id Int @id
    role String
    enabled Bool
    region String?
    updatedAt Int
    @allow(query) { role == Session.role && enabled == Session.enabled && region == Session.region }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        r#"
insert into features (id, role, enabled, region, updatedAt) values (1, 'admin', 1, null, 10);
insert into features (id, role, enabled, region, updatedAt) values (2, 'admin', 0, null, 20);
insert into features (id, role, enabled, region, updatedAt) values (3, 'user', 1, null, 30);
"#,
    )
    .await?;
    let session = SyncSession::from([
        (
            "role".to_string(),
            pyre::sync::SessionValue::Text("admin".to_string()),
        ),
        ("enabled".to_string(), pyre::sync::SessionValue::Integer(1)),
        ("region".to_string(), pyre::sync::SessionValue::Null),
    ]);

    let result = catchup(&conn, &db.context, &SyncCursor::new(), &session, 10).await?;
    let features = result.tables.get("features").expect("features should sync");

    assert_eq!(
        features
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1)]
    );

    Ok(())
}

#[tokio::test]
async fn pyre_session_validates_record_and_builds_logical_and_sql_views(
) -> Result<(), Box<dyn std::error::Error>> {
    let schema = HashMap::from([
        (
            "userId".to_string(),
            FieldSchema {
                type_: "Int".to_string(),
                is_enum: false,
                enum_variants: Vec::new(),
                nullable: false,
                omittable: false,
            },
        ),
        (
            "role".to_string(),
            FieldSchema {
                type_: "String".to_string(),
                is_enum: false,
                enum_variants: Vec::new(),
                nullable: false,
                omittable: false,
            },
        ),
        (
            "enabled".to_string(),
            FieldSchema {
                type_: "Bool".to_string(),
                is_enum: false,
                enum_variants: Vec::new(),
                nullable: false,
                omittable: false,
            },
        ),
    ]);
    let session = PyreSession::new(
        json!({
            "userId": 1,
            "role": "admin",
            "enabled": true
        }),
        &schema,
    )?;

    assert_eq!(session.sql_args()["session_userId"], json!(1));
    assert_eq!(session.sql_args()["session_role"], json!("admin"));
    assert_eq!(session.sql_args()["session_enabled"], json!(1));
    assert!(matches!(
        session.logical().get("userId"),
        Some(pyre::sync::SessionValue::Integer(1))
    ));
    assert!(matches!(
        session.logical().get("role"),
        Some(pyre::sync::SessionValue::Text(value)) if value == "admin"
    ));
    assert!(matches!(
        session.logical().get("enabled"),
        Some(pyre::sync::SessionValue::Integer(1))
    ));

    Ok(())
}

#[tokio::test]
async fn pyre_session_rejects_missing_or_invalid_fields() -> Result<(), Box<dyn std::error::Error>>
{
    let schema = HashMap::from([(
        "userId".to_string(),
        FieldSchema {
            type_: "Int".to_string(),
            is_enum: false,
            enum_variants: Vec::new(),
            nullable: false,
            omittable: false,
        },
    )]);

    let missing = PyreSession::new(json!({}), &schema).expect_err("missing field should fail");
    assert_eq!(missing.to_string(), "missing session field 'userId'");

    let invalid = PyreSession::new(json!({ "userId": "nope" }), &schema)
        .expect_err("invalid field should fail");
    assert_eq!(invalid.to_string(), "session field 'userId' must be Int");

    Ok(())
}
