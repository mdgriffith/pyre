#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::{
    manifest::{Manifest, PyreSession},
    query, schema, seed,
};
use pyre::{ast, parser, typecheck};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

const SCHEMA: &str = r#"
record Event {
    @public
    key Id.Uuid @id
    sequence Sequence.Int
    payload String
}
"#;
const ONE: &str = "01900000-0000-7000-8000-000000000001";
const TWO: &str = "01900000-0000-7000-8000-000000000002";
const THREE: &str = "01900000-0000-7000-8000-000000000003";

fn database(source: &str) -> ast::Database {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema parses");
    ast::Database {
        schemas: vec![schema],
    }
}

fn generated(
    context: &typecheck::Context,
) -> (
    Manifest,
    HashMap<String, String>,
    Vec<pyre::filesystem::GeneratedFile<String>>,
) {
    generated_with_query(
        context,
        r#"
query After($after: Int) {
    event {
        @where { sequence > $after }
        @sort(sequence, Asc)
        key
        sequence
    }
}
"#,
    )
}

fn generated_with_query(
    context: &typecheck::Context,
    source: &str,
) -> (
    Manifest,
    HashMap<String, String>,
    Vec<pyre::filesystem::GeneratedFile<String>>,
) {
    let mut queries = parser::parse_query("query.pyre", source).unwrap();
    pyre::generated_queries::append_generated_crud_queries(&mut queries, context);
    let info = typecheck::check_queries(&queries, context).expect("queries typecheck");
    let ids = queries
        .queries
        .iter()
        .filter_map(|query| match query {
            ast::QueryDef::Query(query) => Some((query.name.clone(), query.interface_hash.clone())),
            _ => None,
        })
        .collect();
    let mut files = Vec::new();
    pyre::generate::write_queries(context, &queries, &info, &mut files);
    let manifest = files
        .iter()
        .find(|file| file.path == Path::new("manifest.json"))
        .unwrap();
    (
        serde_json::from_str(&manifest.contents).unwrap(),
        ids,
        files,
    )
}

#[test]
fn sequence_types_preserve_logical_identity_and_exclude_write_inputs() {
    let database = database(SCHEMA);
    let context = typecheck::check_schema(&database).unwrap();
    let (manifest, ids, mut files) = generated(&context);
    for name in ["EventCreate", "EventUpdate"] {
        assert!(!manifest.queries[&ids[name]]
            .input_schema
            .contains_key("sequence"));
    }
    assert_eq!(
        manifest.queries[&ids["EventCreate"]]
            .generated_edit
            .as_ref()
            .unwrap()
            .create_id
            .as_deref(),
        Some("key")
    );
    assert_eq!(manifest.queries[&ids["EventDelete"]].input_schema.len(), 1);
    assert!(manifest.queries[&ids["EventDelete"]]
        .input_schema
        .contains_key("key"));
    pyre::generate::generate_schema(&context, &database, &mut files);
    let rust = &files
        .iter()
        .find(|file| file.path.ends_with("rust/server.rs"))
        .unwrap()
        .contents;
    assert!(rust.contains("pub sequence: i64"));
    let ts = &files
        .iter()
        .find(|file| file.path.ends_with("metadata/eventCreate.ts"))
        .unwrap()
        .contents;
    assert!(ts.contains("sequence: z.number()"));
    assert!(
        !ts.contains("  optimistic:"),
        "the server assigns sequence values, never optimistic clients"
    );
    let elm = &files
        .iter()
        .find(|file| file.path.ends_with("Query/EventCreate.elm"))
        .unwrap()
        .contents;
    assert!(elm.contains("sequence : Int"));
    let seed = &files
        .iter()
        .find(|file| file.path.ends_with("rust/seed.rs"))
        .unwrap()
        .contents;
    assert!(!seed.contains("pub sequence:"));
    let ts_seed = &files
        .iter()
        .find(|file| file.path.ends_with("typescript/seed.ts"))
        .unwrap()
        .contents;
    assert!(!ts_seed.contains("\"sequence\"?:"));
    let metadata = &files
        .iter()
        .find(|file| file.path.ends_with("typescript/core/schema.ts"))
        .unwrap()
        .contents;
    assert!(metadata
        .contains("field: \"sequence\",\n          unique: true,\n          primary: false"));
    assert!(metadata.contains("field: \"key\",\n          unique: true,\n          primary: true"));
    let stored = pyre::db::migrate::schema_to_storage_string(&context, &database.schemas[0]);
    assert!(stored.contains("Sequence.Int"));
    assert!(typecheck::check_schema(&self::database(&stored)).is_ok());
}

#[test]
fn invalid_sequence_declarations_have_source_diagnostics() {
    for source in [
        SCHEMA.replace("sequence Sequence.Int", "sequence Sequence.Int?"),
        SCHEMA.replace("sequence Sequence.Int", "sequence Json<List<Sequence.Int>>"),
        SCHEMA.replace("sequence Sequence.Int", "sequence Sequence.Int @default(1)"),
        SCHEMA.replace("sequence Sequence.Int", "sequence Sequence.Int @createdAt"),
        SCHEMA.replace("sequence Sequence.Int", "sequence Sequence.Int @updatedAt"),
        SCHEMA.replace("sequence Sequence.Int", "sequence Sequence.Int @id"),
        SCHEMA.replace(
            "sequence Sequence.Int",
            "sequence Sequence.Int\n other Sequence.Int",
        ),
        format!("@syncable(false)\n{}", SCHEMA.replace("Id.Uuid", "Id.Int")),
        format!("session {{\n sequence Sequence.Int\n}}\n{SCHEMA}"),
        format!("type Payload = Value {{ sequence Sequence.Int }}\n{SCHEMA}"),
        SCHEMA.replace(
            "@public",
            "@allow(insert) { sequence > 0 }\n @allow(query, update, delete) { True }",
        ),
    ] {
        let errors = typecheck::check_schema(&database(&source)).expect_err(&source);
        assert!(
            errors.iter().any(|error| !error.locations.is_empty()),
            "{errors:?}"
        );
    }
    let context = typecheck::check_schema(&database(SCHEMA)).unwrap();
    for source in [
        "insert Bad($key: Event.key) { event { key = $key payload = \"x\" sequence = 1 } }",
        "update Bad { event { sequence = 1 } }",
        "query Bad($seq: Sequence.Int) { event { @where { sequence == $seq } key } }",
    ] {
        let queries = parser::parse_query("query.pyre", source).unwrap();
        assert!(
            typecheck::check_queries(&queries, &context).is_err(),
            "{source}"
        );
    }
    let queries = parser::parse_query(
        "query.pyre",
        "query Valid($after: Event.sequence) { event { @where { sequence > $after } key } }",
    )
    .unwrap();
    assert!(typecheck::check_queries(&queries, &context).is_ok());
    assert!(typecheck::check_schema(&database(&SCHEMA.replace(
        "@public",
        "@allow(query, update, delete) { sequence > 0 }\n @allow(insert) { True }"
    )))
    .is_ok());
}

#[tokio::test]
async fn sequences_are_atomic_immutable_and_not_reused_after_deletion(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(SCHEMA).await?;
    let conn = db.db.connect()?;
    let (manifest, ids, _) = generated(&db.context);
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = &ids["EventCreate"];
    let first = query::run_sync(
        &conn,
        &manifest,
        create,
        json!({"key": ONE, "payload": "first"}),
        &session,
    )
    .await?;
    assert_eq!(first.response["event"][0]["sequence"], 1);
    assert_eq!(first.response["event"][0]["key"], ONE);
    let batch = query::run_sync(
        &conn,
        &manifest,
        "$batch",
        json!([
            {"queryId":create, "input":{"key":TWO,"payload":"second"}},
            {"queryId":create, "input":{"key":THREE,"payload":"third"}}
        ]),
        &session,
    )
    .await?;
    assert_eq!(batch.response[0]["result"]["event"][0]["sequence"], 2);
    assert_eq!(batch.response[1]["result"]["event"][0]["sequence"], 3);
    query::run_sync(
        &conn,
        &manifest,
        &ids["EventDelete"],
        json!({"key":THREE}),
        &session,
    )
    .await?;
    let next = query::run_sync(
        &conn,
        &manifest,
        create,
        json!({"key":THREE,"payload":"fourth"}),
        &session,
    )
    .await?;
    assert_eq!(next.response["event"][0]["sequence"], 4);
    let update = query::run_sync(
        &conn,
        &manifest,
        &ids["EventUpdate"],
        json!({"key":ONE,"payload":"edited"}),
        &session,
    )
    .await?;
    assert_eq!(update.response["event"][0]["sequence"], 1);
    assert!(query::run_sync(
        &conn,
        &manifest,
        create,
        json!({"key":ONE,"payload":"duplicate"}),
        &session
    )
    .await
    .is_err());
    assert!(query::run_sync(
        &conn,
        &manifest,
        &ids["EventUpdate"],
        json!({"key":ONE,"sequence":42}),
        &session
    )
    .await
    .is_err());
    assert!(query::run_sync(
        &conn,
        &manifest,
        "$batch",
        json!([
            {"queryId": &ids["EventDelete"], "input":{"key":THREE}},
            {"queryId":create, "input":{"key":THREE,"payload":"rolled back"}},
            {"queryId":create, "input":{"key":ONE,"payload":"conflict"}}
        ]),
        &session
    )
    .await
    .is_err());
    let after = query::run(
        &conn,
        &manifest,
        &ids["After"],
        json!({"after":1}),
        &session,
    )
    .await?;
    assert_eq!(
        after.response["event"],
        json!([{"key":TWO,"sequence":2},{"key":THREE,"sequence":4}])
    );
    let mut pk = conn
        .query(
            "select name from pragma_table_info('events') where pk = 1",
            (),
        )
        .await?;
    assert_eq!(pk.next().await?.unwrap().get::<String>(0)?, "sequence");
    // UUID remains a valid foreign-key target despite no longer being SQLite's PK.
    conn.execute_batch(
        "pragma foreign_keys=on; create table refs (eventKey TEXT REFERENCES events(key));",
    )
    .await?;
    conn.execute("insert into refs values (?)", [ONE]).await?;
    assert!(conn
        .execute("insert into refs values ('missing')", ())
        .await
        .is_err());
    // A different game database starts its own independent sequence.
    let other = TestDatabase::new(SCHEMA).await?;
    let result = query::run(
        &other.db.connect()?,
        &manifest,
        create,
        json!({"key":ONE,"payload":"other game"}),
        &session,
    )
    .await?;
    assert_eq!(result.response["event"][0]["sequence"], 1);
    Ok(())
}

#[tokio::test]
async fn sequence_schema_roundtrips_and_seed_allocates_sequences(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    assert_eq!(
        schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, SCHEMA).await?,
        schema::EnsureDatabaseOutcome::Created
    );
    assert_eq!(
        schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, SCHEMA).await?,
        schema::EnsureDatabaseOutcome::UpToDate
    );
    let loaded = schema::load_schema_from_database(&conn).await?;
    let recovered = pyre::db::introspect::to_schema::to_schema(loaded.introspection());
    let recovered = ast::Database {
        schemas: vec![ast::Schema {
            files: vec![recovered],
            ..ast::Schema::default()
        }],
    };
    let recovered_context = typecheck::check_schema(&recovered).unwrap();
    let columns = ast::collect_columns(&recovered_context.tables["event"].record.fields);
    assert!(columns
        .iter()
        .any(|column| column.name == "key" && ast::is_primary_key(column)));
    assert!(columns.iter().any(|column| column.name == "sequence"
        && ast::is_sequence(column)
        && !ast::is_primary_key(column)));
    assert_eq!(
        ast::get_primary_id_field_name(&loaded.context()?.tables["event"].record.fields).as_deref(),
        Some("key")
    );
    seed::seed(&conn, json!({"events":[{"key":ONE,"payload":"seed"}]})).await?;
    assert!(seed::seed(
        &conn,
        json!({"events":[{"key":TWO,"payload":"seed","sequence":10}]})
    )
    .await
    .is_err());
    let mut rows = conn
        .query("select sequence from events where key = ?", [ONE])
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
    let extended = SCHEMA.replace("payload String", "payload String\n note String?");
    assert_eq!(
        schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, &extended).await?,
        schema::EnsureDatabaseOutcome::Migrated
    );
    assert_eq!(
        schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, &extended).await?,
        schema::EnsureDatabaseOutcome::UpToDate
    );
    Ok(())
}

#[tokio::test]
async fn adding_sequence_to_existing_history_requires_explicit_migration(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    let old = SCHEMA.replace("    sequence Sequence.Int\n", "");
    schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, &old).await?;
    let error = schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, SCHEMA)
        .await
        .unwrap_err();
    let schema::EnsureDatabaseError::Migration(errors) = error else {
        panic!("expected migration diagnostic")
    };
    assert!(errors
        .iter()
        .any(|error| pyre::error::format_error(SCHEMA, error, false).contains("table-rebuild")));
    assert_eq!(
        schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, &old).await?,
        schema::EnsureDatabaseOutcome::UpToDate
    );
    Ok(())
}

#[tokio::test]
async fn sequence_primary_key_drift_is_not_reported_as_up_to_date(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, SCHEMA).await?;
    conn.execute_batch("drop table events; create table events (key TEXT PRIMARY KEY NOT NULL, sequence INTEGER NOT NULL, payload TEXT NOT NULL);").await?;
    let error = schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, SCHEMA)
        .await
        .unwrap_err();
    let schema::EnsureDatabaseError::Migration(errors) = error else {
        panic!("expected migration diagnostic")
    };
    assert!(errors
        .iter()
        .any(|error| pyre::error::format_error(SCHEMA, error, false)
            .contains("physical primary key")));
    Ok(())
}

#[tokio::test]
async fn nested_inserts_and_sync_preserve_uuid_identity_and_assigned_sequences(
) -> Result<(), Box<dyn std::error::Error>> {
    use pyre::server::sync::{ConnectedSessions, SyncServer, SyncSession};
    let source = format!("{}\nrecord Reply {{\n @public\n key Id.Uuid @id\n sequence Sequence.Int\n eventKey Event.key\n text String\n}}",
        SCHEMA.replace("payload String", "payload String\n replies @link(key, Reply.eventKey)"));
    let db = TestDatabase::new(&source).await?;
    let conn = db.db.connect()?;
    let (manifest, ids, _) = generated_with_query(
        &db.context,
        r#"
insert Append($event: Event.key, $reply: Reply.key) {
    event {
        key = $event
        payload = "parent"
        sequence
        replies {
            key = $reply
            text = "child"
            sequence
        }
    }
}
"#,
    );
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let mut result = query::run_sync(
        &conn,
        &manifest,
        &ids["Append"],
        json!({"event":ONE,"reply":TWO}),
        &session,
    )
    .await?;
    assert_eq!(result.response["event"][0]["sequence"], 1);
    let mut reply = conn
        .query("select key, sequence, eventKey from replies", ())
        .await?;
    let reply = reply.next().await?.unwrap();
    assert_eq!(reply.get::<String>(0)?, TWO);
    assert_eq!(reply.get::<i64>(1)?, 1);
    assert_eq!(reply.get::<String>(2)?, ONE);
    let sessions: ConnectedSessions = HashMap::from([("origin".into(), SyncSession::new())]);
    let sync = SyncServer::new(&db.context);
    sync.calculate_deltas(&conn, &mut result, &sessions, "game", Some("origin"))
        .await?;
    let data = result.response["sync"]["data"].as_array().unwrap();
    for (table, key) in [("events", ONE), ("replies", TWO)] {
        let group = data
            .iter()
            .find(|group| group["table_name"] == table)
            .unwrap();
        let headers = group["headers"].as_array().unwrap();
        let key_index = headers.iter().position(|header| header == "key").unwrap();
        let sequence_index = headers
            .iter()
            .position(|header| header == "sequence")
            .unwrap();
        assert_eq!(group["rows"][0][key_index], key);
        assert_eq!(group["rows"][0][sequence_index], 1);
    }
    let mut removed = query::run_sync(
        &conn,
        &manifest,
        &ids["ReplyDelete"],
        json!({"key":TWO}),
        &session,
    )
    .await?;
    sync.calculate_deltas(&conn, &mut removed, &sessions, "game", Some("origin"))
        .await?;
    assert_eq!(
        removed.response["sync"]["data"][0]["headers"],
        json!(["key", "_pyre_removed"])
    );
    assert_eq!(
        removed.response["sync"]["data"][0]["rows"],
        json!([[TWO, true]])
    );
    Ok(())
}
