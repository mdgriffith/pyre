use pyre::ast;
use pyre::parser;
use pyre::sync::{get_sync_sql, SyncCursor, SyncStatusResult, TableSyncStatus};
use pyre::sync_deltas::AffectedRowTableGroup;
use pyre::sync_shape::reshape_table_groups;
use pyre::typecheck;
use serde_json::json;

#[test]
fn sync_sql_marks_json_columns_for_runtime_decoding() {
    let schema_source = r#"
record GameEntity {
    id Int @id
    attrs Json
    updatedAt Int
    @public
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");

    let sync_status = SyncStatusResult {
        server_revision: None,
        tables: vec![TableSyncStatus {
            table_name: "gameEntities".to_string(),
            sync_layer: 0,
            needs_sync: true,
            max_updated_at: None,
            permission_hash: "perm".to_string(),
        }],
    };

    let result = match get_sync_sql(
        &sync_status,
        &SyncCursor::new(),
        &context,
        &Default::default(),
        100,
    ) {
        Ok(result) => result,
        Err(_) => panic!("sync sql should generate"),
    };

    assert_eq!(result.tables.len(), 1, "expected one sync table");
    assert_eq!(result.tables[0].json_columns, vec!["attrs".to_string()]);
    assert!(
        result.tables[0].sql[0].contains("json(\"gameEntities\".\"attrs\") as \"attrs\""),
        "expected sync SQL to decode JSONB columns via json()"
    );
    assert!(
        result.tables[0].sql[0].contains("AS \"_pyre_rows\""),
        "expected sync SQL to aggregate rows for cheaper runtime materialization"
    );
    assert!(
        result.tables[0].sql[0].contains("json(\"attrs\")"),
        "expected aggregate row arrays to preserve JSON columns as JSON values"
    );
}

#[test]
fn sync_status_sql_uses_bound_params_for_session_permissions() {
    let schema_source = r#"
session {
    workspaceSlug String
}

record Note {
    id Int @id
    workspaceSlug String
    updatedAt Int
    @allow(query) { workspaceSlug == Session.workspaceSlug }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    let mut session = std::collections::HashMap::new();
    session.insert(
        "workspaceSlug".to_string(),
        pyre::sync::SessionValue::Text("x' OR 1=1 --".to_string()),
    );

    let statement = pyre::sync::get_sync_status_statement(&SyncCursor::new(), &context, &session)
        .expect("sync status statement should generate");

    assert!(statement.sql.contains("\"notes\".\"workspaceSlug\" = ?"));
    assert!(!statement.sql.contains("x' OR 1=1"));
    assert_eq!(statement.params.len(), 1);
}

#[test]
fn sync_status_sql_expands_session_membership_lists() {
    let schema_source = r#"
session {
    activeClocktowerGameIds Json<List<String>>
}

record ClocktowerGame {
    id String @id
    updatedAt Int
    @allow(query) { id in Session.activeClocktowerGameIds }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    let mut session = std::collections::HashMap::new();
    session.insert(
        "activeClocktowerGameIds".to_string(),
        pyre::sync::SessionValue::Text(r#"["game-1"]"#.to_string()),
    );

    let statement = pyre::sync::get_sync_status_statement(&SyncCursor::new(), &context, &session)
        .expect("sync status statement should generate");

    assert!(statement
        .sql
        .contains("\"clocktowerGames\".\"id\" in (select value from json_each(?))"));
    assert_eq!(statement.params.len(), 1);
}

#[test]
fn sync_sql_caps_page_size() {
    let schema_source = r#"
record Note {
    id Int @id
    updatedAt Int
    @public
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    let sync_status = SyncStatusResult {
        server_revision: None,
        tables: vec![TableSyncStatus {
            table_name: "notes".to_string(),
            sync_layer: 0,
            needs_sync: true,
            max_updated_at: None,
            permission_hash: "perm".to_string(),
        }],
    };

    let result = get_sync_sql(
        &sync_status,
        &SyncCursor::new(),
        &context,
        &Default::default(),
        999_999,
    )
    .expect("sync sql should generate");

    assert!(result.tables[0].sql[0].contains("LIMIT 5001"));
}

#[test]
fn sync_cursor_rejects_unknown_tables() {
    let schema_source = r#"
record Note {
    id Int @id
    updatedAt Int
    @public
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    let mut cursor = SyncCursor::new();
    cursor.insert(
        "not_a_table".to_string(),
        pyre::sync::TableCursor {
            last_seen_updated_at: Some(1),
            permission_hash: "perm".to_string(),
        },
    );

    let err = pyre::sync::get_sync_status_statement(&cursor, &context, &Default::default())
        .expect_err("unknown cursor table should be rejected");

    match err {
        pyre::sync::SyncError::InvalidSyncCursor(message) => {
            assert!(message.contains("unknown table"));
        }
        _ => panic!("expected invalid sync cursor error"),
    }
}

#[test]
fn sync_cursor_rejects_oversized_permission_hashes() {
    let schema_source = r#"
record Note {
    id Int @id
    updatedAt Int
    @public
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");
    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    let mut cursor = SyncCursor::new();
    cursor.insert(
        "notes".to_string(),
        pyre::sync::TableCursor {
            last_seen_updated_at: Some(1),
            permission_hash: "x".repeat(pyre::sync::MAX_SYNC_CURSOR_PERMISSION_HASH_BYTES + 1),
        },
    );

    let err = pyre::sync::get_sync_status_statement(&cursor, &context, &Default::default())
        .expect_err("oversized permission hash should be rejected");

    match err {
        pyre::sync::SyncError::InvalidSyncCursor(message) => {
            assert!(message.contains("permission_hash"));
        }
        _ => panic!("expected invalid sync cursor error"),
    }
}

#[test]
fn query_only_namespaces_are_excluded_from_sync_sql() {
    let main_source = r#"
@syncable(false)

record Account {
    id Int @id
    updatedAt Int
    @public
}
"#;
    let campaign_source = r#"
record Quest {
    id Int @id
    updatedAt Int
    @public
}
"#;

    let mut main = ast::Schema {
        namespace: "Main".to_string(),
        ..ast::Schema::default()
    };
    parser::run("schema/Main/schema.pyre", main_source, &mut main).expect("main schema parses");

    let mut campaign = ast::Schema {
        namespace: "Campaign".to_string(),
        ..ast::Schema::default()
    };
    parser::run(
        "schema/Campaign/schema.pyre",
        campaign_source,
        &mut campaign,
    )
    .expect("campaign schema parses");

    let database = ast::Database {
        schemas: vec![main, campaign],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");

    let status_sql =
        pyre::sync::get_sync_status_sql(&SyncCursor::new(), &context, &Default::default())
            .expect("sync status SQL should generate");
    assert!(status_sql.contains("quests"));
    assert!(!status_sql.contains("accounts"));

    let sync_status = SyncStatusResult {
        server_revision: None,
        tables: vec![
            TableSyncStatus {
                table_name: "accounts".to_string(),
                sync_layer: 0,
                needs_sync: true,
                max_updated_at: None,
                permission_hash: "main".to_string(),
            },
            TableSyncStatus {
                table_name: "quests".to_string(),
                sync_layer: 0,
                needs_sync: true,
                max_updated_at: None,
                permission_hash: "campaign".to_string(),
            },
        ],
    };
    let sync_sql = get_sync_sql(
        &sync_status,
        &SyncCursor::new(),
        &context,
        &Default::default(),
        100,
    )
    .expect("sync SQL should generate");

    assert_eq!(sync_sql.tables.len(), 1);
    assert_eq!(sync_sql.tables[0].table_name, "quests");
}

#[test]
fn all_query_only_schemas_have_empty_sync_status() {
    let schema_source = r#"
@syncable(false)

record Account {
    id Int @id
    updatedAt Int
    @public
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");
    let status_sql =
        pyre::sync::get_sync_status_sql(&SyncCursor::new(), &context, &Default::default())
            .expect("sync status SQL should generate");

    assert_eq!(
        status_sql,
        "SELECT NULL AS table_name, NULL AS sync_layer, NULL AS permission_hash, NULL AS last_seen_updated_at, NULL AS max_updated_at, (SELECT value FROM _pyre_sync WHERE key = 'server_revision') AS server_revision"
    );
}

#[test]
fn sync_sql_includes_flattened_custom_type_columns() {
    let schema_source = r#"
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
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");

    let sync_status = SyncStatusResult {
        server_revision: None,
        tables: vec![TableSyncStatus {
            table_name: "maps".to_string(),
            sync_layer: 0,
            needs_sync: true,
            max_updated_at: None,
            permission_hash: "perm".to_string(),
        }],
    };

    let result = match get_sync_sql(
        &sync_status,
        &SyncCursor::new(),
        &context,
        &Default::default(),
        100,
    ) {
        Ok(result) => result,
        Err(_) => panic!("sync sql should generate"),
    };

    assert_eq!(
        result.tables[0].headers,
        vec![
            "id".to_string(),
            "tiling".to_string(),
            "tiling__tileRootKey".to_string(),
            "tiling__tileWidth".to_string(),
            "tiling__format".to_string(),
            "updatedAt".to_string(),
        ]
    );
}

#[test]
fn reshape_table_groups_reconstructs_custom_types_for_sync_payloads() {
    let schema_source = r#"
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
    tiling Tiling?
    updatedAt Int
    @public
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema should typecheck");

    let reshaped = reshape_table_groups(
        &[AffectedRowTableGroup {
            table_name: "maps".to_string(),
            headers: vec![
                "id".to_string(),
                "name".to_string(),
                "tiling".to_string(),
                "tiling__tileRootKey".to_string(),
                "tiling__tileWidth".to_string(),
                "tiling__format".to_string(),
                "updatedAt".to_string(),
            ],
            rows: vec![vec![
                json!(1),
                json!("World"),
                json!("Tiling"),
                json!("tiles/root"),
                json!(256),
                json!("Png"),
                json!(1700000000),
            ]],
        }],
        &context,
    );

    assert_eq!(reshaped.len(), 1);
    assert_eq!(
        reshaped[0].headers,
        vec!["id", "name", "tiling", "updatedAt"]
    );
    assert_eq!(
        reshaped[0].rows[0],
        vec![
            json!(1),
            json!("World"),
            json!({
                "_type": "Tiling",
                "tileRootKey": "tiles/root",
                "tileWidth": 256,
                "format": { "_type": "Png" }
            }),
            json!(1700000000),
        ]
    );
}
