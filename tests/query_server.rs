#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::manifest::{Manifest, PyreSession, QueryManifest};
use pyre::server::query;
use serde_json::json;

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

#[tokio::test]
async fn run_select_query_formats_response() -> Result<(), Box<dyn std::error::Error>> {
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
async fn run_insert_mutation_extracts_affected_rows_in_sync_mode(
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
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateNote($body: String) {
    note {
        body = $body
        updatedAt = 10
        id
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
        json!({ "body": "one" }),
        &session,
    )
    .await?;

    assert_eq!(result.response, json!({}));
    assert_eq!(result.affected_rows.len(), 1);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(result.affected_rows[0].rows.len(), 1);

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
    id Int @id
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
insert CreateNote($attrs: Json) {
    note {
        ownerId = Session.userId
        attrs = $attrs
        updatedAt = 10
        id
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
        json!({ "attrs": { "theme": "forest" } }),
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
delete DeleteNote($id: Int) {
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
        json!({ "id": 1 }),
        &session,
    )
    .await?;

    assert_eq!(result.affected_rows.len(), 1);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(result.affected_rows[0].rows[0][0], json!(1));
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
    id Int @id
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

    let created = query::run(
        &conn,
        &manifest,
        &create.id,
        json!({ "body": "generated" }),
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
async fn run_insert_mutation_binds_repeated_json_union_parameter(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Visibility
   = Hidden
   | Everyone
   | Users {
        userId Int
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

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "visibility": { "_type": "Hidden" } }),
        &session,
    )
    .await?;

    assert_eq!(result.response["scene"][0]["id"], json!(1));

    let mut rows = conn.query("select visibility from scenes", ()).await?;
    let row = rows.next().await?.expect("scene row should exist");
    assert_eq!(row.get::<String>(0)?, "Hidden");

    Ok(())
}

#[tokio::test]
async fn run_multi_top_level_query_formats_all_response_keys(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
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
