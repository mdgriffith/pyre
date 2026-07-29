use criterion::{black_box, criterion_group, criterion_main, Criterion};
use libsql::Database;
use pyre::ast;
use pyre::db::diff;
use pyre::db::introspect;
use pyre::db::migrate;
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::parser;
use pyre::seed;
use pyre::typecheck;
use serde_json::{json, Value};
use std::collections::HashMap;
use tempfile;

const TEST_SCHEMA: &str = r#"
record User {
    accounts      @link(Account.userId)
    posts         @link(Post.authorUserId)
    databaseUsers @link(DatabaseUser.userId)

    // Fields
    id        Int     @id
    name      String?
    status    Status
    createdAt DateTime @default(now)
    @public
}

record DatabaseUser {
    id         Int   @id
    databaseId String

    userId Int
    users  @link(userId, User.id)
    @public
}

record Account {
    @tablename("accounts")
    users @link(userId, User.id)

    id     Int   @id
    userId Int
    name   String
    status String
    @public
}

record Post {
    users @link(authorUserId, User.id)

    id           Int     @id
    createdAt    DateTime @default(now)
    authorUserId Int
    title        String
    content      String
    status       Status
    @public
}

type Status
   = Active
   | Inactive
   | Special {
        reason String
     }
   | Special2 {
        reason String
        error  String
     }
"#;

const SIMPLE_QUERY: &str = r#"
query GetUsers {
    user {
        id
        name
        status
    }
}
"#;

const NESTED_QUERY: &str = r#"
query GetUsersWithPosts {
    user {
        id
        name
        status
        posts {
            id
            title
            content
            status
        }
    }
}
"#;

const DEEPLY_NESTED_QUERY: &str = r#"
query GetUsersWithPostsAndAccounts {
    user {
        id
        name
        status
        posts {
            id
            title
            content
            status
        }
        accounts {
            id
            name
            status
        }
    }
}
"#;

// Setup: Parse and typecheck schema once
fn setup_schema() -> (ast::Schema, typecheck::Context) {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", TEST_SCHEMA, &mut schema).unwrap();
    let database = ast::Database {
        schemas: vec![schema.clone()],
    };
    let context = typecheck::check_schema(&database).unwrap();
    (schema, context)
}

// Benchmark query parsing
fn query_parse_benchmark(c: &mut Criterion) {
    c.bench_function("query::parse", |b| {
        b.iter(|| {
            parser::parse_query("query.pyre", black_box(SIMPLE_QUERY)).unwrap();
        })
    });
}

// Benchmark query typechecking
fn query_typecheck_benchmark(c: &mut Criterion) {
    let (_schema, _context) = setup_schema();
    let query_list = parser::parse_query("query.pyre", SIMPLE_QUERY).unwrap();

    c.bench_function("query::typecheck", |b| {
        b.iter(|| {
            let (_schema, mut context) = setup_schema();
            typecheck::check_queries(black_box(&query_list), &mut context).unwrap();
        })
    });
}

// Benchmark SQL generation
fn query_sql_generation_benchmark(c: &mut Criterion) {
    let (_schema, _context) = setup_schema();
    let query_list = parser::parse_query("query.pyre", SIMPLE_QUERY).unwrap();

    c.bench_function("query::generate_sql", |b| {
        b.iter(|| {
            let (_schema, mut context) = setup_schema();
            let query_info_map = typecheck::check_queries(&query_list, &mut context).unwrap();

            let query = query_list
                .queries
                .iter()
                .find_map(|q| match q {
                    ast::QueryDef::Query(q) => Some(q),
                    _ => None,
                })
                .unwrap();

            let table_field = query
                .fields
                .iter()
                .find_map(|f| match f {
                    ast::TopLevelQueryField::Field(qf) => Some(qf),
                    _ => None,
                })
                .unwrap();

            let table = context.tables.get(&table_field.name).unwrap();
            let query_info = query_info_map.get(&query.name).unwrap();

            pyre::generate::sql::to_string(
                black_box(&context),
                black_box(query),
                black_box(query_info),
                black_box(table),
                black_box(table_field),
            );
        })
    });
}

// Benchmark nested query SQL generation
fn query_nested_sql_generation_benchmark(c: &mut Criterion) {
    let (_schema, _context) = setup_schema();
    let query_list = parser::parse_query("query.pyre", NESTED_QUERY).unwrap();

    c.bench_function("query::generate_sql_nested", |b| {
        b.iter(|| {
            let (_schema, mut context) = setup_schema();
            let query_info_map = typecheck::check_queries(&query_list, &mut context).unwrap();

            let query = query_list
                .queries
                .iter()
                .find_map(|q| match q {
                    ast::QueryDef::Query(q) => Some(q),
                    _ => None,
                })
                .unwrap();

            let table_field = query
                .fields
                .iter()
                .find_map(|f| match f {
                    ast::TopLevelQueryField::Field(qf) => Some(qf),
                    _ => None,
                })
                .unwrap();

            let table = context.tables.get(&table_field.name).unwrap();
            let query_info = query_info_map.get(&query.name).unwrap();

            pyre::generate::sql::to_string(
                black_box(&context),
                black_box(query),
                black_box(query_info),
                black_box(table),
                black_box(table_field),
            );
        })
    });
}

// Setup in-memory database with seeded data
// Use a file-based database to ensure persistence across connections
async fn setup_database() -> (Database, typecheck::Context, tempfile::TempDir) {
    // Create a temporary file-based database (like TestDatabase does)
    let temp_dir = tempfile::TempDir::new().unwrap();
    let db_path = temp_dir.path().join("bench.db");
    let db_path_str = db_path.to_str().unwrap();

    let db = libsql::Builder::new_local(db_path_str)
        .build()
        .await
        .unwrap();

    // Parse and typecheck schema
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", TEST_SCHEMA, &mut schema).unwrap();
    let database = ast::Database {
        schemas: vec![schema.clone()],
    };
    let context = typecheck::check_schema(&database).unwrap();

    // Create empty introspection
    let introspection = introspect::Introspection {
        tables: vec![],
        migration_state: introspect::MigrationState::NoMigrationTable,
        schema: introspect::SchemaResult::Success {
            schema: ast::Schema::default(),
            context: typecheck::empty_context(),
        },
    };

    // Generate migration SQL
    let db_diff = diff::diff(&context, &schema, &introspection);
    let mut migration_sql = diff::to_sql::to_sql(&db_diff);

    // Add migration tables
    migration_sql.splice(0..0, migrate::internal_setup_sql());

    // Add schema insertion
    let schema_string = pyre::generate::to_string::schema_to_string("", &schema);
    migration_sql.push(SqlAndParams::SqlWithParams {
        sql: migrate::INSERT_MIGRATION_SUCCESS_WITH_SCHEMA.to_string(),
        args: vec!["bench-init".to_string(), "".to_string(), schema_string],
    });

    // Execute migration
    let conn = db.connect().unwrap();
    for stmt in &migration_sql {
        match stmt {
            SqlAndParams::Sql(s) => {
                conn.execute(s, ()).await.unwrap();
            }
            SqlAndParams::SqlWithParams { sql: s, args } => {
                let values: Vec<libsql::Value> = args
                    .iter()
                    .map(|s| libsql::Value::Text(s.clone()))
                    .collect();
                conn.execute(s, libsql::params_from_iter(values))
                    .await
                    .unwrap();
            }
        }
    }

    // Seed database with moderate amount of data
    let seed_options = seed::Options {
        seed: Some(12345),
        default_rows_per_table: 1000,
        table_rows: HashMap::new(),
        foreign_key_ratios: HashMap::new(),
        default_foreign_key_ratio: 5.0, // 5 posts per user, 5 accounts per user
    };

    let seed_operations = seed::seed_database(&schema, &context, Some(seed_options));
    for op in &seed_operations {
        conn.execute(&op.sql, ()).await.unwrap();
    }

    (db, context, temp_dir)
}

// Benchmark SQL execution with JSON processing (simple query)
fn query_execution_simple_json_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (db, mut context, _temp_dir) = rt.block_on(setup_database());

    let query_list = parser::parse_query("query.pyre", SIMPLE_QUERY).unwrap();
    let query_info_map = typecheck::check_queries(&query_list, &mut context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(qf) => Some(qf),
            _ => None,
        })
        .unwrap();

    let table = context.tables.get(&table_field.name).unwrap();
    let query_info = query_info_map.get(&query.name).unwrap();

    let sql_statements =
        pyre::generate::sql::to_string(&context, query, query_info, table, table_field);
    let sql = sql_statements[0].sql.clone();

    c.bench_function("query::execute_sql_simple_json", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = db.connect().unwrap();
                let mut rows = conn.query(&sql, ()).await.unwrap();
                while let Some(_row) = rows.next().await.unwrap() {
                    // Consume all rows
                }
            });
        });
    });
}

// Benchmark SQL execution with JSON processing (nested query)
fn query_execution_nested_json_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (db, mut context, _temp_dir) = rt.block_on(setup_database());

    let query_list = parser::parse_query("query.pyre", NESTED_QUERY).unwrap();
    let query_info_map = typecheck::check_queries(&query_list, &mut context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(qf) => Some(qf),
            _ => None,
        })
        .unwrap();

    let table = context.tables.get(&table_field.name).unwrap();
    let query_info = query_info_map.get(&query.name).unwrap();

    let sql_statements =
        pyre::generate::sql::to_string(&context, query, query_info, table, table_field);
    let sql = sql_statements[0].sql.clone();

    c.bench_function("query::execute_sql_nested_json_from_database", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = db.connect().unwrap();
                let mut rows = conn.query(&sql, ()).await.unwrap();
                while let Some(_row) = rows.next().await.unwrap() {
                    // Consume all rows
                }
            });
        });
    });
}

// Benchmark SQL execution with equivalent JOIN query (nested query)
// This simulates what a traditional join-based query would look like
fn query_execution_nested_join_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (db, _context, _temp_dir) = rt.block_on(setup_database());

    // Equivalent JOIN-based SQL (without JSON processing)
    // This creates duplication but avoids JSON processing overhead
    // Note: Column names are camelCase and quoted in SQLite
    let join_sql = r#"
        SELECT
            "u"."id",
            "u"."name",
            "u"."status",
            "p"."id" AS "post_id",
            "p"."title" AS "post_title",
            "p"."content" AS "post_content",
            "p"."status" AS "post_status"
        FROM "users" "u"
        LEFT JOIN "posts" "p" ON "u"."id" = "p"."authorUserId"
        ORDER BY "u"."id", "p"."id"
    "#;

    c.bench_function("query::execute_sql_nested_join", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = db.connect().unwrap();
                let mut rows = conn.query(join_sql, ()).await.unwrap();
                while let Some(_row) = rows.next().await.unwrap() {
                    // Consume all rows (with duplication)
                }
            });
        });
    });
}

// Benchmark SQL execution with JOIN query + application-side JSON composition
// This simulates composing nested JSON in application code instead of SQL
fn query_execution_nested_join_compose_json_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (db, _context, _temp_dir) = rt.block_on(setup_database());

    // Equivalent JOIN-based SQL (without JSON processing)
    let join_sql = r#"
        SELECT
            "u"."id",
            "u"."name",
            "u"."status",
            "p"."id" AS "post_id",
            "p"."title" AS "post_title",
            "p"."content" AS "post_content",
            "p"."status" AS "post_status"
        FROM "users" "u"
        LEFT JOIN "posts" "p" ON "u"."id" = "p"."authorUserId"
        ORDER BY "u"."id", "p"."id"
    "#;

    c.bench_function(
        "query::execute_sql_nested_join_application_composition_of_json",
        |b| {
            b.iter(|| {
                rt.block_on(async {
                    let conn = db.connect().unwrap();
                    let mut rows = conn.query(join_sql, ()).await.unwrap();

                    // Compose nested JSON in application code
                    let mut users: HashMap<i64, Value> = HashMap::new();

                    while let Some(row) = rows.next().await.unwrap() {
                        let user_id: i64 = row.get(0).unwrap();

                        // Get or create user object
                        let user = users.entry(user_id).or_insert_with(|| {
                            json!({
                                "id": user_id,
                                "name": row.get::<Option<String>>(1).unwrap(),
                                "status": row.get::<String>(2).unwrap(),
                                "posts": json!([])
                            })
                        });

                        // Add post if present
                        if let Ok(Some(post_id)) = row.get::<Option<i64>>(3) {
                            let posts = user.get_mut("posts").unwrap().as_array_mut().unwrap();
                            posts.push(json!({
                                "id": post_id,
                                "title": row.get::<Option<String>>(4).unwrap(),
                                "content": row.get::<Option<String>>(5).unwrap(),
                                "status": row.get::<Option<String>>(6).unwrap(),
                            }));
                        }
                    }

                    // Convert to final JSON structure
                    let result = json!({
                        "user": users.values().collect::<Vec<_>>()
                    });

                    // Serialize to JSON string (simulating what would be sent over network)
                    black_box(serde_json::to_string(&result).unwrap());
                });
            });
        },
    );
}

// Benchmark SQL execution with JSON processing (deeply nested query)
fn query_execution_deeply_nested_json_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (db, mut context, _temp_dir) = rt.block_on(setup_database());

    let query_list = parser::parse_query("query.pyre", DEEPLY_NESTED_QUERY).unwrap();
    let query_info_map = typecheck::check_queries(&query_list, &mut context).unwrap();

    let query = query_list
        .queries
        .iter()
        .find_map(|q| match q {
            ast::QueryDef::Query(q) => Some(q),
            _ => None,
        })
        .unwrap();

    let table_field = query
        .fields
        .iter()
        .find_map(|f| match f {
            ast::TopLevelQueryField::Field(qf) => Some(qf),
            _ => None,
        })
        .unwrap();

    let table = context.tables.get(&table_field.name).unwrap();
    let query_info = query_info_map.get(&query.name).unwrap();

    let sql_statements =
        pyre::generate::sql::to_string(&context, query, query_info, table, table_field);
    let sql = sql_statements[0].sql.clone();

    c.bench_function("query::execute_sql_deeply_nested_json", |b| {
        b.iter(|| {
            rt.block_on(async {
                let conn = db.connect().unwrap();
                let mut rows = conn.query(&sql, ()).await.unwrap();
                while let Some(_row) = rows.next().await.unwrap() {
                    // Consume all rows
                }
            });
        });
    });
}

// NOTE: These benchmarks hand forever, even with the LIMIT added.  THis is why they're commented out.
// // Benchmark SQL execution with equivalent JOIN query (deeply nested query)
// // This simulates what a traditional join-based query would look like
// fn query_execution_deeply_nested_join_benchmark(c: &mut Criterion) {
//     let rt = tokio::runtime::Runtime::new().unwrap();
//     let (db, _context, _temp_dir) = rt.block_on(setup_database());

//     // Equivalent JOIN-based SQL (without JSON processing)
//     // This creates duplication but avoids JSON processing overhead
//     // Note: Column names are camelCase and quoted in SQLite
//     // LIMIT added to prevent Cartesian product explosion (1000 users × 5 posts × 5 accounts = 25k rows)
//     let join_sql = r#"
//         SELECT
//             "u"."id",
//             "u"."name",
//             "u"."status",
//             "p"."id" AS "post_id",
//             "p"."title" AS "post_title",
//             "p"."content" AS "post_content",
//             "p"."status" AS "post_status",
//             "a"."id" AS "account_id",
//             "a"."name" AS "account_name",
//             "a"."status" AS "account_status"
//         FROM "users" "u"
//         LEFT JOIN "posts" "p" ON "u"."id" = "p"."authorUserId"
//         LEFT JOIN "accounts" "a" ON "u"."id" = "a"."userId"
//         ORDER BY "u"."id", "p"."id", "a"."id"
//         LIMIT 10000
//     "#;

//     c.bench_function("query::execute_sql_deeply_nested_join", |b| {
//         b.iter(|| {
//             rt.block_on(async {
//                 let conn = db.connect().unwrap();
//                 let mut rows = conn.query(join_sql, ()).await.unwrap();
//                 while let Some(_row) = rows.next().await.unwrap() {
//                     // Consume all rows (with duplication)
//                 }
//             });
//         });
//     });
// }

// // Benchmark SQL execution with JOIN query + application-side JSON composition (deeply nested)
// // This simulates composing nested JSON in application code instead of SQL
// fn query_execution_deeply_nested_join_compose_json_benchmark(c: &mut Criterion) {
//     let rt = tokio::runtime::Runtime::new().unwrap();
//     let (db, _context, _temp_dir) = rt.block_on(setup_database());

//     // Equivalent JOIN-based SQL (without JSON processing)
//     // LIMIT added to prevent Cartesian product explosion (1000 users × 5 posts × 5 accounts = 25k rows)
// let join_sql = r#"
//     SELECT
//         "u"."id",
//         "u"."name",
//         "u"."status",
//         "p"."id" AS "post_id",
//         "p"."title" AS "post_title",
//         "p"."content" AS "post_content",
//         "p"."status" AS "post_status",
//         "a"."id" AS "account_id",
//         "a"."name" AS "account_name",
//         "a"."status" AS "account_status"
//     FROM "users" "u"
//     LEFT JOIN "posts" "p" ON "u"."id" = "p"."authorUserId"
//     LEFT JOIN "accounts" "a" ON "u"."id" = "a"."userId"
//     ORDER BY "u"."id", "p"."id", "a"."id"
//     LIMIT 10000
// "#;

//     c.bench_function(
//         "query::execute_sql_deeply_nested_join_application_composition_of_json",
//         |b| {
//             b.iter(|| {
//                 rt.block_on(async {
//                     let conn = db.connect().unwrap();
//                     let mut rows = conn.query(join_sql, ()).await.unwrap();

//                     // Compose nested JSON in application code
//                     let mut users: HashMap<i64, Value> = HashMap::new();

//                     while let Some(row) = rows.next().await.unwrap() {
//                         let user_id: i64 = row.get(0).unwrap();

//                         // Get or create user object
//                         let user = users.entry(user_id).or_insert_with(|| {
//                             json!({
//                                 "id": user_id,
//                                 "name": row.get::<Option<String>>(1).unwrap(),
//                                 "status": row.get::<String>(2).unwrap(),
//                                 "posts": json!([]),
//                                 "accounts": json!([])
//                             })
//                         });

//                         // Add post if present
//                         if let Ok(Some(post_id)) = row.get::<Option<i64>>(3) {
//                             let posts = user.get_mut("posts").unwrap().as_array_mut().unwrap();
//                             // Check if this post was already added
//                             let post_exists = posts.iter().any(|p| {
//                                 p.get("id")
//                                     .and_then(|id| id.as_i64())
//                                     .map(|id| id == post_id)
//                                     .unwrap_or(false)
//                             });
//                             if !post_exists {
//                                 posts.push(json!({
//                                     "id": post_id,
//                                     "title": row.get::<Option<String>>(4).unwrap(),
//                                     "content": row.get::<Option<String>>(5).unwrap(),
//                                     "status": row.get::<Option<String>>(6).unwrap(),
//                                 }));
//                             }
//                         }

//                         // Add account if present
//                         if let Ok(Some(account_id)) = row.get::<Option<i64>>(7) {
//                             let accounts =
//                                 user.get_mut("accounts").unwrap().as_array_mut().unwrap();
//                             // Check if this account was already added
//                             let account_exists = accounts.iter().any(|a| {
//                                 a.get("id")
//                                     .and_then(|id| id.as_i64())
//                                     .map(|id| id == account_id)
//                                     .unwrap_or(false)
//                             });
//                             if !account_exists {
//                                 accounts.push(json!({
//                                     "id": account_id,
//                                     "name": row.get::<Option<String>>(8).unwrap(),
//                                     "status": row.get::<Option<String>>(9).unwrap(),
//                                 }));
//                             }
//                         }
//                     }

//                     // Convert to final JSON structure
//                     let result = json!({
//                         "user": users.values().collect::<Vec<_>>()
//                     });

//                     // Serialize to JSON string (simulating what would be sent over network)
//                     black_box(serde_json::to_string(&result).unwrap());
//                 });
//             });
//         },
//     );
// }

criterion_group!(
    benches,
    query_parse_benchmark,
    query_typecheck_benchmark,
    query_sql_generation_benchmark,
    query_nested_sql_generation_benchmark,
    query_execution_simple_json_benchmark,
    query_execution_nested_json_benchmark,
    query_execution_nested_join_benchmark,
    query_execution_nested_join_compose_json_benchmark,
    query_execution_deeply_nested_json_benchmark,
    // query_execution_deeply_nested_join_benchmark,
    // query_execution_deeply_nested_join_compose_json_benchmark,
);
criterion_main!(benches);
