use pyre::ast;
use pyre::error::ErrorType;
use pyre::{generate, parser, sync, typecheck};
use std::collections::HashMap;

fn parse_schema(source: &str) -> ast::Schema {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    schema
}

fn permission(record: &ast::RecordDetails, operation: ast::QueryOperation) -> ast::WhereArg {
    ast::get_permissions(record, &operation).unwrap()
}

fn workspace_record(schema: &ast::Schema) -> ast::RecordDetails {
    schema
        .files
        .iter()
        .flat_map(|file| &file.definitions)
        .find_map(|definition| match definition {
            ast::Definition::Record {
                name,
                fields,
                start,
                end,
                start_name,
                end_name,
            } if name == "Workspace" => Some(ast::RecordDetails {
                name: name.clone(),
                fields: fields.clone(),
                start: start.clone(),
                end: end.clone(),
                start_name: start_name.clone(),
                end_name: end_name.clone(),
            }),
            _ => None,
        })
        .unwrap()
}

const QUERY_ONLY_SCHEMA: &str = r#"
@syncable(false)

session {
    userId Int
}

type Role
    = Admin
    | Member

record User {
    @tablename("people")
    @public
    id Int @id
}

record Membership {
    @public
    id Int @id
    workspaceId Int
    userId Int
    role Role
    user @link(userId, User.id)
}

record Workspace {
    @allow(query, update, delete) {
        exists memberships {
            userId == Session.userId
            role == Admin || role == Member
        }
    }
    id Int @id
    name String
    memberships @link(Membership.workspaceId)
}
"#;

#[test]
fn parses_single_and_multiple_hop_exists_with_locations() {
    let schema = parse_schema(QUERY_ONLY_SCHEMA);
    let record = workspace_record(&schema);
    let ast::WhereArg::Exists(path, body) = permission(&record, ast::QueryOperation::Query) else {
        panic!("expected exists permission")
    };
    assert_eq!(
        path.iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["memberships"]
    );
    assert!(path[0].1.start.offset < path[0].1.end.offset);
    assert!(matches!(*body, ast::WhereArg::And(_)));

    let multi = QUERY_ONLY_SCHEMA.replace("exists memberships {", "exists memberships.user {");
    let schema = parse_schema(&multi);
    let record = workspace_record(&schema);
    let ast::WhereArg::Exists(path, _) = permission(&record, ast::QueryOperation::Query) else {
        panic!("expected exists permission")
    };
    assert_eq!(
        path.iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["memberships", "user"]
    );
}

#[test]
fn formats_exists_canonically_and_round_trips() {
    let schema = parse_schema(QUERY_ONLY_SCHEMA);
    let formatted = generate::to_string::schema_to_string(&schema.namespace, &schema);
    assert!(formatted.contains(
        "exists memberships {\n            userId == Session.userId\n            && role == Admin || role == Member\n         }"
    ), "{formatted}");
    parse_schema(&formatted);
}

fn check_schema(source: &str) -> Result<typecheck::Context, Vec<pyre::error::Error>> {
    let schema = parse_schema(source);
    typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
}

#[test]
fn validates_paths_body_and_sync_gate() {
    assert!(check_schema(QUERY_ONLY_SCHEMA).is_ok());

    for invalid in [
        QUERY_ONLY_SCHEMA.replace("exists memberships {", "exists missing {"),
        QUERY_ONLY_SCHEMA.replace("exists memberships {", "exists memberships.missing {"),
        QUERY_ONLY_SCHEMA.replace("exists memberships {", "exists id {"),
        QUERY_ONLY_SCHEMA.replace(
            "userId == Session.userId",
            "exists user { id == Session.userId }",
        ),
    ] {
        let errors = check_schema(&invalid).unwrap_err();
        assert!(errors.iter().any(|error| matches!(
            &error.error_type,
            ErrorType::InvalidRelationalPermission { .. }
        )));
    }

    let synced = QUERY_ONLY_SCHEMA.replace("@syncable(false)\n", "");
    let errors = check_schema(&synced).unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        &error.error_type,
        ErrorType::SyncedRelationalQueryPermission
    )));

    let star = synced.replace("@allow(query, update, delete)", "@allow(*)");
    let errors = check_schema(&star).unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        &error.error_type,
        ErrorType::SyncedRelationalQueryPermission
    )));

    let server_only = synced.replace("@allow(query, update, delete)", "@allow(update, delete)");
    assert!(check_schema(&server_only).is_ok());

    let insert = QUERY_ONLY_SCHEMA.replace("@allow(query, update, delete)", "@allow(insert)");
    let errors = check_schema(&insert).unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        &error.error_type,
        ErrorType::InvalidRelationalPermission { message }
            if message.contains("insert permissions")
    )));
}

fn generated_sql(schema_source: &str, query_source: &str) -> (String, bool) {
    let schema = parse_schema(schema_source);
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap();
    let queries = parser::parse_query("query.pyre", query_source).unwrap();
    let infos = typecheck::check_queries(&queries, &context).unwrap();
    let query = queries
        .queries
        .iter()
        .find_map(|query| match query {
            ast::QueryDef::Query(query) => Some(query),
            _ => None,
        })
        .unwrap();
    let field = query
        .fields
        .iter()
        .find_map(|field| match field {
            ast::TopLevelQueryField::Field(field) => Some(field),
            _ => None,
        })
        .unwrap();
    let table = context.tables.get(&field.name).unwrap();
    let info = infos.get(&query.name).unwrap();
    let sql = generate::sql::to_string(&context, query, info, table, field)
        .into_iter()
        .map(|statement| statement.sql)
        .collect::<Vec<_>>()
        .join("\n");
    (sql, info.variables.contains_key("Session.userId"))
}

#[test]
fn renders_correlated_exists_for_select_update_delete_and_custom_tables() {
    let (select, info) = generated_sql(QUERY_ONLY_SCHEMA, "query Get { workspace { id } }");
    assert!(
        select.contains("exists (select 1 from \"memberships\" as \"__pyre_exists_0\""),
        "{select}"
    );
    assert!(
        select.contains("\"__pyre_exists_0\".\"workspaceId\" = \"workspaces\".\"id\""),
        "{select}"
    );
    assert!(
        select.contains("\"__pyre_exists_0\".\"role\" = 'Admin'"),
        "{select}"
    );
    assert!(info);

    let multi = QUERY_ONLY_SCHEMA
        .replace("exists memberships {", "exists memberships.user {")
        .replace(
            "userId == Session.userId\n            role == Admin || role == Member",
            "id == Session.userId",
        );
    let (multi_sql, _) = generated_sql(&multi, "query Get { workspace { id } }");
    assert!(
        multi_sql.contains("join \"people\" as \"__pyre_exists_1\""),
        "{multi_sql}"
    );

    let (update, _) = generated_sql(
        QUERY_ONLY_SCHEMA,
        "update Rename($id: Int, $name: String) { workspace { @where { id == $id } name = $name } }",
    );
    assert!(
        update.contains("update workspaces") && update.contains("exists (select 1"),
        "{update}"
    );

    let (delete, _) = generated_sql(
        QUERY_ONLY_SCHEMA,
        "delete Remove($id: Int) { workspace { @where { id == $id } } }",
    );
    assert!(
        delete.contains("delete from workspaces") && delete.contains("exists (select 1"),
        "{delete}"
    );
}

#[test]
fn rejects_updates_that_change_the_authorizing_relationship() {
    let schema = parse_schema(QUERY_ONLY_SCHEMA);
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap();
    let queries = parser::parse_query(
        "query.pyre",
        "update Move($id: Int, $workspaceId: Int) { workspace { @where { id == $id } id = $workspaceId } }",
    )
    .unwrap();
    let errors = match typecheck::check_queries(&queries, &context) {
        Ok(_) => panic!("expected relationship-key update to fail"),
        Err(errors) => errors,
    };

    assert!(errors.iter().any(|error| matches!(
        &error.error_type,
        ErrorType::InvalidRelationalPermission { message }
            if message.contains("relationship key")
    )));
}

#[test]
fn renders_exists_for_terminal_linked_selection_with_the_child_alias() {
    let schema = QUERY_ONLY_SCHEMA.replace(
        "record Workspace {",
        "record Account {\n    @public\n    id Int @id\n    workspaceId Int\n    workspace @link(workspaceId, Workspace.id)\n}\n\nrecord Workspace {",
    );
    let (sql, _) = generated_sql(&schema, "query Get { account { id workspace { id } } }");

    assert!(
        sql.contains("\"__pyre_exists_0\".\"workspaceId\" = t.\"id\""),
        "{sql}"
    );
    assert!(sql.contains("and t.id in (select workspaceId"), "{sql}");
}

#[test]
fn permission_hash_includes_exists_path_and_body() {
    let schema = parse_schema(QUERY_ONLY_SCHEMA);
    let record = workspace_record(&schema);
    let original = permission(&record, ast::QueryOperation::Query);
    let changed_path = permission(
        &workspace_record(&parse_schema(
            &QUERY_ONLY_SCHEMA.replace("exists memberships {", "exists memberships.user {"),
        )),
        ast::QueryOperation::Query,
    );
    let changed_body = permission(
        &workspace_record(&parse_schema(
            &QUERY_ONLY_SCHEMA.replace("role == Admin", "role == Member"),
        )),
        ast::QueryOperation::Query,
    );
    let session = HashMap::new();
    let original_hash = sync::calculate_permission_hash(&Some(original), &session);
    assert_ne!(
        original_hash,
        sync::calculate_permission_hash(&Some(changed_path), &session)
    );
    assert_ne!(
        original_hash,
        sync::calculate_permission_hash(&Some(changed_body), &session)
    );
}

#[test]
fn generated_exists_alias_does_not_shadow_a_physical_table() {
    let schema = QUERY_ONLY_SCHEMA.replace(
        "record Workspace {",
        "record AliasCollision {\n    @tablename(\"__PYRE_EXISTS_0\")\n    @public\n    id Int @id\n}\n\nrecord Workspace {",
    );
    let (sql, _) = generated_sql(&schema, "query Get { workspace { id } }");

    assert!(sql.contains("as \"__pyre_exists_1\""), "{sql}");
    assert!(
        sql.contains("\"__pyre_exists_1\".\"workspaceId\" = \"workspaces\".\"id\""),
        "{sql}"
    );
}
