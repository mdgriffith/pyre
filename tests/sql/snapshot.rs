//! Golden-file tests for generated SQL.
//!
//! Every case here parses a schema and a query, runs the full SQL generation
//! pipeline, and compares the exact SQL text against a checked-in snapshot in
//! `tests/sql/snapshots/`. Any change to SQL generation shows up as a readable
//! diff in the failing test.
//!
//! To bless new output after an intentional change:
//!
//!     UPDATE_GOLDEN=1 cargo test snapshot
//!
//! then review the snapshot diff in git before committing.

use pyre::ast;
use pyre::parser;
use pyre::typecheck;

use crate::helpers::schema;

fn generate_sql(schema_source: &str, query_source: &str) -> String {
    let mut parsed_schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut parsed_schema)
        .unwrap_or_else(|e| panic!("schema failed to parse: {:?}", e));

    let database = ast::Database {
        schemas: vec![parsed_schema],
    };
    let context = typecheck::check_schema(&database).unwrap_or_else(|errors| {
        panic!(
            "schema failed to typecheck:\n{}",
            format_errors(schema_source, &errors)
        )
    });

    let query_list = parser::parse_query("query.pyre", query_source)
        .unwrap_or_else(|e| panic!("query failed to parse: {:?}", e));

    let all_query_info = typecheck::check_queries(&query_list, &context).unwrap_or_else(|errors| {
        panic!(
            "query failed to typecheck:\n{}",
            format_errors(query_source, &errors)
        )
    });

    let mut output = String::new();
    for query_def in &query_list.queries {
        let query = match query_def {
            ast::QueryDef::Query(q) => q,
            _ => continue,
        };
        let info = all_query_info
            .get(&query.name)
            .unwrap_or_else(|| panic!("no query info for {}", query.name));

        for top_level in &query.fields {
            let table_field = match top_level {
                ast::TopLevelQueryField::Field(f) => f,
                _ => continue,
            };
            let table = context
                .tables
                .get(&table_field.name)
                .unwrap_or_else(|| panic!("no table named {}", table_field.name));

            let prepared =
                pyre::generate::sql::to_string(&context, query, info, table, table_field);

            output.push_str(&format!("-- query: {}\n", query.name));
            let total = prepared.len();
            for (index, statement) in prepared.iter().enumerate() {
                let kind = if statement.include {
                    "returns rows"
                } else {
                    "setup"
                };
                output.push_str(&format!(
                    "-- statement {} of {} ({})\n",
                    index + 1,
                    total,
                    kind
                ));
                output.push_str(statement.sql.trim_end());
                output.push_str("\n\n");
            }
        }
    }
    output
}

fn format_errors(source: &str, errors: &[pyre::error::Error]) -> String {
    errors
        .iter()
        .map(|e| pyre::error::format_error(source, e, false))
        .collect::<Vec<_>>()
        .join("\n")
}

fn snapshot_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("sql")
        .join("snapshots")
}

fn check_snapshot(name: &str, schema_source: &str, query_source: &str) {
    let actual = generate_sql(schema_source, query_source);
    let path = snapshot_dir().join(format!("{}.sql", name));

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(snapshot_dir()).expect("failed to create snapshot dir");
        std::fs::write(&path, &actual).expect("failed to write snapshot");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "Missing snapshot {}.\nRun `UPDATE_GOLDEN=1 cargo test snapshot` to create it.",
            path.display()
        )
    });

    if normalize(&expected) != normalize(&actual) {
        panic!(
            "Generated SQL for '{}' changed.\n\n=== expected ({}) ===\n{}\n=== actual ===\n{}\n\nIf this change is intentional, run `UPDATE_GOLDEN=1 cargo test snapshot` and review the diff.",
            name,
            path.display(),
            expected,
            actual
        );
    }
}

fn normalize(s: &str) -> String {
    // Normalize line endings so snapshots are stable across platforms.
    s.replace("\r\n", "\n").trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Selects on the standard schema
// ---------------------------------------------------------------------------

#[test]
fn snapshot_select_basic() {
    check_snapshot(
        "select_basic",
        &schema::full_schema(),
        r#"
query GetUsers {
    user {
        id
        name
    }
}
"#,
    );
}

#[test]
fn snapshot_select_wildcard() {
    check_snapshot(
        "select_wildcard",
        &schema::full_schema(),
        r#"
query GetUsers {
    user {
        *
    }
}
"#,
    );
}

#[test]
fn snapshot_select_union_type() {
    check_snapshot(
        "select_union_type",
        &schema::full_schema(),
        r#"
query GetUsers {
    user {
        id
        status
    }
}
"#,
    );
}

#[test]
fn snapshot_select_nested_link() {
    check_snapshot(
        "select_nested_link",
        &schema::full_schema(),
        r#"
query GetUsersWithPosts {
    user {
        id
        name
        posts {
            id
            title
        }
    }
}
"#,
    );
}

#[test]
fn snapshot_select_nested_link_filtered() {
    check_snapshot(
        "select_nested_link_filtered",
        &schema::full_schema(),
        r#"
query GetUsersWithRecentPosts($minId: Int) {
    user {
        @where { id > 0 }
        @sort(name, Asc)
        id
        name
        posts {
            @where { id >= $minId }
            @sort(id, Desc)
            @limit(3)
            id
            title
        }
    }
}
"#,
    );
}

#[test]
fn snapshot_select_two_links_and_alias() {
    check_snapshot(
        "select_two_links_and_alias",
        &schema::full_schema(),
        r#"
query GetUserGraph {
    user {
        id
        writings: posts {
            id
            title
        }
        accounts {
            id
            name
        }
    }
}
"#,
    );
}

#[test]
fn snapshot_select_where_operators() {
    check_snapshot(
        "select_where_operators",
        &schema::full_schema(),
        r#"
query IntOperators {
    user {
        @where { id > 0 && id < 100 && id >= 2 && id <= 99 }
        id
    }
}
"#,
    );
}

#[test]
fn snapshot_select_where_or_grouping() {
    check_snapshot(
        "select_where_or_grouping",
        &schema::full_schema(),
        r#"
query OrGrouping {
    user {
        @where { name == "Alice" || name == "Bob" && id > 0 }
        id
        name
    }
}
"#,
    );
}

#[test]
fn snapshot_select_where_union_variant() {
    check_snapshot(
        "select_where_union_variant",
        &schema::full_schema(),
        r#"
query ActiveUsers {
    user {
        @where { status == Active }
        id
        status
    }
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Mutations on the standard schema
// ---------------------------------------------------------------------------

#[test]
fn snapshot_insert_basic() {
    check_snapshot(
        "insert_basic",
        &schema::full_schema(),
        r#"
insert CreateUser($name: String) {
    user {
        name = $name
        status = Active
    }
}
"#,
    );
}

#[test]
fn snapshot_insert_nested() {
    check_snapshot(
        "insert_nested",
        &schema::full_schema(),
        r#"
insert CreateUserWithPosts($name: String, $status: Status) {
    user {
        name = $name
        status = $status
        firstPost: posts {
            title = "First Post"
            content = "Hello"
        }
        secondPost: posts {
            title = "Second Post"
            content = "World"
        }
    }
}
"#,
    );
}

#[test]
fn snapshot_update_basic() {
    check_snapshot(
        "update_basic",
        &schema::full_schema(),
        r#"
update RenameUser($id: Int, $name: String) {
    user {
        @where { id == $id }
        name = $name
    }
}
"#,
    );
}

#[test]
fn snapshot_delete_basic() {
    check_snapshot(
        "delete_basic",
        &schema::full_schema(),
        r#"
delete DeleteUser($id: Int) {
    user {
        @where { id == $id }
    }
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Permissions / session predicates
// ---------------------------------------------------------------------------

#[test]
fn snapshot_perm_select_basic() {
    check_snapshot(
        "perm_select_basic",
        &super::permissions_schema(),
        r#"
query GetPosts {
    post {
        id
        title
        authorId
    }
}
"#,
    );
}

#[test]
fn snapshot_perm_select_nested_link() {
    // Nested link where the linked table also carries a permission predicate.
    // This is the shape that regressed in the aliased-CTE permission fix
    // (commit a26b8c7): the predicate has to reference the CTE alias, not the
    // raw table name.
    check_snapshot(
        "perm_select_nested_link",
        &super::permissions_schema(),
        r#"
query GetPostsWithComments {
    post {
        id
        title
        comments {
            @limit(5)
            id
            content
        }
    }
}
"#,
    );
}

#[test]
fn snapshot_perm_select_or_predicate() {
    check_snapshot(
        "perm_select_or_predicate",
        &super::permissions_schema(),
        r#"
query GetArticles {
    article {
        id
        title
        status
    }
}
"#,
    );
}

#[test]
fn snapshot_perm_insert() {
    check_snapshot(
        "perm_insert",
        &super::permissions_schema(),
        r#"
insert CreatePost($title: String, $content: String, $authorId: Int, $published: Bool) {
    post {
        title = $title
        content = $content
        authorId = $authorId
        published = $published
    }
}
"#,
    );
}

#[test]
fn snapshot_perm_update() {
    check_snapshot(
        "perm_update",
        &super::permissions_schema(),
        r#"
update UpdateArticle($id: Int, $title: String) {
    article {
        @where { id == $id }
        title = $title
    }
}
"#,
    );
}

#[test]
fn snapshot_perm_delete_session_and() {
    check_snapshot(
        "perm_delete_session_and",
        &super::permissions_schema(),
        r#"
delete DeleteDocument($id: Int) {
    document {
        @where { id == $id }
    }
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Json<T> columns
// ---------------------------------------------------------------------------

#[test]
fn snapshot_json_select() {
    check_snapshot(
        "json_select",
        &super::json_schema(),
        r#"
query GetEvents {
    event {
        id
        name
        payload
        tags
        counts
    }
}
"#,
    );
}

#[test]
fn snapshot_json_insert() {
    check_snapshot(
        "json_insert",
        &super::json_schema(),
        r#"
insert CreateEvent($name: String, $payload: Json<Lifecycle>, $tags: Json<List<String>>, $counts: Json<Dict<Int>>) {
    event {
        name = $name
        payload = $payload
        tags = $tags
        counts = $counts
    }
}
"#,
    );
}

#[test]
fn snapshot_json_update() {
    check_snapshot(
        "json_update",
        &super::json_schema(),
        r#"
update UpdateEvent($id: Int, $payload: Json<Lifecycle>) {
    event {
        @where { id == $id }
        payload = $payload
    }
}
"#,
    );
}
