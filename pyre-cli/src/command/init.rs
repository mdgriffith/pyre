use std::fs;
use std::io;

use super::shared::{id_column, parse_session_source, write_db_schema, Options};
use pyre::ast;
use pyre::error;
use pyre::format;

pub fn init(options: &Options, multidb: bool, session: Option<&str>) -> io::Result<()> {
    let mut database = ast::Database {
        schemas: Vec::new(),
    };
    let cwd = std::env::current_dir().expect("Failed to get current directory");
    let pyre_dir = cwd.join("pyre");
    let session_source = session.unwrap_or("session {\n}\n");
    parse_session_source("session.pyre", session_source, options.enable_color)?;

    if !pyre_dir.exists() {
        fs::create_dir(&pyre_dir).expect("Failed to create pyre directory");
    } else {
        error::format_custom_error(
            "Directory exists",
            "The pyre directory already exists in the current directory",
        );
        std::process::exit(1);
    }

    fs::write(pyre_dir.join("session.pyre"), session_source)?;

    if multidb {
        let schema_dir = pyre_dir.join("schema");
        crate::filesystem::create_dir_if_not_exists(&schema_dir)?;

        // Create Base Schema
        let base_dir = schema_dir.join("base");
        crate::filesystem::create_dir_if_not_exists(&base_dir)?;

        database.schemas.push(ast::Schema {
            namespace: "Base".to_string(),
            sync_mode: ast::SyncMode::Synced,
            session: None,
            files: vec![ast::SchemaFile {
                path: base_dir.join("schema.pyre").to_string_lossy().to_string(),
                definitions: vec![ast::Definition::Record {
                    name: "User".to_string(),
                    fields: vec![
                        ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                            ast::PermissionDetails::Public,
                        )),
                        ast::Field::Column(id_column()),
                    ],
                    start: None,
                    end: None,
                    start_name: None,
                    end_name: None,
                }],
            }],
        });

        // Create User Schema
        let user_dir = schema_dir.join("user");
        crate::filesystem::create_dir_if_not_exists(&user_dir)?;

        database.schemas.push(ast::Schema {
            namespace: "User".to_string(),
            sync_mode: ast::SyncMode::Synced,
            session: None,
            files: vec![ast::SchemaFile {
                path: user_dir.join("schema.pyre").to_string_lossy().to_string(),
                definitions: vec![ast::Definition::Record {
                    name: "Example".to_string(),
                    fields: vec![
                        ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                            ast::PermissionDetails::Public,
                        )),
                        ast::Field::FieldDirective(ast::FieldDirective::Link(ast::LinkDetails {
                            link_name: "user".to_string(),
                            local_ids: vec!["userId".to_string()],
                            foreign: ast::Qualified {
                                schema: "base".to_string(),
                                table: "User".to_string(),
                                fields: vec!["id".to_string()],
                            },
                            start_name: None,
                            end_name: None,
                            inline_comment: None,
                        })),
                        ast::Field::Column(id_column()),
                        ast::Field::Column(ast::Column {
                            name: "userId".to_string(),
                            type_: ast::ColumnType::Int,
                            nullable: false,
                            directives: vec![ast::ColumnDirective::PrimaryKey],
                            start: None,
                            end: None,
                            start_name: None,
                            end_name: None,
                            start_typename: None,
                            end_typename: None,
                            inline_comment: None,
                        }),
                    ],
                    start: None,
                    end: None,
                    start_name: None,
                    end_name: None,
                }],
            }],
        });
    } else {
        let records = vec![ast::Definition::Record {
            name: "User".to_string(),
            fields: vec![
                ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                    ast::PermissionDetails::Public,
                )),
                ast::Field::Column(id_column()),
            ],
            start: None,
            end: None,
            start_name: None,
            end_name: None,
        }];
        database.schemas.push(ast::Schema {
            namespace: ast::DEFAULT_SCHEMANAME.to_string(),
            sync_mode: ast::SyncMode::Synced,
            session: None,
            files: vec![ast::SchemaFile {
                path: pyre_dir.join("schema.pyre").to_str().unwrap().to_string(),
                definitions: records,
            }],
        });
    }

    format::database(&mut database);
    write_db_schema(options, &database)?;

    Ok(())
}
