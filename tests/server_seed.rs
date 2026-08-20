#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::seed;
use serde_json::json;

#[tokio::test]
async fn seed_inserts_nested_rows_and_serializes_values() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(
        r#"
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
        .expect("generated UUID should be returned")
        .to_string();
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
async fn seed_database_errors_include_paths_and_roll_back() -> Result<(), Box<dyn std::error::Error>>
{
    let db = TestDatabase::new(
        r#"
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
async fn seed_rejects_unknown_fields_legacy_discriminators_and_link_conflicts(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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

    let legacy = seed::seed(
        &conn,
        json!({ "users": [{ "status": { "type": "Active" } }] }),
    )
    .await
    .expect_err("legacy discriminator should fail");
    assert!(legacy.to_string().contains("must use '_type'"));

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
