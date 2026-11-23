#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::seed;
use serde_json::json;

#[tokio::test]
async fn seed_matches_wasm_conformance_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(include_str!("fixtures/seed/schema.pyre")).await?;
    let conn = db.db.connect()?;
    let input: serde_json::Value = serde_json::from_str(include_str!("fixtures/seed/input.json"))?;
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/seed/expected.json"))?;
    assert_eq!(seed::seed(&conn, input).await?.response, expected);
    let mut rows = conn
        .query("SELECT id, authorId FROM posts ORDER BY id", ())
        .await?;
    for (id, author_id) in [(4i64, 1i64), (5, 2)] {
        let row = rows.next().await?.expect("seeded post");
        assert_eq!((row.get::<i64>(0)?, row.get::<i64>(1)?), (id, author_id));
    }
    assert!(rows.next().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn seed_rejects_invalid_typed_json_and_rolls_back() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(include_str!("fixtures/seed/typed-json.pyre")).await?;
    let conn = db.db.connect()?;
    let valid = json!({
        "payload": {"_type": "Payload", "count": 1},
        "groups": {"group": [null, {"_type": "Payload", "count": 2}]}
    });
    for (invalid, path) in [
        (
            json!({"payload": {"_type": "Payload", "count": "wrong"}, "groups": {}}),
            "entries[1].payload.count",
        ),
        (
            json!({"payload": valid["payload"], "groups": {"group": [{"_type": "Payload", "count": false}]}}),
            "entries[1].groups[\"group\"][0].count",
        ),
    ] {
        let error = seed::seed(&conn, json!({"entries": [valid, invalid]}))
            .await
            .expect_err("invalid nested value should fail");
        assert!(matches!(error, seed::Error::InvalidInput(message) if message.contains(path)));
        let mut rows = conn.query("SELECT id FROM entries", ()).await?;
        assert!(rows.next().await?.is_none());
    }
    let response = seed::seed(&conn, json!({"entries": [valid]}))
        .await?
        .response;
    assert_eq!(response["entries"][0]["groups"], valid["groups"]);
    Ok(())
}

#[tokio::test]
async fn seed_uses_main_tables_and_metadata_despite_temp_shadows(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(include_str!("fixtures/seed/schema.pyre")).await?;
    let conn = db.db.connect()?;
    conn.execute_batch("CREATE TEMP TABLE users (id INTEGER PRIMARY KEY, decoy TEXT); CREATE TEMP TABLE _pyre_migrations (decoy TEXT);").await?;
    seed::seed_for_namespace(
        &conn,
        Some("_default"),
        json!({"users": [{"name": "Main"}]}),
    )
    .await?;
    let mut main = conn.query("SELECT name FROM main.users", ()).await?;
    assert_eq!(main.next().await?.unwrap().get::<String>(0)?, "Main");
    let mut temp = conn.query("SELECT id FROM temp.users", ()).await?;
    assert!(temp.next().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn seed_inserts_nested_rows_and_serializes_values() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Status
   = Draft
   | Published { note String }

record User {
    id Id.Int @id
    name String
    posts @link(Post.authorId)
    @public
}

record Post {
    id Id.Int @id
    authorId User.id
    title String
    metadata Json
    status Status
    publishedAt DateTime?
    author @link(authorId, User.id)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let result = seed::seed(
        &conn,
        json!({
            "users": [{
                "name": "Imported user",
                "posts": [{
                    "title": "Page one",
                    "metadata": { "page": 1, "labels": ["pdf"] },
                    "status": { "_type": "Published", "note": "reviewed" },
                    "publishedAt": "2026-08-14T12:00:00Z"
                }]
            }]
        }),
    )
    .await?;

    assert_eq!(result.response["users"][0]["id"], json!(1));
    assert_eq!(
        result.response["users"][0]["posts"][0]["authorId"],
        json!(1)
    );
    assert_eq!(
        result.response["users"][0]["posts"][0]["metadata"],
        json!({ "page": 1, "labels": ["pdf"] })
    );
    assert_eq!(
        result.response["users"][0]["posts"][0]["status"],
        json!({ "_type": "Published", "note": "reviewed" })
    );
    assert_eq!(
        result.response["users"][0]["posts"][0]["publishedAt"],
        json!(1786708800)
    );

    let invalid_datetime = seed::seed(
        &conn,
        json!({
            "posts": [{
                "authorId": 1,
                "title": "Invalid date",
                "metadata": {},
                "status": "Draft",
                "publishedAt": "not-a-date"
            }]
        }),
    )
    .await
    .expect_err("invalid DateTime should fail");
    assert!(invalid_datetime
        .to_string()
        .contains("posts[0].publishedAt"));

    let count: i64 = conn
        .query("SELECT count(*) FROM posts", ())
        .await?
        .next()
        .await?
        .expect("count row")
        .get(0)?;
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn seed_rolls_back_nested_failures() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record User {
    id Id.Int @id
    name String
    posts @link(Post.authorId)
    @public
}

record Post {
    id Id.Int @id
    authorId User.id
    title String
    author @link(authorId, User.id)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let error = seed::seed(
        &conn,
        json!({
            "users": [{
                "name": "Must roll back",
                "posts": [{ "title": null }]
            }]
        }),
    )
    .await
    .expect_err("invalid child should fail the seed call");
    assert!(error.to_string().contains("title cannot be null"));

    let count: i64 = conn
        .query("SELECT count(*) FROM users", ())
        .await?
        .next()
        .await?
        .expect("count row")
        .get(0)?;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn seed_inserts_to_one_links_before_the_dependent_row(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record User {
    id Id.Int @id
    name String
    @public
}

record Post {
    id Id.Int @id
    authorId User.id
    title String
    author @link(authorId, User.id)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let result = seed::seed(
        &conn,
        json!({
            "posts": [{
                "title": "Imported post",
                "author": { "name": "Nested author" }
            }]
        }),
    )
    .await?;

    assert_eq!(result.response["posts"][0]["authorId"], json!(1));
    assert_eq!(
        result.response["posts"][0]["author"]["name"],
        json!("Nested author")
    );
    Ok(())
}

#[tokio::test]
async fn seed_requires_custom_payload_fields_and_normalizes_nested_values(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Details
   = Details {
        active Bool
        metadata Json
     }

record Item {
    id Id.Int @id
    details Details
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let error = seed::seed(
        &conn,
        json!({ "items": [{ "details": { "_type": "Details", "active": true } }] }),
    )
    .await
    .expect_err("missing custom payload field should fail");
    assert!(error
        .to_string()
        .contains("items[0].details.metadata is required"));

    let result = seed::seed(
        &conn,
        json!({
            "items": [{
                "details": {
                    "_type": "Details",
                    "active": true,
                    "metadata": { "source": "pdf", "page": 4 }
                }
            }]
        }),
    )
    .await?;
    assert_eq!(
        result.response["items"][0]["details"],
        json!({
            "_type": "Details",
            "active": true,
            "metadata": { "source": "pdf", "page": 4 }
        })
    );
    Ok(())
}

#[tokio::test]
async fn seed_supports_uuid_links_defaults_explicit_null_and_flat_foreign_keys(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Owner {
    id Id.Uuid @id
    name String @default("Unknown")
    @public
}

record Asset {
    id Id.Int @id
    ownerId Owner.id
    note String?
    owner @link(ownerId, Owner.id)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let result = seed::seed(
        &conn,
        json!({
            "assets": [{
                "note": null,
                "owner": { "id": "00000000-0000-4000-8000-000000000001" }
            }]
        }),
    )
    .await?;
    let owner_id = result.response["assets"][0]["ownerId"]
        .as_str()
        .expect("explicit UUID should be returned")
        .to_string();
    assert_eq!(owner_id, "00000000-0000-4000-8000-000000000001");
    assert_eq!(result.response["assets"][0]["owner"]["name"], "Unknown");
    assert!(result.response["assets"][0]["note"].is_null());

    let flat = seed::seed(
        &conn,
        json!({ "assets": [{ "ownerId": owner_id, "note": "flat" }] }),
    )
    .await?;
    assert_eq!(flat.response["assets"][0]["note"], "flat");
    Ok(())
}

#[tokio::test]
async fn seed_generates_uuid_v7_and_leaves_sequences_to_sqlite(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Event {
    eventId Id.Uuid @id
    position Sequence.Int
    active Bool?
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let fixture = json!({ "events": [{ "active": null }] });
    let first = seed::seed(&conn, &fixture).await?;
    let second = seed::seed(&conn, &fixture).await?;
    let first = &first.response["events"][0];
    let second = &second.response["events"][0];
    let first_id = uuid::Uuid::parse_str(first["eventId"].as_str().unwrap())?;
    let second_id = uuid::Uuid::parse_str(second["eventId"].as_str().unwrap())?;
    assert_eq!(first_id.get_version_num(), 7);
    assert_eq!(second_id.get_version_num(), 7);
    assert_ne!(first_id, second_id);
    assert_eq!(first["position"], 1);
    assert_eq!(second["position"], 2);
    assert!(second["active"].is_null());

    let error = seed::seed(&conn, json!({ "events": [{}, { "position": 10 }] }))
        .await
        .expect_err("server-managed sequence cannot be supplied");
    assert!(error.to_string().contains("events[1].position"));
    let error = seed::seed(&conn, json!({ "events": [{ "eventId": "bad-id" }] }))
        .await
        .expect_err("invalid identity cannot be supplied");
    assert!(error.to_string().contains("events[0].eventId"));

    let next = seed::seed(&conn, &fixture).await?;
    assert_eq!(next.response["events"][0]["position"], 3);
    Ok(())
}

#[tokio::test]
async fn seed_database_errors_include_paths_and_roll_back() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(
        r#"
@syncable(false)
record User {
    id Id.Int @id
    email String @unique
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let error = seed::seed(
        &conn,
        json!({ "users": [{ "email": "same" }, { "email": "same" }] }),
    )
    .await
    .expect_err("unique violation should fail");
    assert!(error.to_string().contains("users[1]"));

    let count: i64 = conn
        .query("SELECT count(*) FROM users", ())
        .await?
        .next()
        .await?
        .expect("count row")
        .get(0)?;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn seed_aborts_instead_of_inheriting_schema_conflict_policies(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Item {
    id Id.Int @id
    name String @default("original")
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    for conflict in ["REPLACE", "IGNORE", "FAIL", "ROLLBACK"] {
        // SQLite migration files can declare a constraint's default conflict policy.
        conn.execute_batch(&format!(
            "DROP TABLE items;
             CREATE TABLE items (id INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE ON CONFLICT {conflict} DEFAULT 'original',
                updatedAt INTEGER DEFAULT 0);
             INSERT INTO items VALUES (1, 'original', 0);"
        ))
        .await?;
        for colliding in [json!({"id": 3, "name": "original"}), json!({})] {
            let error = seed::seed(
                &conn,
                json!({"items": [{"id": 2, "name": "rollback-me"}, colliding]}),
            )
            .await
            .expect_err(conflict);
            assert!(matches!(error, seed::Error::DatabaseAt { .. }), "{error}");
            assert!(error.to_string().contains("items[1]"), "{error}");
            let mut rows = conn
                .query("SELECT id, name FROM items ORDER BY id", ())
                .await?;
            let original = rows.next().await?.expect("original row still exists");
            assert_eq!(original.get::<i64>(0)?, 1);
            assert_eq!(original.get::<String>(1)?, "original");
            assert!(
                rows.next().await?.is_none(),
                "earlier fixture row rolled back"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn seed_preserves_trigger_conflict_handling_and_refuses_replacing_tables_with_triggers(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Item {
    id Id.Int @id
    name String
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "CREATE TABLE seen (name TEXT PRIMARY KEY);
         INSERT INTO seen VALUES ('known');
         CREATE TEMP TRIGGER remember AFTER INSERT ON main.items BEGIN
            INSERT OR IGNORE INTO seen VALUES (new.name);
         END;",
    )
    .await?;
    seed::seed(&conn, json!({"items": [{"id": 1, "name": "known"}]})).await?;

    conn.execute_batch(
        "DROP TABLE items;
         CREATE TABLE items (id INTEGER PRIMARY KEY,
            name TEXT UNIQUE ON/* block comment */ CONFLICT -- line comment
            \u{feff}REPLACE, updatedAt INTEGER DEFAULT 0);
         INSERT INTO items VALUES (1, 'known', 0);
         CREATE TRIGGER remember AFTER INSERT ON items BEGIN
            INSERT OR IGNORE INTO seen VALUES (new.name);
         END;",
    )
    .await?;
    let error = seed::seed(&conn, json!({"items": [{"id": 2, "name": "known"}]}))
        .await
        .expect_err("seed must not silently replace the original");
    assert!(error.to_string().contains("ON CONFLICT REPLACE"), "{error}");
    let mut rows = conn.query("SELECT id FROM items", ()).await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
    assert!(rows.next().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn seed_distinguishes_jsonb_null_from_sql_null() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Entry {
    id Id.Int @id
    requiredJson Json<String?>
    optionalJson Json<String>?
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let result = seed::seed(
        &conn,
        json!({"entries": [{"requiredJson": null, "optionalJson": null}]}),
    )
    .await?;
    assert!(result.response["entries"][0]["requiredJson"].is_null());
    let mut rows = conn
        .query(
            "SELECT id FROM entries WHERE requiredJson IS jsonb('null') AND optionalJson IS NULL",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
    Ok(())
}

#[tokio::test]
async fn seed_rejects_unknown_fields_legacy_discriminators_and_link_conflicts(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
@syncable(false)
type Status
   = Active

record User {
    id Id.Int @id
    status Status
    posts @link(Post.authorId)
    @public
}

record Post {
    id Id.Int @id
    authorId User.id
    author @link(authorId, User.id)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;

    let unknown = seed::seed(
        &conn,
        json!({ "users": [{ "status": "Active", "missing": 1 }] }),
    )
    .await
    .expect_err("unknown field should fail");
    assert!(unknown.to_string().contains("users[0].missing"));

    let unknown_table = seed::seed(&conn, json!({ "missing": [] }))
        .await
        .expect_err("unknown table should fail");
    assert!(unknown_table
        .to_string()
        .contains("unknown seed table 'missing'"));

    for key in ["type", "type_", "$"] {
        let legacy = seed::seed(&conn, json!({ "users": [{ "status": { key: "Active" } }] }))
            .await
            .expect_err("legacy discriminator should fail");
        assert!(legacy.to_string().contains("must use '_type'"));

        let extra = seed::seed(
            &conn,
            json!({ "users": [{ "status": { "_type": "Active", key: "Active" } }] }),
        )
        .await
        .expect_err("undeclared payload field should fail");
        assert!(extra
            .to_string()
            .contains(&format!(".{key} is not a field")));
    }

    let extra = seed::seed(
        &conn,
        json!({ "users": [{ "status": { "_type": "Active", "extra": true } }] }),
    )
    .await
    .expect_err("unit variant fields should fail");
    assert!(extra.to_string().contains("extra is not a field"));

    let conflict = seed::seed(
        &conn,
        json!({
            "users": [{
                "id": 1,
                "status": "Active",
                "posts": [{ "authorId": 99 }]
            }]
        }),
    )
    .await
    .expect_err("derived foreign key conflict should fail");
    assert!(conflict
        .to_string()
        .contains("users[0].posts[0].authorId conflicts"));
    Ok(())
}

#[tokio::test]
async fn seed_handles_representative_flat_import_volume() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(
        r#"
@syncable(false)
record Item {
    id Id.Int @id
    externalId String @unique
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let rows = (0..500)
        .map(|index| json!({ "externalId": format!("pdf-{index}") }))
        .collect::<Vec<_>>();

    let result = seed::seed(&conn, json!({ "items": rows })).await?;
    assert_eq!(result.response["items"].as_array().unwrap().len(), 500);
    Ok(())
}
