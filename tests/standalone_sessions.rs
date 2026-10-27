use pyre::server::{schema, seed};
use pyre::{ast, parser, typecheck};
use serde_json::json;

fn parse(namespace: &str, source: &str) -> ast::Schema {
    let mut schema = ast::Schema {
        namespace: namespace.into(),
        ..ast::Schema::default()
    };
    parser::run("schema.pyre", source, &mut schema).expect("schema parses");
    schema
}

fn fixture() -> (ast::Database, typecheck::Context) {
    let shared = parse(
        "Main",
        r#"
session {
    principalId Principal.id?
    userId User.id?
    currentGameId Main.Game.id?
    displayScreenId ScreenPresentation.id?
    participantId Clocktower.Participant.id?
    admission Admission?
    gameIds Json<List<Game.id>>
    isAdmin Bool
}
"#,
    );
    let mut main = parse(
        "Main",
        r#"
@syncable(false)
type Admission
    = Admitted { principalId Principal.id, users Json<Dict<User.id?>> }
    | Guest
record Principal {
    @public
    id Id.Uuid @id
}
record User {
    @public
    id Id.Int @id
}
record Game {
    @public
    id Id.Uuid @id
}
"#,
    );
    main.session = shared.session;
    let campaign = parse(
        "Campaign",
        r#"
record ScreenPresentation {
    @public
    id Id.Uuid @id
}
"#,
    );
    let clocktower = parse(
        "Clocktower",
        r#"
record Participant {
    @allow(query) { And(id == Session.participantId, principalId == Session.principalId) }
    @allow(insert, update, delete) { Session.isAdmin == True }
    id Id.Uuid @id
    principalId Main.Principal.id
    name String
}
"#,
    );
    let database = ast::Database {
        schemas: vec![main, campaign, clocktower],
    };
    let context = typecheck::check_schema(&database).expect("complete project typechecks");
    (database, context)
}

#[tokio::test]
async fn fresh_namespace_databases_load_and_seed_shared_identity_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let (database, context) = fixture();
    let temp = tempfile::tempdir()?;
    for namespace in &database.schemas {
        let (_, identities) = pyre::generate::to_string::standalone_schema(&context, namespace);
        for (mode, source) in [
            (
                "generated",
                pyre::generate::to_string::standalone_schema_to_string(&context, namespace),
            ),
            (
                "stored",
                pyre::db::migrate::schema_to_storage_string(&context, namespace),
            ),
        ] {
            let path = temp
                .path()
                .join(format!("{}-{mode}.db", namespace.namespace));
            let db = libsql::Builder::new_local(&path).build().await?;
            let conn = db.connect()?;
            schema::ensure_database_with_external_identities(
                &conn,
                &namespace.namespace,
                &source,
                &identities,
            )
            .await
            .unwrap_or_else(|error| panic!("{} {mode}: {error:?}\n{source}", namespace.namespace));
            let loaded = schema::load_schema_from_database(&conn).await?;
            let local = loaded.context()?;
            assert_eq!(loaded.schema()?.namespace, namespace.namespace);
            for reference in [
                "Principal.id",
                "User.id",
                "Main.Game.id",
                "ScreenPresentation.id",
                "Clocktower.Participant.id",
            ] {
                assert_eq!(
                    typecheck::resolve_query_param_type(local, reference),
                    typecheck::resolve_query_param_type(&context, reference),
                    "{reference}"
                );
            }
            assert_eq!(
                pyre::generate::to_string::standalone_schema_to_string(local, loaded.schema()?),
                source,
                "regenerating a loaded snapshot must preserve references and metadata"
            );
            let session = local.session.as_ref().unwrap();
            for (name, expected) in [
                ("principalId", ast::ConcreteSerializationType::IdUuid),
                ("userId", ast::ConcreteSerializationType::IdInt),
                ("currentGameId", ast::ConcreteSerializationType::IdUuid),
                ("displayScreenId", ast::ConcreteSerializationType::IdUuid),
                ("participantId", ast::ConcreteSerializationType::IdUuid),
            ] {
                let column = ast::collect_columns(&session.fields)
                    .into_iter()
                    .find(|column| column.name == name)
                    .unwrap();
                assert!(column.nullable);
                assert_eq!(
                    column.type_.to_serialization_type().into_concrete(),
                    Some(expected),
                    "{name}"
                );
            }
            assert_eq!(
                local.tables.len(),
                if namespace.namespace == "Main" { 3 } else { 1 }
            );
            let uuid = "01900000-0000-7000-8000-000000000001";
            let mut value = json!({
                "principalId":uuid, "userId":42, "currentGameId":uuid,
                "displayScreenId":uuid, "participantId":uuid,
                "admission":{"_type":"Admitted", "principalId":uuid, "users":{"first":42,"none":null}},
                "gameIds":[uuid], "isAdmin":false
            });
            let prepared = pyre::session::prepare_session(local, &value).unwrap();
            assert_eq!(
                prepared["principalId"],
                pyre::sync::SessionValue::Text(uuid.into())
            );
            assert_eq!(prepared["userId"], pyre::sync::SessionValue::Integer(42));
            value["userId"] = json!(uuid);
            assert!(pyre::session::prepare_session(local, &value).is_err());
            value["userId"] = json!(42);
            value["principalId"] = json!(42);
            assert!(pyre::session::prepare_session(local, &value).is_err());
            if namespace.namespace == "Clocktower" {
                seed::seed(
                    &conn,
                    json!({"participants": [{"id":uuid, "principalId":uuid, "name":"Player"}]}),
                )
                .await?;
                let query = |reference: &str| {
                    parser::parse_query("query.pyre", &format!(
                    "query Find($id: {reference}) {{ participant {{ @where {{ principalId == $id }} id }} }}"
                )).unwrap()
                };
                assert!(typecheck::check_queries(&query("Principal.id"), local).is_ok());
                assert!(
                    typecheck::check_queries(&query("Game.id"), local).is_err(),
                    "different UUID identity brands must remain incompatible"
                );
                let unavailable =
                    parser::parse_query("query.pyre", "query NotLocal { principal { id } }")
                        .unwrap();
                assert!(typecheck::check_queries(&unavailable, local).is_err());
            }
            assert_eq!(
                schema::ensure_database_with_external_identities(
                    &conn,
                    &namespace.namespace,
                    &source,
                    &identities
                )
                .await?,
                schema::EnsureDatabaseOutcome::UpToDate
            );
        }
    }
    Ok(())
}

#[test]
fn inline_session_and_both_initializers_use_the_standalone_boundary() {
    let (mut database, _) = fixture();
    let main = &mut database.schemas[0];
    main.files[0]
        .definitions
        .push(ast::Definition::Session(main.session.clone().unwrap()));
    let context = typecheck::check_schema(&database).unwrap();
    let authored = pyre::generate::to_string::schema_to_string("", &database.schemas[0]);
    let mut files = Vec::new();
    pyre::generate::server::rust::generate_schema(
        &context,
        &database,
        std::path::Path::new("rust"),
        &mut files,
    );
    pyre::generate::typescript::targets::server::generate_schema(
        &context,
        &database,
        std::path::Path::new("typescript"),
        &mut files,
    );
    let rust = &files
        .iter()
        .find(|file| file.path.ends_with("rust/databases.rs"))
        .unwrap()
        .contents;
    let typescript = &files
        .iter()
        .find(|file| file.path.ends_with("typescript/databases.ts"))
        .unwrap()
        .contents;
    for schema in &database.schemas {
        let source = pyre::generate::to_string::standalone_schema_to_string(&context, schema);
        let (_, identities) = pyre::generate::to_string::standalone_schema(&context, schema);
        assert_eq!(source.matches("session {").count(), 1);
        let local = parse(&schema.namespace, &source);
        typecheck::check_schema_with_external_identities(
            &ast::Database {
                schemas: vec![local],
            },
            &identities,
        )
        .unwrap();
        assert!(rust.contains(&format!("{:?}", source)));
        assert!(typescript.contains(&serde_json::to_string(&source).unwrap()));
    }
    assert_eq!(
        authored,
        pyre::generate::to_string::schema_to_string("", &database.schemas[0])
    );
    assert!(authored.contains("Clocktower.Participant.id"));
}

#[tokio::test]
async fn migrations_upsert_missing_metadata_and_preserve_user_data(
) -> Result<(), Box<dyn std::error::Error>> {
    let (database, context) = fixture();
    let namespace = &database.schemas[2];
    let (source, identities) = pyre::generate::to_string::standalone_schema(&context, namespace);
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    schema::ensure_database_with_external_identities(&conn, "Clocktower", &source, &identities)
        .await?;
    let uuid = "01900000-0000-7000-8000-000000000001";
    seed::seed(
        &conn,
        json!({"participants": [{"id":uuid, "principalId":uuid, "name":"Player"}]}),
    )
    .await?;
    let incomplete: Vec<_> = identities
        .iter()
        .filter(|identity| identity.record != "Principal")
        .cloned()
        .collect();
    assert!(
        schema::ensure_database_with_external_identities(&conn, "Clocktower", &source, &incomplete)
            .await
            .is_err(),
        "incomplete authoritative definitions must not fall back to stored types"
    );
    assert_eq!(
        schema::load_schema_from_database(&conn)
            .await?
            .context()?
            .external_identities,
        identities
    );
    conn.execute(
        "delete from _pyre_external_identities where record_name = 'Principal'",
        (),
    )
    .await?;
    let error = schema::load_schema_from_database(&conn)
        .await
        .err()
        .expect("missing identity must fail loading");
    assert!(error.to_string().contains("pyre migrate"));
    assert_eq!(
        schema::ensure_database_with_external_identities(&conn, "Clocktower", &source, &identities)
            .await?,
        schema::EnsureDatabaseOutcome::Migrated
    );
    let loaded = schema::load_schema_from_database(&conn).await?;
    assert_eq!(loaded.context()?.external_identities, identities);
    assert_eq!(
        schema::ensure_database_with_external_identities(&conn, "Clocktower", &source, &identities)
            .await?,
        schema::EnsureDatabaseOutcome::UpToDate
    );
    let mut rows = conn.query("select name from participants", ()).await?;
    assert_eq!(rows.next().await?.unwrap().get::<String>(0)?, "Player");
    drop(rows);

    // Older databases may have neither the identity table nor namespace column.
    conn.execute("drop table _pyre_external_identities", ())
        .await?;
    conn.execute(
        "alter table _pyre_migrations drop column schema_namespace",
        (),
    )
    .await?;
    assert!(schema::load_schema_from_database(&conn).await.is_err());
    schema::ensure_database_with_external_identities(&conn, "Clocktower", &source, &identities)
        .await?;
    assert_eq!(
        schema::load_schema_from_database(&conn)
            .await?
            .schema()?
            .namespace,
        "Clocktower"
    );

    // Authoritative definitions still cannot silently reinterpret physical
    // TEXT references as integer IDs. Such a change needs a real migration.
    let (mut changed, _) = fixture();
    for definition in &mut changed.schemas[0].files[0].definitions {
        if let ast::Definition::Record { name, fields, .. } = definition {
            if name == "Principal" {
                for field in fields {
                    if let ast::Field::Column(column) = field {
                        if column.name == "id" {
                            column.type_ = ast::ColumnType::IdInt {
                                table: "Principal".into(),
                            };
                        }
                    }
                }
            }
        }
    }
    let changed_context = typecheck::check_schema(&changed).unwrap();
    let (changed_source, changed_identities) =
        pyre::generate::to_string::standalone_schema(&changed_context, &changed.schemas[2]);
    assert!(schema::ensure_database_with_external_identities(
        &conn,
        "Clocktower",
        &changed_source,
        &changed_identities
    )
    .await
    .is_err());
    assert_eq!(
        schema::load_schema_from_database(&conn)
            .await?
            .context()?
            .external_identities,
        identities
    );

    // Invalid stored references cannot be hidden by metadata maintenance, and
    // rollback preserves the existing rows when validation fails.
    conn.execute(
        "delete from _pyre_external_identities where record_name = 'Principal'",
        (),
    )
    .await?;
    conn.execute("update _pyre_migrations set schema = replace(schema, 'Main.Principal.id', 'Main.Missing.id')", ()).await?;
    assert!(schema::ensure_database_with_external_identities(
        &conn,
        "Clocktower",
        &source,
        &identities
    )
    .await
    .is_err());
    let mut rows = conn
        .query(
            "select count(*) from _pyre_external_identities where record_name = 'Principal'",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 0);
    Ok(())
}

#[test]
fn identity_metadata_cannot_fabricate_tables_or_hide_invalid_references() {
    let (database, context) = fixture();
    let (source, identities) =
        pyre::generate::to_string::standalone_schema(&context, &database.schemas[2]);
    assert!(!source.contains("@schemaSnapshot"));
    assert!(
        identities
            .iter()
            .any(|identity| identity.record == "Principal"
                && identity.kind == ast::IdentityKind::Uuid)
    );
    assert!(identities
        .iter()
        .any(|identity| identity.record == "User" && identity.kind == ast::IdentityKind::Int));
    assert!(source.contains("Clocktower.Participant.id?"));
    let mut missing = identities.clone();
    missing.retain(|identity| identity.record != "Principal");
    let mut duplicate = identities.clone();
    duplicate.push(identities[0].clone());
    let mut local = identities.clone();
    local.push(ast::ExternalIdentity {
        namespace: "Clocktower".into(),
        record: "Participant".into(),
        field: "id".into(),
        kind: ast::IdentityKind::Uuid,
    });
    let mut wrong_namespace = identities.clone();
    for identity in &mut wrong_namespace {
        if identity.record == "Principal" {
            identity.namespace = "Campaign".into();
        }
    }
    for invalid in [vec![], missing, duplicate, local, wrong_namespace] {
        let schema = parse("Clocktower", &source);
        assert!(
            typecheck::check_schema_with_external_identities(
                &ast::Database {
                    schemas: vec![schema]
                },
                &invalid
            )
            .is_err(),
            "{invalid:?}"
        );
    }
    let invalid = source.replace(
        "record Participant {",
        "record Participant {\n    principal @link(principalId, Main.Principal.id)",
    );
    assert!(typecheck::check_schema_with_external_identities(
        &ast::Database {
            schemas: vec![parse("Clocktower", &invalid)]
        },
        &identities
    )
    .is_err());
    let mut invalid_project = database;
    let session = invalid_project.schemas[0].session.as_mut().unwrap();
    let ast::Field::Column(column) = session
        .fields
        .iter_mut()
        .find(|field| matches!(field, ast::Field::Column(column) if column.name == "principalId"))
        .unwrap()
    else {
        unreachable!()
    };
    column.type_ = ast::ColumnType::from_str("Missing.id");
    assert!(
        typecheck::check_schema(&invalid_project).is_err(),
        "unresolved authored references must fail typechecking"
    );
}

#[tokio::test]
async fn metadata_upserts_preserve_rows_and_prune_removed_references(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    let identities = vec![ast::ExternalIdentity {
        namespace: "Auth".into(),
        record: "User".into(),
        field: "id".into(),
        kind: ast::IdentityKind::Uuid,
    }];
    let source = "session {\n    ownerId Auth.User.id?\n}\nrecord Note {\n    @public\n    id Id.Uuid @id\n}\n";
    schema::ensure_database_with_external_identities(&conn, "App", source, &identities).await?;
    let mut rows = conn
        .query("select rowid from _pyre_external_identities", ())
        .await?;
    let rowid: i64 = rows.next().await?.unwrap().get(0)?;
    drop(rows);
    conn.execute(
        "update _pyre_external_identities set identity_kind = 'Id.Int'",
        (),
    )
    .await?;
    conn.execute(
        "insert into _pyre_external_identities values ('App', 'Note', 'id', 'Id.Uuid')",
        (),
    )
    .await?;
    assert!(
        schema::load_schema_from_database(&conn).await.is_err(),
        "stored metadata cannot fabricate a local record identity"
    );
    schema::ensure_database_with_external_identities(&conn, "App", source, &identities).await?;
    let mut rows = conn
        .query(
            "select rowid, identity_kind from _pyre_external_identities",
            (),
        )
        .await?;
    let row = rows.next().await?.unwrap();
    assert_eq!(
        row.get::<i64>(0)?,
        rowid,
        "upsert must update the existing row"
    );
    assert_eq!(row.get::<String>(1)?, "Id.Uuid");
    drop(row);
    drop(rows);
    let changed = source.replace("    ownerId Auth.User.id?\n", "");
    schema::ensure_database_with_external_identities(&conn, "App", &changed, &identities).await?;
    assert!(schema::load_schema_from_database(&conn)
        .await?
        .context()?
        .external_identities
        .is_empty());
    let mut rows = conn
        .query("select count(*) from _pyre_external_identities", ())
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 0);
    assert_eq!(
        schema::ensure_database_with_external_identities(&conn, "App", &changed, &identities)
            .await?,
        schema::EnsureDatabaseOutcome::UpToDate
    );
    Ok(())
}

#[tokio::test]
async fn schema_loaders_respect_a_consistent_read_snapshot(
) -> Result<(), Box<dyn std::error::Error>> {
    let (database, context) = fixture();
    let (source, identities) =
        pyre::generate::to_string::standalone_schema(&context, &database.schemas[2]);
    let temp = tempfile::tempdir()?;
    let db = libsql::Builder::new_local(temp.path().join("snapshot.db"))
        .build()
        .await?;
    let reader = db.connect()?;
    reader.execute_batch("pragma journal_mode=WAL").await?;
    schema::ensure_database_with_external_identities(&reader, "Clocktower", &source, &identities)
        .await?;
    let tx = reader
        .transaction_with_behavior(libsql::TransactionBehavior::Deferred)
        .await?;
    assert_eq!(
        schema::load_schema_from_database(&tx)
            .await?
            .context()?
            .external_identities,
        identities
    );
    let changed = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("displayScreenId "))
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(changed, source.trim_end());
    let writer = db.connect()?;
    schema::ensure_database_with_external_identities(&writer, "Clocktower", &changed, &identities)
        .await?;
    assert_eq!(
        schema::load_schema_from_database(&tx)
            .await?
            .context()?
            .external_identities,
        identities
    );
    tx.commit().await?;
    let loaded = schema::load_schema_from_database(&reader).await?;
    assert!(!loaded
        .context()?
        .external_identities
        .iter()
        .any(|identity| identity.record == "ScreenPresentation"));
    Ok(())
}

#[tokio::test]
async fn unsupported_identity_kinds_are_not_silently_overwritten(
) -> Result<(), Box<dyn std::error::Error>> {
    let (database, context) = fixture();
    let (source, identities) =
        pyre::generate::to_string::standalone_schema(&context, &database.schemas[2]);
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    schema::ensure_database_with_external_identities(&conn, "Clocktower", &source, &identities)
        .await?;
    conn.execute_batch("pragma ignore_check_constraints=ON; update _pyre_external_identities set identity_kind = 'Id.Future' where record_name = 'Principal'; pragma ignore_check_constraints=OFF;").await?;
    let error = schema::load_schema_from_database(&conn)
        .await
        .err()
        .expect("unsupported kind must fail");
    assert!(error.to_string().contains("Id.Future"));
    assert!(schema::ensure_database_with_external_identities(
        &conn,
        "Clocktower",
        &source,
        &identities
    )
    .await
    .is_err());
    let mut rows = conn
        .query(
            "select identity_kind from _pyre_external_identities where record_name = 'Principal'",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<String>(0)?, "Id.Future");
    Ok(())
}
