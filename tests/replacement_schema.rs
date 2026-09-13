use pyre::server::manifest::{BoundManifest, Manifest, PyreSession};
use pyre::server::query::{self, BatchBinding};
use pyre::server::sync::{Replacement, ReplacementRequest, SyncFence, SyncServer};
use pyre::sync_deltas::AffectedRowTableGroup;
use pyre::{ast, generate, parser, sync, typecheck};
use serde_json::json;
use std::collections::HashMap;

fn context(source: &str) -> typecheck::Context {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap()
}

fn manifest(context: &typecheck::Context) -> Manifest {
    let mut files = vec![];
    generate::manifest::generate_schema(context, &mut files);
    serde_json::from_str(&files[0].contents).unwrap()
}

#[test]
fn namespace_contracts_are_isolated_but_share_the_allowlist_fingerprint() {
    let compile = |permission: &str, extra_session: &str| {
        let mut main = ast::Schema {
            namespace: "Main".into(),
            ..Default::default()
        };
        parser::run("main.pyre", &format!("session {{\n userId Int\n{extra_session}\n}}\ntype Choice\n = Open\n | Closed\nrecord Entry {{\n @public\n id Id.Int @id\n choice Choice\n}}\n"), &mut main).unwrap();
        let mut archive = ast::Schema {
            namespace: "Archive".into(),
            ..Default::default()
        };
        parser::run("archive.pyre", &format!("type ArchiveOnly\n = Stored\n | Retired\nrecord Saved {{\n @allow(*) {{ {permission} }}\n id Id.Int @id\n state ArchiveOnly\n}}\n"), &mut archive).unwrap();
        typecheck::check_schema(&ast::Database {
            schemas: vec![main, archive],
        })
        .unwrap()
    };
    let original = manifest(&compile("True", ""));
    let changed_archive = manifest(&compile("False", ""));
    assert_eq!(
        original.replacement_contracts["Main"],
        changed_archive.replacement_contracts["Main"]
    );
    assert_ne!(
        original.replacement_contracts["Archive"],
        changed_archive.replacement_contracts["Archive"]
    );
    assert_ne!(original.fingerprint(), changed_archive.fingerprint());
    let changed_session = manifest(&compile("True", " extra String?"));
    for namespace in ["Main", "Archive"] {
        assert_ne!(
            original.replacement_contracts[namespace],
            changed_session.replacement_contracts[namespace]
        );
    }
    assert!(generate::manifest::replacement_contract(&compile("True", ""), "Unknown").is_none());
}

#[test]
fn stored_session_retains_local_uuid_brand_for_permissions() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "session {\n principalId Principal.id?\n}\nrecord Principal {\n @public\n id Id.Uuid @id\n}\nrecord Member {\n @allow(*) { principalId == Session.principalId }\n id Id.Int @id\n principalId Principal.id\n}\n",
        &mut schema,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![schema],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    let source = pyre::db::migrate::schema_to_storage_string(&context, &database.schemas[0]);
    let loaded = pyre::db::introspect::from_raw(pyre::db::introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: pyre::db::introspect::MigrationState::NoMigrationTable,
        schema_source: source,
        links: vec![],
    });
    let pyre::db::introspect::SchemaResult::Success {
        context: restored, ..
    } = loaded.schema
    else {
        panic!("stored local session should typecheck: {:?}", loaded.schema);
    };
    assert_eq!(
        generate::manifest::replacement_contract(&context, ast::DEFAULT_SCHEMANAME),
        generate::manifest::replacement_contract(&restored, ast::DEFAULT_SCHEMANAME),
    );
}

#[test]
fn namespace_contracts_survive_standalone_storage_and_dynamic_migration() {
    let mut main = ast::Schema {
        namespace: "Main".into(),
        ..Default::default()
    };
    parser::run("main.pyre", "session {\n owner Main.Entry.id?\n claims Json<Claims>?\n}\ntype Claims\n = Member { owner Main.Entry.id }\ntype Choice\n = Open\n | Closed\nrecord Entry {\n @public\n id Id.Uuid @id\n}\n", &mut main).unwrap();
    let mut archive = ast::Schema {
        namespace: "Archive".into(),
        ..Default::default()
    };
    parser::run(
        "archive.pyre",
        "record Saved {\n @public\n id Id.Int @id\n choice Choice\n}\n",
        &mut archive,
    )
    .unwrap();
    let mut database = ast::Database {
        schemas: vec![main, archive],
    };
    ast::resolve_id_brands(&mut database);
    let context = typecheck::check_schema(&database).unwrap();
    for schema in &database.schemas {
        let source = generate::to_string::standalone_schema_to_string(&context, schema);
        let loaded = pyre::db::introspect::from_raw(pyre::db::introspect::IntrospectionRaw {
            tables: vec![],
            migration_state: pyre::db::introspect::MigrationState::NoMigrationTable,
            schema_source: source.clone(),
            links: vec![],
        });
        let pyre::db::introspect::SchemaResult::Success {
            schema: restored,
            context: restored_context,
        } = &loaded.schema
        else {
            panic!(
                "standalone {} failed: {source}\n{:?}",
                schema.namespace, loaded.schema
            );
        };
        assert_eq!(restored.namespace, schema.namespace);
        assert_eq!(
            generate::manifest::replacement_contract(&context, &schema.namespace),
            generate::manifest::replacement_contract(restored_context, &schema.namespace),
            "{}: {source}\noriginal session: {:?}\nrestored session: {:?}",
            schema.namespace,
            context.session,
            restored_context.session
        );
        pyre::db::migrate::migrate_dynamic("roundtrip".into(), &loaded, &source, "schema.pyre")
            .unwrap();
    }
}

async fn replacement(
    conn: &libsql::Connection,
    context: &typecheck::Context,
    manifest: &Manifest,
    session: &PyreSession,
) -> Replacement {
    let fingerprint = manifest.fingerprint();
    let namespace = ast::DEFAULT_SCHEMANAME;
    let binding = BatchBinding {
        database_id: "test",
        namespace,
        manifest: &fingerprint,
        instance: "tab",
        auth_generation: 0,
    };
    let epoch = conn
        .query("SELECT database_epoch FROM _pyre_sync", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    SyncServer::new(context)
        .replacement(
            conn,
            manifest,
            &binding,
            &ReplacementRequest {
                version: 1,
                request_id: "read".into(),
                target: 0,
                fence: SyncFence {
                    database_id: "test".into(),
                    namespace: namespace.into(),
                    manifest: fingerprint.clone(),
                    instance: "tab".into(),
                    auth_generation: 0,
                    database_epoch: epoch,
                },
            },
            session,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn replacement_json_permissions_match_compiled_queries_including_links() {
    for json_type in ["Json<Int>", "Json"] {
        for rhs in ["Session.value", "7", "null"] {
            if json_type == "Json" && rhs != "Session.value" {
                continue;
            }
            for (operator, expected) in [("==", vec![1]), ("!=", vec![2, 3])] {
                let source = format!(
                    r#"
session {{
    value {json_type}?
}}
record Membership {{
    id Int @id
    workspaceId Int
    value {json_type}?
    @allow(query) {{ value {operator} {rhs} }}
    @allow(insert, update, delete) {{ False }}
}}
record Workspace {{
    id Int @id
    memberships @link(Membership.workspaceId)
    @allow(query) {{ exists memberships {{ value {operator} {rhs} }} }}
    @allow(insert, update, delete) {{ False }}
}}
"#
                );
                let context = context(&source);
                let db = libsql::Builder::new_local(":memory:")
                    .build()
                    .await
                    .unwrap();
                let conn = db.connect().unwrap();
                pyre::server::schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, &source)
                    .await
                    .unwrap();
                conn.execute_batch("INSERT INTO workspaces(id) VALUES(1),(2),(3); INSERT INTO memberships(id,workspaceId,value) VALUES(1,1,jsonb('7')),(2,2,jsonb('8')),(3,3,NULL);").await.unwrap();
                let queries = parser::parse_query(
                    "query.pyre",
                    "query Visible { membership { id } workspace { id } }",
                )
                .unwrap();
                let info = typecheck::check_queries(&queries, &context).unwrap();
                let mut files = vec![];
                generate::manifest::generate_queries(&context, &queries, &info, &mut files);
                let manifest: Manifest = serde_json::from_str(&files[0].contents).unwrap();
                for value in [json!(7), json!(null)] {
                    let session =
                        PyreSession::new(json!({"value":value}), &manifest.session_schema).unwrap();
                    let snapshot = replacement(&conn, &context, &manifest, &session).await;
                    let result = query::run(
                        &conn,
                        &manifest,
                        manifest.queries.keys().next().unwrap(),
                        json!({}),
                        &session,
                    )
                    .await
                    .unwrap();
                    let expected = if rhs == "null" || (rhs == "Session.value" && value.is_null()) {
                        if operator == "==" {
                            vec![3]
                        } else {
                            vec![1, 2]
                        }
                    } else {
                        expected.clone()
                    };
                    for (table, field) in
                        [("memberships", "membership"), ("workspaces", "workspace")]
                    {
                        let mut ids = snapshot.tables[table]
                            .rows
                            .iter()
                            .map(|row| row["id"].as_i64().unwrap())
                            .collect::<Vec<_>>();
                        ids.sort();
                        assert_eq!(
                            ids, expected,
                            "{json_type}: {operator} {rhs}, session={value}, {table}"
                        );
                        let mut query_ids = result.response[field]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|row| row["id"].as_i64().unwrap())
                            .collect::<Vec<_>>();
                        query_ids.sort();
                        assert_eq!(ids, query_ids);
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn replacement_decodes_json_strings_exactly_once() {
    let source = r#"
type Payload = Text { value Json<String> }
record Item {
    id Int @id
    label Json<String>
    raw Json
    optional Json<String?>
    payload Payload
    @public
}
"#;
    let context = context(source);
    let manifest = manifest(&context);
    let session = PyreSession::new(json!({}), &manifest.session_schema).unwrap();
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    pyre::server::schema::ensure_database(&conn, ast::DEFAULT_SCHEMANAME, source)
        .await
        .unwrap();
    let strings = [
        "hello",
        "7",
        "null",
        "true",
        "[1]",
        "{\"x\":1}",
        "\"quoted\"",
        "{invalid",
    ];
    for (index, value) in strings.iter().enumerate() {
        conn.execute("INSERT INTO items(id,label,raw,optional,payload,payload__value) VALUES(?1,jsonb(?2),jsonb(?2),jsonb('null'),'Text',jsonb(?2))",
            libsql::params![index as i64, serde_json::to_string(value).unwrap()]).await.unwrap();
    }
    let snapshot = replacement(&conn, &context, &manifest, &session).await;
    assert!(snapshot.complete);
    assert_eq!(snapshot.tables["items"].rows.len(), strings.len());
    for row in &snapshot.tables["items"].rows {
        let value = strings[row["id"].as_u64().unwrap() as usize];
        assert_eq!(row["label"], value);
        assert_eq!(row["raw"], value);
        assert_eq!(row["optional"], json!(null));
        assert_eq!(row["payload"], json!({"_type":"Text", "value":value}));
    }
}

#[test]
fn replacement_contract_authenticates_permissions_session_codecs_and_sync_scope() {
    let source = "session {\n    userId Int\n}\nrecord Item {\n    id Int @id\n    owner Int\n    @allow(query) { owner == Session.userId }\n    @allow(insert, update, delete) { False }\n}\n";
    let original = context(source);
    let compiled = manifest(&original);
    assert!(compiled.matches_context(&original));
    let bound = BoundManifest::new(compiled.clone(), &original).unwrap();
    assert_eq!(bound.fingerprint(), compiled.fingerprint());
    assert!(bound.authorizes_namespace(ast::DEFAULT_SCHEMANAME));
    assert_eq!(
        compiled.compiled_contract,
        generate::manifest::compiled_schema_contract(&original)
    );
    for changed in [
        source.replace("owner == Session.userId", "True"),
        source.replace("userId Int", "userId Int?"),
        source.replace("owner Int", "owner Int?"),
        format!("@syncable(false)\n{source}"),
    ] {
        assert!(!compiled.matches_context(&context(&changed)));
        assert!(BoundManifest::new(compiled.clone(), &context(&changed)).is_err());
    }
    let mut invalid = compiled.clone();
    invalid.compiled_contract.clear();
    assert!(!invalid.matches_context(&original));
    assert!(BoundManifest::new(invalid.clone(), &original).is_err());
    invalid = compiled;
    invalid.version = 2;
    assert!(!invalid.matches_context(&original));
    assert!(BoundManifest::new(invalid, &original).is_err());
}

const LINKED: &str = r#"
session {
    userId Int
}
record Membership {
    id Int @id
    workspaceId Int
    userId Int
    @allow(query) { False }
    @allow(insert, update, delete) { False }
}
record Workspace {
    id Int @id
    memberships @link(Membership.workspaceId)
    @allow(query) { exists memberships { userId == Session.userId } }
    @allow(insert, update, delete) { False }
}
"#;

#[tokio::test]
async fn normally_checked_linked_schema_supports_replacement_and_rejects_legacy_pages() {
    let context = context(LINKED);
    let namespace = context.valid_namespaces.iter().next().unwrap();
    assert!(sync::requires_replacement(&context));
    let session = HashMap::from([("userId".into(), sync::SessionValue::Integer(7))]);
    assert!(sync::get_sync_status_statement(&HashMap::new(), &context, &session).is_err());
    let plan = sync::get_replacement_sql(&context, &session, namespace).unwrap();
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch("CREATE TABLE workspaces(id INTEGER PRIMARY KEY, updatedAt INTEGER); CREATE TABLE memberships(id INTEGER PRIMARY KEY, workspaceId INTEGER, userId INTEGER, updatedAt INTEGER); INSERT INTO workspaces VALUES(1,0),(2,0); INSERT INTO memberships VALUES(1,1,7,0),(2,2,8,0);").await.unwrap();
    let workspace = plan
        .tables
        .iter()
        .find(|table| table.table_name == "workspaces")
        .unwrap();
    let mut rows = conn.query(&workspace.sql[0], [7]).await.unwrap();
    let values: serde_json::Value = serde_json::from_str(
        &rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(values.as_array().unwrap().len(), 1);
    drop(rows);
    conn.execute("DELETE FROM memberships WHERE userId = 7", ())
        .await
        .unwrap();
    let mut rows = conn.query(&workspace.sql[0], [7]).await.unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "[]"
    );
    // A query-only namespace remains out of replacement scope.
    let query_only = self::context(&format!("@syncable(false)\n{LINKED}"));
    assert!(!sync::requires_replacement(&query_only));
    assert!(sync::get_replacement_sql(&query_only, &session, namespace)
        .unwrap()
        .tables
        .is_empty());
}

#[test]
fn strict_replacement_rejects_bad_codecs_and_incomplete_storage_rows() {
    let context = context(
        r#"
type State
    = Open { active Bool }
    | Closed
type Document
    = Details { count Int, enabled Bool }
record Item {
    id Int @id
    active Bool
    state State
    document Json<Document>
    @public
}
"#,
    );
    let namespace = context.valid_namespaces.iter().next().unwrap();
    let plan = sync::get_replacement_sql(&context, &HashMap::new(), namespace).unwrap();
    let table = &plan.tables[0];
    let object = json!({"id":1,"active":1,"state":"Open","state__active":0,"document":{"_type":"Details","count":2,"enabled":true},"updatedAt":0});
    let group = AffectedRowTableGroup {
        table_name: table.table_name.clone(),
        headers: table.headers.clone(),
        rows: vec![table
            .headers
            .iter()
            .map(|name| object[name].clone())
            .collect()],
    };
    assert!(sync::reshape_replacement_table(&context, namespace, &group).is_ok());
    for (field, value) in [
        ("active", json!(2)),
        ("id", json!("not an integer")),
        ("state", json!("Unknown")),
        ("state__active", json!(null)),
        (
            "document",
            json!({"_type":"Details","count":"bad","enabled":true}),
        ),
        ("document", json!({"_type":"Details","count":2,"enabled":1})),
        ("document", json!({"_type":"Unknown"})),
    ] {
        let mut invalid = group.clone();
        let index = invalid
            .headers
            .iter()
            .position(|name| name == field)
            .unwrap();
        invalid.rows[0][index] = value;
        assert!(
            sync::reshape_replacement_table(&context, namespace, &invalid).is_err(),
            "{field}"
        );
    }
    let mut short = group.clone();
    short.rows[0].pop();
    assert!(sync::reshape_replacement_table(&context, namespace, &short).is_err());
    let mut missing = group.clone();
    missing.headers.pop();
    missing.rows[0].pop();
    assert!(sync::reshape_replacement_table(&context, namespace, &missing).is_err());
    assert!(sync::reshape_replacement_table(&context, "wrong", &group).is_err());
}

#[test]
fn replacement_rejects_legacy_uuid_strings_without_coercing_case_or_identity() {
    let context = context("record Item {\n @public\n key Id.Uuid @id\n parent Item.key?\n references Json<List<Id.Uuid>>\n}\n");
    let plan =
        sync::get_replacement_sql(&context, &HashMap::new(), ast::DEFAULT_SCHEMANAME).unwrap();
    let table = &plan.tables[0];
    let uuid = "ABCDEFAB-CDEF-0123-4567-ABCDEFABCDEF";
    let object = json!({"key":uuid,"parent":uuid,"references":[uuid],"updatedAt":0});
    let group = AffectedRowTableGroup {
        table_name: table.table_name.clone(),
        headers: table.headers.clone(),
        rows: vec![table
            .headers
            .iter()
            .map(|name| object[name].clone())
            .collect()],
    };
    let rows = sync::reshape_replacement_table(&context, ast::DEFAULT_SCHEMANAME, &group).unwrap();
    assert_eq!(
        rows.rows[0][rows.headers.iter().position(|name| name == "key").unwrap()],
        uuid
    );
    for invalid in [
        "symbolic",
        "abcdefabcdef01234567abcdefabcdef",
        "ABCDEFAB-CDEF-0123-4567-ABCDEFABCDEF\n",
    ] {
        for field in ["key", "parent", "references"] {
            let mut malformed = group.clone();
            let index = table.headers.iter().position(|name| name == field).unwrap();
            malformed.rows[0][index] = if field == "references" {
                json!([invalid])
            } else {
                json!(invalid)
            };
            assert!(
                sync::reshape_replacement_table(&context, ast::DEFAULT_SCHEMANAME, &malformed)
                    .is_err(),
                "{field}: {invalid:?}"
            );
        }
    }
}
