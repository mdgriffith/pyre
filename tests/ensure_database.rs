use pyre::server::schema::{ensure_database, EnsureDatabaseError, EnsureDatabaseOutcome};

const SCHEMA: &str = r#"record Note {
    id Int @id
    body String
    @public
}
"#;

async fn connection() -> Result<(tempfile::TempDir, libsql::Connection), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("database.db");
    let db = libsql::Builder::new_local(path.to_string_lossy().as_ref())
        .build()
        .await?;
    let conn = db.connect()?;
    Ok((temp, conn))
}

#[tokio::test]
async fn ensure_database_creates_and_reuses_a_database() -> Result<(), Box<dyn std::error::Error>> {
    let (_temp, conn) = connection().await?;

    assert_eq!(
        ensure_database(&conn, "Campaign", SCHEMA).await?,
        EnsureDatabaseOutcome::Created
    );
    assert_eq!(
        ensure_database(&conn, "Campaign", SCHEMA).await?,
        EnsureDatabaseOutcome::UpToDate
    );

    let table_count: i64 = conn
        .query(
            "select count(*) from sqlite_master where name = 'notes'",
            (),
        )
        .await?
        .next()
        .await?
        .expect("table count row")
        .get(0)?;
    let migration_count: i64 = conn
        .query("select count(*) from _pyre_migrations", ())
        .await?
        .next()
        .await?
        .expect("migration count row")
        .get(0)?;

    assert_eq!(table_count, 1);
    assert_eq!(migration_count, 1);
    Ok(())
}

#[tokio::test]
async fn ensure_database_records_schema_only_changes() -> Result<(), Box<dyn std::error::Error>> {
    let (_temp, conn) = connection().await?;
    ensure_database(&conn, "Campaign", SCHEMA).await?;
    let changed = format!("// generated schema revision\n{SCHEMA}");

    assert_eq!(
        ensure_database(&conn, "Campaign", &changed).await?,
        EnsureDatabaseOutcome::Migrated
    );

    let schema: String = conn
        .query(
            "select schema from _pyre_migrations order by id desc limit 1",
            (),
        )
        .await?
        .next()
        .await?
        .expect("schema row")
        .get(0)?;
    assert_eq!(schema, changed);
    Ok(())
}

#[tokio::test]
async fn ensure_database_rejects_an_unmanaged_database() -> Result<(), Box<dyn std::error::Error>> {
    let (_temp, conn) = connection().await?;
    conn.execute("create table legacy (id integer primary key)", ())
        .await?;

    let error = ensure_database(&conn, "Campaign", SCHEMA)
        .await
        .expect_err("unmanaged database should be rejected");
    assert!(matches!(error, EnsureDatabaseError::UnmanagedDatabase));
    Ok(())
}

#[tokio::test]
async fn ensure_database_rejects_the_wrong_schema_family() -> Result<(), Box<dyn std::error::Error>>
{
    let (_temp, conn) = connection().await?;
    ensure_database(&conn, "Campaign", SCHEMA).await?;
    let clocktower_schema = r#"record Player {
    id Int @id
    name String
    @public
}
"#;

    let error = ensure_database(&conn, "Clocktower", clocktower_schema)
        .await
        .expect_err("a different schema family should be rejected");
    assert!(matches!(error, EnsureDatabaseError::Migration(_)));
    Ok(())
}
