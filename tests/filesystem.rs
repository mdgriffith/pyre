#![cfg(feature = "filesystem")]

use pyre::{ast, filesystem, generate, parser, typecheck};
use std::collections::HashMap;
use tempfile::TempDir;

#[test]
fn test_get_schema_source_matches_suffix_paths() {
    let mut found = filesystem::Found {
        schema_files: HashMap::new(),
        session_file: None,
        query_files: vec![],
        namespaces: vec![],
    };

    let schema_source = "type DevSource\n   = Browser\n".to_string();
    let schema_file = filesystem::SchemaFile {
        path: "/tmp/project/pyre/schema.pyre".to_string(),
        content: schema_source.clone(),
    };

    found
        .schema_files
        .insert("default".to_string(), vec![schema_file]);

    assert_eq!(
        filesystem::get_schema_source("pyre/schema.pyre", &found),
        Some(schema_source.as_str())
    );
    assert_eq!(
        filesystem::get_schema_source("schema.pyre", &found),
        Some(schema_source.as_str())
    );
}

#[test]
fn test_get_namespace_for_nested_schema_file() {
    let base_dir = std::path::Path::new("/tmp/project/pyre");
    let nested_path = std::path::Path::new("/tmp/project/pyre/schema/Auth/schema.pyre");

    let namespace = filesystem::get_namespace(nested_path, base_dir);
    assert_eq!(namespace, "Auth");
}

#[test]
fn test_collect_filepaths_groups_schema_files_by_namespace() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let root = temp_dir.path();

    std::fs::create_dir_all(root.join("pyre/schema/App")).expect("App schema dir should exist");
    std::fs::create_dir_all(root.join("pyre/schema/Auth")).expect("Auth schema dir should exist");

    std::fs::write(
        root.join("pyre/schema/App/schema.pyre"),
        "record Project {\n    id Int @id\n    @public\n}\n",
    )
    .expect("App schema file should be written");

    std::fs::write(
        root.join("pyre/schema/Auth/schema.pyre"),
        "record Account {\n    id Int @id\n    @public\n}\n",
    )
    .expect("Auth schema file should be written");

    let found = filesystem::collect_filepaths(root.join("pyre").as_path())
        .expect("collect_filepaths should succeed");

    assert!(found.schema_files.contains_key("App"));
    assert!(found.schema_files.contains_key("Auth"));
    assert_eq!(
        found.schema_files.get("App").map(|files| files.len()),
        Some(1)
    );
    assert_eq!(
        found.schema_files.get("Auth").map(|files| files.len()),
        Some(1)
    );
    assert!(found.session_file.is_none());
}

#[test]
fn test_collect_filepaths_reads_project_session_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let root = temp_dir.path().join("pyre");
    std::fs::create_dir_all(&root).expect("pyre dir should exist");
    std::fs::write(root.join("session.pyre"), "session {\n    userId Int\n}\n")
        .expect("session file should be written");
    std::fs::write(
        root.join("schema.pyre"),
        "record User {\n    id Int @id\n}\n",
    )
    .expect("schema file should be written");

    let found = filesystem::collect_filepaths(&root).expect("collect_filepaths should succeed");

    assert_eq!(
        found
            .session_file
            .as_ref()
            .map(|file| file.content.as_str()),
        Some("session {\n    userId Int\n}\n")
    );
    assert_eq!(found.query_files.len(), 0);
}

#[test]
fn test_schema_serialization_includes_shared_session() {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "record Project {\n    id Int @id\n    @allow(query) { ownerId == Session.userId }\n    ownerId Int\n}\n",
        &mut schema,
    )
    .expect("schema should parse");

    let mut session_schema = ast::Schema::default();
    parser::run(
        "session.pyre",
        "session {\n    userId Int\n}\n",
        &mut session_schema,
    )
    .expect("session should parse");
    schema.session = session_schema.session;

    typecheck::check_schema(&ast::Database {
        schemas: vec![schema.clone()],
    })
    .expect("shared session should typecheck against namespace schema");

    let stored_source = generate::to_string::schema_to_string("", &schema);
    assert!(stored_source.starts_with("session {\n    userId Int\n}\n"));

    let mut stored_schema = ast::Schema::default();
    parser::run("stored-schema.pyre", &stored_source, &mut stored_schema)
        .expect("stored schema should parse");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![stored_schema],
    })
    .expect("stored schema should retain session context");
    assert!(context.session.is_some());
}
