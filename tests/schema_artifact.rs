use pyre::{ast, generate, parser, typecheck};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

const SCHEMA: &str = r#"
session {
    userId Int
    minimum Int
    serverOnly Int
    alternate Int
    unused Int
}
record Item {
    id Int @id
    ownerId Int
    score Int
    @allow(*) { ownerId == Session.serverOnly }
}
record Unqueried {
    id Int @id
    tenantId Int
    @allow(*) { tenantId == Session.serverOnly }
}
"#;

const LOCAL: &str = r#"
query Local {
    item {
        @where { score >= Session.minimum && ownerId == Session.userId && Session.userId > 0 }
        id
    }
}
"#;
const PERMISSION_ONLY: &str = "query PermissionOnly { item { id } }";
const INSERT: &str = "insert CreateItem { item { ownerId = Session.userId score = 1 } }";

fn artifact(
    schema_source: &str,
    query_source: &str,
    path: &str,
) -> (Value, BTreeMap<String, String>) {
    // Fresh parsing/typechecking also creates independently seeded HashMaps.
    let mut schema = ast::Schema::default();
    parser::run(&format!("{path}/schema.pyre"), schema_source, &mut schema).expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let queries =
        parser::parse_query(&format!("{path}/queries.pyre"), query_source).expect("queries parse");
    let info = typecheck::check_queries(&queries, &context).expect("queries typecheck");
    let mut files = Vec::new();
    generate::manifest::generate_queries(&context, &queries, &info, &mut files);
    generate::typescript::core::generate_queries(
        &context,
        &info,
        &queries,
        Path::new("typescript/core"),
        &mut files,
    );
    let manifest: Value = serde_json::from_str(
        &files
            .iter()
            .find(|file| file.path == Path::new("manifest.json"))
            .expect("manifest generated")
            .contents,
    )
    .expect("manifest is JSON");
    let browser = files
        .iter()
        .find(|file| file.path == Path::new("typescript/core/artifact.ts"))
        .expect("browser artifact generated");
    let browser: Value = serde_json::from_str(
        browser
            .contents
            .split("export const schemaArtifact = ")
            .nth(1)
            .unwrap()
            .trim_end()
            .strip_suffix(" as const;")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(browser["schemaId"], manifest["schema_id"]);
    let required: std::collections::BTreeSet<_> = manifest["queries"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|query| query["required_client_session_fields"].as_array().unwrap())
        .map(|field| field.as_str().unwrap())
        .collect();
    assert_eq!(browser["requiredClientSessionFields"], json!(required));
    let metadata = queries
        .queries
        .iter()
        .filter_map(|definition| {
            let ast::QueryDef::Query(query) = definition else {
                return None;
            };
            let name = format!("{}{}", query.name[..1].to_lowercase(), &query.name[1..]);
            let file = files
                .iter()
                .find(|file| file.path.ends_with(format!("queries/metadata/{name}.ts")))
                .expect("TypeScript metadata generated");
            Some((query.interface_hash.clone(), file.contents.clone()))
        })
        .collect();
    (manifest, metadata)
}

#[test]
fn fingerprint_is_stable_across_paths_hash_seeds_and_query_order() {
    let queries = format!("{LOCAL}\n{PERMISSION_ONLY}\n{INSERT}");
    let (baseline, _) = artifact(SCHEMA, &queries, "original");
    let id = baseline["schema_id"]
        .as_str()
        .expect("schema_id is a string");
    assert_eq!(id.len(), 64);
    assert!(id
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    assert_eq!(baseline["queries"].as_object().unwrap().len(), 3);
    for run in 0..12 {
        let reordered = format!("{INSERT}\n{PERMISSION_ONLY}\n{LOCAL}");
        let source = if run % 2 == 0 { &queries } else { &reordered };
        let (actual, _) = artifact(SCHEMA, source, &format!("different/source/{run}"));
        assert_eq!(actual, baseline, "generation {run}");
    }
}

#[test]
fn fingerprint_changes_with_schema_permissions_session_sync_and_local_predicates() {
    let (baseline, _) = artifact(SCHEMA, LOCAL, "baseline");
    for (label, schema, query) in [
        (
            "unselected column",
            SCHEMA.replace("score Int", "score Int\n    title String"),
            LOCAL.to_string(),
        ),
        (
            "permission-only session field",
            SCHEMA.replace(
                "ownerId == Session.serverOnly",
                "ownerId == Session.alternate",
            ),
            LOCAL.to_string(),
        ),
        (
            "queried table permission predicate",
            SCHEMA.replace(
                "ownerId == Session.serverOnly",
                "ownerId != Session.serverOnly",
            ),
            LOCAL.to_string(),
        ),
        (
            "unqueried table permission predicate",
            SCHEMA.replace(
                "tenantId == Session.serverOnly",
                "tenantId != Session.serverOnly",
            ),
            LOCAL.to_string(),
        ),
        (
            "unqueried table permission field",
            SCHEMA.replace(
                "tenantId == Session.serverOnly",
                "tenantId == Session.alternate",
            ),
            LOCAL.to_string(),
        ),
        (
            "unused session type",
            SCHEMA.replace("unused Int", "unused String"),
            LOCAL.to_string(),
        ),
        (
            "namespace sync mode",
            format!("@syncable(false)\n{SCHEMA}"),
            LOCAL.to_string(),
        ),
        (
            "local predicate",
            SCHEMA.to_string(),
            LOCAL.replace("score >=", "score >"),
        ),
    ] {
        let (changed, _) = artifact(&schema, &query, "baseline");
        assert_ne!(changed["schema_id"], baseline["schema_id"], "{label}");
        if label == "local predicate" {
            let original = baseline["queries"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            let updated = changed["queries"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_ne!(original["local_query_plan"], updated["local_query_plan"]);
        }
    }
}

#[test]
fn fingerprint_preserves_inline_union_directives() {
    let schema = format!(
        "type Payload = Value {{ count Int @default(1) }}\n{}",
        SCHEMA.replace("score Int", "score Int\n    payload Payload")
    );
    let (baseline, _) = artifact(&schema, PERMISSION_ONLY, "baseline");
    for changed in [
        schema.replace("@default(1)", "@default(2)"),
        schema.replace("@default(1)", "@default(1) @index"),
    ] {
        let (changed, _) = artifact(&changed, PERMISSION_ONLY, "changed");
        assert_ne!(baseline["schema_id"], changed["schema_id"]);
    }
}

#[test]
fn manifest_local_plan_and_dependencies_match_typescript_not_permissions() {
    let (manifest, metadata) = artifact(
        SCHEMA,
        &format!("{LOCAL}\n{PERMISSION_ONLY}\n{INSERT}"),
        "metadata",
    );
    let mut reads = 0;
    let mut writes = 0;
    for (id, query) in manifest["queries"].as_object().unwrap() {
        let ts = &metadata[id];
        let fields = query["required_client_session_fields"]
            .as_array()
            .expect("dependency array");
        assert!(!fields.contains(&json!("serverOnly")));
        let ts_fields = fields
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        assert!(
            ts.contains(&format!("required_client_session_fields: [{ts_fields}],")),
            "{ts}"
        );
        if query["operation"] == "query" {
            reads += 1;
            assert!(query["session_args"]
                .as_array()
                .unwrap()
                .contains(&json!("serverOnly")));
            let plan = query["local_query_plan"]
                .as_str()
                .expect("read has local plan");
            assert!(
                ts.contains(plan),
                "manifest plan differs from TypeScript: {plan}\n{ts}"
            );
            assert!(!plan.contains("serverOnly"));
            if plan.contains("$session") {
                assert_eq!(
                    query["required_client_session_fields"],
                    json!(["minimum", "userId"])
                );
            } else {
                assert_eq!(query["required_client_session_fields"], json!([]));
            }
        } else {
            writes += 1;
            assert_eq!(query["required_client_session_fields"], json!([]));
            assert!(query.as_object().unwrap().contains_key("local_query_plan"));
            assert!(query["local_query_plan"].is_null());
            assert!(!ts.contains("const queryShape"));
        }
    }
    assert_eq!((reads, writes), (2, 1));
}
