use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pyre::ast;
use pyre::db::{diff, introspect, migrate};
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::parser;
use pyre::server::manifest::{Manifest, PyreSession, QueryManifest};
use pyre::server::query;
use pyre::typecheck;
use serde_json::json;
use std::collections::HashMap;

const SCHEMA: &str = r#"
session {
    userId Int
    role String
}

record Note {
    id Int @id
    ownerId Int
    body String
    attrs Json
    updatedAt Int
    @public
}
"#;

const SELECT_QUERY: &str = r#"
query GetNotes {
    note {
        id
        ownerId
        body
        attrs
        updatedAt
    }
}
"#;

struct BenchState {
    db: libsql::Database,
    manifest: Manifest,
    query_id: String,
    _temp_dir: tempfile::TempDir,
}

async fn setup_database(row_count: i64) -> BenchState {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let db_path = temp_dir.path().join("server-runtime-bench.db");
    let db = libsql::Builder::new_local(db_path.to_str().unwrap())
        .build()
        .await
        .unwrap();

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", SCHEMA, &mut schema).unwrap();
    let database = ast::Database {
        schemas: vec![schema.clone()],
    };
    let context = typecheck::check_schema(&database).unwrap();
    let introspection = introspect::Introspection {
        tables: vec![],
        migration_state: introspect::MigrationState::NoMigrationTable,
        schema: introspect::SchemaResult::Success {
            schema: ast::Schema::default(),
            context: typecheck::empty_context(),
        },
    };
    let db_diff = diff::diff(&context, &schema, &introspection);
    let mut migration_sql = diff::to_sql::to_sql(&db_diff);
    migration_sql.splice(0..0, migrate::internal_setup_sql());
    migration_sql.push(SqlAndParams::SqlWithParams {
        sql: migrate::INSERT_MIGRATION_SUCCESS_WITH_SCHEMA.to_string(),
        args: vec![
            "bench-init".to_string(),
            "".to_string(),
            pyre::generate::to_string::schema_to_string("", &schema),
        ],
    });

    let conn = db.connect().unwrap();
    for statement in migration_sql {
        match statement {
            SqlAndParams::Sql(sql) => {
                conn.execute(&sql, ()).await.unwrap();
            }
            SqlAndParams::SqlWithParams { sql, args } => {
                conn.execute(
                    &sql,
                    libsql::params_from_iter(args.into_iter().map(libsql::Value::Text)),
                )
                .await
                .unwrap();
            }
        }
    }

    for id in 1..=row_count {
        conn.execute(
            "insert into notes (id, ownerId, body, attrs, updatedAt) values (?, 1, ?, json(?), ?)",
            libsql::params_from_iter(vec![
                libsql::Value::Integer(id),
                libsql::Value::Text(format!("note-{id}")),
                libsql::Value::Text(format!(r#"{{"index":{id},"tag":"bench"}}"#)),
                libsql::Value::Integer(id),
            ]),
        )
        .await
        .unwrap();
    }

    let manifest = manifest_for(&context, SELECT_QUERY);
    let query_id = only_query(&manifest).id.clone();

    BenchState {
        db,
        manifest,
        query_id,
        _temp_dir: temp_dir,
    }
}

fn manifest_for(context: &typecheck::Context, query_source: &str) -> Manifest {
    let query_list = parser::parse_query("query.pyre", query_source).unwrap();
    let query_info = typecheck::check_queries(&query_list, context).unwrap();
    let mut files = Vec::new();
    pyre::generate::manifest::generate_queries(context, &query_list, &query_info, &mut files);
    let manifest_file = files
        .into_iter()
        .find(|file| file.path == std::path::Path::new("manifest.json"))
        .unwrap();

    serde_json::from_str(&manifest_file.contents).unwrap()
}

fn only_query(manifest: &Manifest) -> &QueryManifest {
    manifest.queries.values().next().unwrap()
}

fn session_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(setup_database(1));
    let input = json!({ "userId": 1, "role": "admin" });

    c.bench_function("server_runtime::session_new", |b| {
        b.iter(|| {
            PyreSession::new(
                black_box(input.clone()),
                black_box(&state.manifest.session_schema),
            )
            .unwrap();
        })
    });
}

fn query_runtime_vs_direct_sql_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(setup_database(1_000));
    let session = PyreSession::new(
        json!({ "userId": 1, "role": "admin" }),
        &state.manifest.session_schema,
    )
    .unwrap();
    let direct_sql = only_query(&state.manifest).sql[0].sql.clone();

    let mut group = c.benchmark_group("server_runtime::select_1000_rows");
    group.bench_function("query_run", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = state.db.connect().unwrap();
                let result = query::run(
                    black_box(&conn),
                    black_box(&state.manifest),
                    black_box(&state.query_id),
                    black_box(json!({})),
                    black_box(&session),
                )
                .await
                .unwrap();
                black_box(result.response);
            })
        })
    });
    group.bench_function("direct_sql_raw_fetch", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = state.db.connect().unwrap();
                let mut rows = conn.query(black_box(&direct_sql), ()).await.unwrap();
                while let Some(row) = rows.next().await.unwrap() {
                    let value = row.get::<String>(0).unwrap();
                    black_box(value);
                }
            })
        })
    });
    group.bench_function("direct_sql_fetch_and_parse_json", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = state.db.connect().unwrap();
                let mut rows = conn.query(black_box(&direct_sql), ()).await.unwrap();
                while let Some(row) = rows.next().await.unwrap() {
                    let value = row.get::<String>(0).unwrap();
                    let parsed = serde_json::from_str::<serde_json::Value>(&value).unwrap();
                    black_box(parsed);
                }
            })
        })
    });
    group.bench_function("direct_sql_fetch_and_format_response", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = state.db.connect().unwrap();
                let mut rows = conn.query(black_box(&direct_sql), ()).await.unwrap();
                let columns = (0..rows.column_count())
                    .map(|index| rows.column_name(index).unwrap_or("").to_string())
                    .collect::<Vec<_>>();
                let mut result_rows = Vec::new();

                while let Some(row) = rows.next().await.unwrap() {
                    let mut result_row = HashMap::new();
                    for (index, column) in columns.iter().enumerate() {
                        let value = row.get::<libsql::Value>(index as i32).unwrap();
                        result_row.insert(column.clone(), libsql_to_json(value));
                    }
                    result_rows.push(result_row);
                }

                let mut response = serde_json::Map::new();
                if let Some(column) = columns.first() {
                    for row in &result_rows {
                        if let Some(serde_json::Value::String(raw)) = row.get(column) {
                            let parsed = serde_json::from_str::<serde_json::Value>(raw).unwrap();
                            response.insert(
                                column.clone(),
                                if parsed.is_array() {
                                    parsed
                                } else {
                                    serde_json::Value::Array(vec![parsed])
                                },
                            );
                            break;
                        }
                    }
                }
                black_box(response);
            })
        })
    });
    group.finish();
}

fn libsql_to_json(value: libsql::Value) -> serde_json::Value {
    match value {
        libsql::Value::Null => serde_json::Value::Null,
        libsql::Value::Integer(value) => serde_json::Value::from(value),
        libsql::Value::Real(value) => serde_json::Value::from(value),
        libsql::Value::Text(value) => serde_json::Value::String(value),
        libsql::Value::Blob(value) => {
            serde_json::Value::Array(value.into_iter().map(serde_json::Value::from).collect())
        }
    }
}

criterion_group!(
    benches,
    session_benchmark,
    query_runtime_vs_direct_sql_benchmark
);
criterion_main!(benches);
