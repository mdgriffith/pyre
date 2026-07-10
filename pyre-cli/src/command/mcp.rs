use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use super::docs::{find_doc, DocResource, DOC_RESOURCES};
use super::shared::Options;
use crate::db;
use pyre::server::manifest::{FieldSchema, Manifest, PyreSession, QueryManifest, SqlInfo};
use pyre::{ast, format, generate, parser, typecheck};

const SERVER_NAME: &str = "pyre";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[DEFAULT_PROTOCOL_VERSION];
pub async fn mcp(options: &Options<'_>) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    while let Some(message) = read_message(&mut reader)? {
        let request = match serde_json::from_slice::<JsonValue>(&message) {
            Ok(request) => request,
            Err(error) => {
                write_message(&mut writer, &parse_error(error.to_string()))?;
                continue;
            }
        };

        if let Some(response) = handle_message(options, request).await {
            write_message(&mut writer, &response)?;
        }
    }

    Ok(())
}

async fn handle_message(options: &Options<'_>, request: JsonValue) -> Option<JsonValue> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(JsonValue::as_str);

    Some(match method {
        Some("initialize") => success_response(id, initialize_result(&request)),
        Some("ping") => success_response(id, json!({})),
        Some("tools/list") => success_response(id, json!({ "tools": tools() })),
        Some("resources/list") => success_response(id, json!({ "resources": resources() })),
        Some("resources/read") => match read_resource(options, request.get("params")) {
            Ok(result) => success_response(id, result),
            Err(message) => error_response(id, -32602, &message),
        },
        Some("prompts/list") => success_response(id, json!({ "prompts": [] })),
        Some("tools/call") => match call_tool(options, request.get("params")).await {
            Ok(result) => success_response(id, result),
            Err(message) => error_response_data(
                id,
                -32603,
                &message,
                json!({
                    "message": message
                }),
            ),
        },
        Some(method) => error_response(id, -32601, &format!("Unknown method: {method}")),
        None => error_response(id, -32600, "Request is missing method"),
    })
}

fn initialize_result(request: &JsonValue) -> JsonValue {
    let requested_protocol_version = request
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(JsonValue::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    let protocol_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested_protocol_version) {
        requested_protocol_version
    } else {
        DEFAULT_PROTOCOL_VERSION
    };

    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "resources": {
                "listChanged": false
            },
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION
        }
    })
}

async fn call_tool(options: &Options<'_>, params: Option<&JsonValue>) -> Result<JsonValue, String> {
    let params = params.ok_or_else(|| "tools/call params are required".to_string())?;
    let name = params
        .get("name")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "tools/call params.name is required".to_string())?;
    let arguments = params.get("arguments").unwrap_or(&JsonValue::Null);

    let value = match name {
        "pyre_init" => init_project(options, arguments).await?,
        "pyre_project_info" => project_info(options, arguments)?,
        "pyre_docs" => docs(arguments)?,
        "pyre_schema" => schema(arguments)?,
        "pyre_check" => run_cli(options, arguments, &["check", "--json"])?,
        "pyre_format" => run_format(options, arguments)?,
        "pyre_generate" => run_generate(options, arguments)?,
        "pyre_generate_migration" => run_generate_migration(options, arguments)?,
        "pyre_migrate" => run_migrate(options, arguments)?,
        "pyre_introspect" => run_introspect(options, arguments)?,
        "pyre_db_status" => db_status(options, arguments).await?,
        "pyre_preview_query" => preview_dynamic_query(options, arguments)?,
        "pyre_explain_query" => explain_dynamic_query(options, arguments).await?,
        "pyre_query" => dynamic_query(options, arguments).await?,
        _ => return Err(format!("Unknown Pyre MCP tool: {name}")),
    };

    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        }]
    }))
}

async fn init_project(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let dir = string_arg(arguments, "dir").unwrap_or_else(|| "pyre".to_string());
    let schema_source = required_string_arg(arguments, "schema")?;
    let session_source =
        string_arg(arguments, "session").unwrap_or_else(|| "session {\n}\n".to_string());
    let namespace = string_arg(arguments, "namespace");
    let database = string_arg(arguments, "database");
    validate_safe_path("dir", &dir)?;
    if let Some(database) = database.as_ref() {
        validate_safe_path("database", database)?;
    }

    if let Some(namespace) = namespace.as_ref() {
        if namespace.is_empty() || namespace.contains('/') || namespace.contains('\\') {
            return Err("namespace must be a non-empty path segment".to_string());
        }
        if !namespace
            .chars()
            .next()
            .map(|char| char.is_uppercase())
            .unwrap_or(false)
        {
            return Err("namespace must be capitalized".to_string());
        }
    }

    let dir_path = PathBuf::from(&dir);
    if dir_path.exists() {
        return Err(format!("Directory already exists: {dir}"));
    }
    if let Some(database) = database.as_ref() {
        if Path::new(database).exists() {
            return Err(format!("Database already exists: {database}"));
        }
    }

    let schema_path = match namespace.as_ref() {
        Some(namespace) => dir_path.join("schema").join(namespace).join("schema.pyre"),
        None => dir_path.join("schema.pyre"),
    };
    let session_path = dir_path.join("session.pyre");
    let schema_path_string = schema_path.to_string_lossy().to_string();
    let real_namespace = namespace
        .clone()
        .unwrap_or_else(|| ast::DEFAULT_SCHEMANAME.to_string());
    let mut schema = ast::Schema {
        namespace: real_namespace.clone(),
        sync_mode: ast::SyncMode::Synced,
        session: None,
        files: vec![],
    };

    if let Err(error) = parser::run(&schema_path_string, &schema_source, &mut schema) {
        return Err(parser::render_error(
            &schema_source,
            error,
            options.enable_color,
        ));
    }
    if schema.files.iter().any(|file| {
        file.definitions
            .iter()
            .any(|definition| matches!(definition, ast::Definition::Session(_)))
    }) {
        return Err(
            "schema must not contain a session declaration; define it in session.pyre".to_string(),
        );
    }
    let session =
        super::shared::parse_session_source("session.pyre", &session_source, options.enable_color)
            .map_err(|error| error.to_string())?;
    schema.session = Some(session);

    let mut database_schema = ast::Database {
        schemas: vec![schema],
    };
    ast::resolve_id_brands(&mut database_schema);
    if let Err(errors) = typecheck::check_schema(&database_schema) {
        let errors = errors
            .iter()
            .map(pyre::error::format_json)
            .collect::<Vec<_>>();
        return Err(serde_json::to_string_pretty(&errors).map_err(|error| error.to_string())?);
    }

    format::database(&mut database_schema);
    let schema = database_schema
        .schemas
        .first()
        .ok_or_else(|| "No schema was parsed".to_string())?;
    let schema_file = schema
        .files
        .first()
        .ok_or_else(|| "No schema file was parsed".to_string())?;
    let formatted = generate::to_string::schemafile_to_string(&schema.namespace, schema_file);

    if let Some(parent) = schema_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&session_path, session_source).map_err(|error| error.to_string())?;
    fs::write(&schema_path, formatted).map_err(|error| error.to_string())?;

    let mut result = json!({
        "ok": true,
        "dir": dir,
        "namespace": real_namespace,
        "createdFiles": [session_path.to_string_lossy(), schema_path_string]
    });

    if let Some(database) = database {
        let mut migrate_args = vec![
            "migrate".to_string(),
            database.clone(),
            "--push".to_string(),
        ];
        if let Some(namespace) = namespace.as_ref() {
            migrate_args.push("--namespace".to_string());
            migrate_args.push(namespace.clone());
        }
        let migrate_result = run_cli(
            options,
            &json!({ "dir": dir }),
            &migrate_args.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;

        result["database"] = json!({
            "path": database,
            "migrated": migrate_result["ok"].as_bool().unwrap_or(false),
            "result": migrate_result
        });
        if result["database"]["migrated"] != json!(true) {
            result["ok"] = json!(false);
        }
    }

    Ok(result)
}

fn project_info(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let in_dir = input_dir(options, arguments)?;
    let found = crate::filesystem::collect_filepaths(&in_dir).map_err(|error| error.to_string())?;
    let generated_dir =
        string_arg(arguments, "generated").unwrap_or_else(|| "pyre/generated".to_string());
    let migration_dir =
        string_arg(arguments, "migration_dir").unwrap_or_else(|| "pyre/migrations".to_string());
    validate_safe_path("migration_dir", &migration_dir)?;
    validate_safe_path("generated", &generated_dir)?;

    Ok(json!({
        "inputDir": in_dir,
        "generatedDir": generated_dir,
        "generatedDirExists": Path::new(&generated_dir).exists(),
        "migrationDir": migration_dir,
        "migrationDirExists": Path::new(&migration_dir).exists(),
        "namespaces": sorted_keys(&found.schema_files),
        "schemaFileCount": found.schema_files.values().map(Vec::len).sum::<usize>(),
        "sessionFile": found.session_file.as_ref().map(|file| &file.path),
        "queryFileCount": found.query_files.len(),
        "queryFiles": found.query_files
    }))
}

fn docs(arguments: &JsonValue) -> Result<JsonValue, String> {
    let topic = string_arg(arguments, "topic").unwrap_or_else(|| "getting-started".to_string());
    let content = find_doc(&topic)
        .map(|doc| doc.content)
        .ok_or_else(|| format!("Unknown docs topic: {topic}"))?;

    Ok(json!({ "topic": topic, "content": content }))
}

fn read_resource(options: &Options<'_>, params: Option<&JsonValue>) -> Result<JsonValue, String> {
    let params = params.ok_or_else(|| "resources/read params are required".to_string())?;
    let uri = params
        .get("uri")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "resources/read params.uri is required".to_string())?;
    if uri == "pyre://project/schema" {
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&schema_from_dir(options.in_dir)?).map_err(|error| error.to_string())?
            }]
        }));
    }

    let text = doc_resource_content(uri).ok_or_else(|| format!("Unknown Pyre resource: {uri}"))?;

    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "text/markdown",
            "text": text
        }]
    }))
}

fn schema(arguments: &JsonValue) -> Result<JsonValue, String> {
    let in_dir = string_arg(arguments, "dir").unwrap_or_else(|| "pyre".to_string());
    validate_safe_path("dir", &in_dir)?;
    schema_from_dir(Path::new(&in_dir))
}

fn schema_from_dir(in_dir: &Path) -> Result<JsonValue, String> {
    let found = crate::filesystem::collect_filepaths(in_dir).map_err(|error| error.to_string())?;
    let mut schemas = Vec::new();
    let mut namespaces = found.schema_files.into_iter().collect::<Vec<_>>();
    namespaces.sort_by(|left, right| left.0.cmp(&right.0));

    for (namespace, files) in namespaces {
        for file in files {
            schemas.push(json!({
                "namespace": namespace,
                "path": file.path,
                "content": file.content
            }));
        }
    }

    Ok(json!({
        "session": found.session_file.map(|file| json!({
            "path": file.path,
            "content": file.content
        })),
        "schemas": schemas
    }))
}

fn run_format(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let mut args = vec!["format".to_string()];
    if bool_arg(arguments, "to_stdout").unwrap_or(false) {
        args.push("--to-stdout".to_string());
    }
    let files = string_array_arg(arguments, "files")?;
    for file in &files {
        validate_safe_path("files", file)?;
    }
    args.extend(files);
    run_cli(
        options,
        arguments,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn run_generate(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let out = string_arg(arguments, "out").unwrap_or_else(|| "pyre/generated".to_string());
    validate_safe_path("out", &out)?;
    run_cli(options, arguments, &["generate", "--out", &out])
}

fn run_generate_migration(
    options: &Options<'_>,
    arguments: &JsonValue,
) -> Result<JsonValue, String> {
    let name = required_string_arg(arguments, "name")?;
    let db = required_string_arg(arguments, "database")?;
    validate_database_ref("database", &db)?;
    let migration_dir =
        string_arg(arguments, "migration_dir").unwrap_or_else(|| "pyre/migrations".to_string());
    validate_safe_path("migration_dir", &migration_dir)?;
    let mut args = vec![
        "migration".to_string(),
        name,
        "--db".to_string(),
        db,
        "--migration-dir".to_string(),
        migration_dir,
    ];
    push_optional_arg(arguments, &mut args, "auth", "--auth");
    push_optional_arg(arguments, &mut args, "namespace", "--namespace");
    run_cli(
        options,
        arguments,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn run_migrate(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let database = required_string_arg(arguments, "database")?;
    validate_database_ref("database", &database)?;
    let migration_dir =
        string_arg(arguments, "migration_dir").unwrap_or_else(|| "pyre/migrations".to_string());
    validate_safe_path("migration_dir", &migration_dir)?;
    let mut args = vec![
        "migrate".to_string(),
        database,
        "--migration-dir".to_string(),
        migration_dir,
    ];
    push_optional_arg(arguments, &mut args, "auth", "--auth");
    push_optional_arg(arguments, &mut args, "namespace", "--namespace");
    if bool_arg(arguments, "push").unwrap_or(false) {
        args.push("--push".to_string());
    }
    run_cli(
        options,
        arguments,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn run_introspect(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let database = required_string_arg(arguments, "database")?;
    validate_database_ref("database", &database)?;
    let mut args = vec!["introspect".to_string(), database];
    push_optional_arg(arguments, &mut args, "auth", "--auth");
    push_optional_arg(arguments, &mut args, "namespace", "--namespace");
    run_cli(
        options,
        arguments,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn run_cli(
    options: &Options<'_>,
    arguments: &JsonValue,
    command_args: &[&str],
) -> Result<JsonValue, String> {
    let in_dir = input_dir(options, arguments)?;
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pyre"));
    let command = command_summary(&executable, &in_dir, command_args);
    let output = Command::new(executable)
        .arg("--in")
        .arg(&in_dir)
        .args(command_args)
        .output();

    match output {
        Ok(output) => Ok(json!({
            "ok": output.status.success(),
            "command": command,
            "status": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr)
        })),
        Err(error) => Ok(json!({ "ok": false, "command": command, "error": error.to_string() })),
    }
}

fn command_summary(executable: &Path, in_dir: &Path, command_args: &[&str]) -> Vec<String> {
    let mut command = vec![
        executable.to_string_lossy().to_string(),
        "--in".to_string(),
        in_dir.to_string_lossy().to_string(),
    ];
    command.extend(redact_command_args(command_args));
    command
}

fn redact_command_args(command_args: &[&str]) -> Vec<String> {
    let mut redacted = Vec::with_capacity(command_args.len());
    let mut redact_next = false;
    for arg in command_args {
        if redact_next {
            redacted.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }

        redacted.push((*arg).to_string());
        if *arg == "--auth" {
            redact_next = true;
        }
    }
    redacted
}

async fn db_status(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let database = required_string_arg(arguments, "database")?;
    validate_database_ref("database", &database)?;
    let auth = string_arg(arguments, "auth");
    let namespace =
        string_arg(arguments, "namespace").unwrap_or_else(|| ast::DEFAULT_SCHEMANAME.to_string());
    let migration_dir =
        string_arg(arguments, "migration_dir").unwrap_or_else(|| "pyre/migrations".to_string());
    let namespace_migration_dir = if namespace == ast::DEFAULT_SCHEMANAME {
        PathBuf::from(&migration_dir)
    } else {
        Path::new(&migration_dir).join(&namespace)
    };

    let db = match db::connect(&database, &auth).await {
        Ok(db) => db,
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "accessible": false,
                "status": "inaccessible",
                "error": error.format_error()
            }))
        }
    };
    let conn = db.connect().map_err(|error| error.to_string())?;
    let introspection = crate::db::introspect::introspect_connection(&conn)
        .await
        .map_err(|error| error.to_string())?;

    let migration_files = db::read_migration_items(&namespace_migration_dir).unwrap_or_default();
    let applied_migrations = match &introspection.migration_state {
        pyre::db::introspect::MigrationState::NoMigrationTable => Vec::new(),
        pyre::db::introspect::MigrationState::MigrationTable { migrations } => migrations
            .iter()
            .map(|migration| migration.name.clone())
            .collect::<Vec<_>>(),
    };
    let pending_migrations = migration_files
        .iter()
        .filter(|name| !applied_migrations.contains(name))
        .cloned()
        .collect::<Vec<_>>();

    let mut schema_status = json!({ "checked": false });
    let status = if !pending_migrations.is_empty() {
        "pending_migrations"
    } else {
        match current_schema_context(options, arguments) {
            Ok((database_schema, context, _paths)) => {
                if let Some(schema) = database_schema
                    .schemas
                    .iter()
                    .find(|schema| schema.namespace == namespace)
                {
                    let diff = pyre::db::diff::diff(&context, schema, &introspection);
                    let up_to_date = pyre::db::diff::is_empty(&diff);
                    schema_status =
                        json!({ "checked": true, "upToDate": up_to_date, "diff": diff });
                    if up_to_date {
                        "up_to_date"
                    } else {
                        "schema_drift"
                    }
                } else {
                    schema_status = json!({ "checked": false, "error": format!("namespace not found: {namespace}") });
                    "unknown"
                }
            }
            Err(error) => {
                schema_status = json!({ "checked": false, "error": error });
                "unknown"
            }
        }
    };

    Ok(json!({
        "ok": true,
        "accessible": true,
        "status": status,
        "namespace": namespace,
        "appliedMigrations": applied_migrations,
        "pendingMigrations": pending_migrations,
        "schema": schema_status
    }))
}

async fn dynamic_query(options: &Options<'_>, arguments: &JsonValue) -> Result<JsonValue, String> {
    let database = required_string_arg(arguments, "database")?;
    validate_database_ref("database", &database)?;
    let auth = string_arg(arguments, "auth");
    let input = arguments
        .get("params")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let session_value = arguments
        .get("session")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let (query_list, manifest) = dynamic_query_plan(options, arguments)?;
    let db = db::connect(&database, &auth)
        .await
        .map_err(|error| error.format_error())?;
    let conn = db.connect().map_err(|error| error.to_string())?;
    let session = PyreSession::new(session_value, &manifest.session_schema)
        .map_err(|error| error.to_string())?;

    let mut results = Vec::new();
    for query_def in &query_list.queries {
        if let ast::QueryDef::Query(query) = query_def {
            let result =
                pyre::server::query::run(&conn, &manifest, &query.name, input.clone(), &session)
                    .await
                    .map_err(|error| error.to_string())?;
            let manifest_query = manifest
                .queries
                .get(&query.name)
                .expect("manifest query exists");
            results.push(json!({
                "name": query.name,
                "operation": manifest_query.operation,
                "response": result.response,
                "affectedRows": result.affected_rows,
                "sql": manifest_query.sql
            }));
        }
    }

    Ok(json!({ "ok": true, "results": results }))
}

fn preview_dynamic_query(
    options: &Options<'_>,
    arguments: &JsonValue,
) -> Result<JsonValue, String> {
    let (query_list, manifest) = dynamic_query_plan(options, arguments)?;
    let mut results = Vec::new();

    for query_def in &query_list.queries {
        if let ast::QueryDef::Query(query) = query_def {
            let manifest_query = manifest
                .queries
                .get(&query.name)
                .expect("manifest query exists");
            results.push(json!({
                "name": query.name,
                "operation": manifest_query.operation,
                "primaryDb": manifest_query.primary_db,
                "attachedDbs": manifest_query.attached_dbs,
                "inputSchema": manifest_query.input_schema,
                "sessionArgs": manifest_query.session_args,
                "optionalInputArgs": manifest_query.optional_input_args,
                "sql": manifest_query.sql
            }));
        }
    }

    Ok(json!({ "ok": true, "results": results }))
}

async fn explain_dynamic_query(
    options: &Options<'_>,
    arguments: &JsonValue,
) -> Result<JsonValue, String> {
    let database = required_string_arg(arguments, "database")?;
    validate_database_ref("database", &database)?;
    let auth = string_arg(arguments, "auth");
    let input = arguments
        .get("params")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let session_value = arguments
        .get("session")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let (query_list, manifest) = dynamic_query_plan(options, arguments)?;
    let db = db::connect(&database, &auth)
        .await
        .map_err(|error| error.format_error())?;
    let conn = db.connect().map_err(|error| error.to_string())?;
    let session = PyreSession::new(session_value, &manifest.session_schema)
        .map_err(|error| error.to_string())?;

    let mut results = Vec::new();
    for query_def in &query_list.queries {
        if let ast::QueryDef::Query(query) = query_def {
            let statements = pyre::server::query::explain(
                &conn,
                &manifest,
                &query.name,
                input.clone(),
                &session,
            )
            .await
            .map_err(|error| error.to_string())?;
            let manifest_query = manifest
                .queries
                .get(&query.name)
                .expect("manifest query exists");
            let statements = statements
                .into_iter()
                .map(|statement| {
                    json!({
                        "include": statement.include,
                        "sql": statement.sql,
                        "params": statement.params,
                        "values": statement.values,
                        "plan": statement.plan,
                        "error": statement.error
                    })
                })
                .collect::<Vec<_>>();
            results.push(json!({
                "name": query.name,
                "operation": manifest_query.operation,
                "statements": statements
            }));
        }
    }

    Ok(json!({ "ok": true, "results": results }))
}

fn dynamic_query_plan(
    options: &Options<'_>,
    arguments: &JsonValue,
) -> Result<(ast::QueryList, Manifest), String> {
    let query_source = required_string_arg(arguments, "query")?;
    let (database_schema, context, _paths) = current_schema_context(options, arguments)?;
    let query_list = pyre::parser::parse_query("mcp.pyre", &query_source)
        .map_err(|_| "Failed to parse dynamic Pyre query".to_string())?;
    let query_infos = pyre::typecheck::check_queries(&query_list, &context)
        .map_err(|errors| format!("Dynamic query failed typecheck: {errors:#?}"))?;
    let manifest = dynamic_manifest(&database_schema, &context, &query_list, &query_infos)?;
    Ok((query_list, manifest))
}

fn current_schema_context(
    options: &Options<'_>,
    arguments: &JsonValue,
) -> Result<
    (
        ast::Database,
        pyre::typecheck::Context,
        pyre::filesystem::Found,
    ),
    String,
> {
    let in_dir = input_dir(options, arguments)?;
    let paths = crate::filesystem::collect_filepaths(&in_dir).map_err(|error| error.to_string())?;
    let database_schema =
        super::shared::parse_database_schemas(&paths, false).map_err(|error| error.to_string())?;
    let context = pyre::typecheck::check_schema(&database_schema)
        .map_err(|errors| format!("Schema failed typecheck: {errors:#?}"))?;
    Ok((database_schema, context, paths))
}

fn dynamic_manifest(
    database: &ast::Database,
    context: &pyre::typecheck::Context,
    query_list: &ast::QueryList,
    query_infos: &HashMap<String, pyre::typecheck::QueryInfo>,
) -> Result<Manifest, String> {
    let mut queries = HashMap::new();
    for query_def in &query_list.queries {
        let ast::QueryDef::Query(query) = query_def else {
            continue;
        };
        let query_info = query_infos
            .get(&query.name)
            .ok_or_else(|| format!("Missing typecheck info for query {}", query.name))?;
        queries.insert(
            query.name.clone(),
            dynamic_query_manifest(context, query, query_info)?,
        );
    }

    Ok(Manifest {
        version: 1,
        session_schema: session_schema(database),
        queries,
    })
}

fn dynamic_query_manifest(
    context: &pyre::typecheck::Context,
    query: &ast::Query,
    query_info: &pyre::typecheck::QueryInfo,
) -> Result<QueryManifest, String> {
    let param_names = query_param_names(query, query_info);
    let mut sql = Vec::new();

    for field in &query.fields {
        let ast::TopLevelQueryField::Field(query_field) = field else {
            continue;
        };
        let table = context
            .tables
            .get(&query_field.name)
            .ok_or_else(|| format!("Unknown query table {}", query_field.name))?;
        for prepared in
            pyre::generate::sql::to_string(context, query, query_info, table, query_field)
        {
            sql.push(SqlInfo {
                include: prepared.include,
                params: param_names.clone(),
                sql: prepared.sql,
            });
        }
    }

    Ok(QueryManifest {
        id: query.name.clone(),
        operation: format!("{:?}", query.operation).to_lowercase(),
        primary_db: query_info.primary_db.clone(),
        attached_dbs: query_info.attached_dbs.iter().cloned().collect(),
        input_schema: input_schema(query_info),
        session_args: session_args(query_info),
        optional_input_args: query
            .args
            .iter()
            .filter(|arg| arg.omittable)
            .map(|arg| arg.name.clone())
            .collect(),
        json_input_args: Vec::new(),
        sql,
        sync_sql: None,
    })
}

fn input_schema(query_info: &pyre::typecheck::QueryInfo) -> HashMap<String, FieldSchema> {
    let mut schema = HashMap::new();
    for param in query_info.variables.values() {
        if let pyre::typecheck::ParamInfo::Defined {
            raw_variable_name,
            type_,
            nullable,
            from_session,
            ..
        } = param
        {
            if *from_session {
                continue;
            }
            schema.insert(
                raw_variable_name.clone(),
                FieldSchema {
                    type_: type_.clone().unwrap_or_else(|| "Json".to_string()),
                    nullable: *nullable,
                    omittable: false,
                },
            );
        }
    }
    schema
}

fn session_schema(database: &ast::Database) -> HashMap<String, FieldSchema> {
    let session = database
        .schemas
        .iter()
        .find_map(|schema| schema.session.clone())
        .unwrap_or_else(ast::default_session_details);
    let mut schema = HashMap::new();
    for field in session.fields {
        if let ast::Field::Column(column) = field {
            schema.insert(
                column.name,
                FieldSchema {
                    type_: column.type_.to_string(),
                    nullable: column.nullable,
                    omittable: false,
                },
            );
        }
    }
    schema
}

fn query_param_names(query: &ast::Query, query_info: &pyre::typecheck::QueryInfo) -> Vec<String> {
    let mut names = Vec::new();
    for param in query_info.variables.values() {
        if let pyre::typecheck::ParamInfo::Defined {
            raw_variable_name, ..
        } = param
        {
            names.push(raw_variable_name.clone());
        }
    }
    for arg in &query.args {
        if arg.omittable {
            names.push(format!("{}__is_set", arg.name));
        }
    }
    names.sort();
    names.dedup();
    names
}

fn session_args(query_info: &pyre::typecheck::QueryInfo) -> Vec<String> {
    let mut args = Vec::new();
    for param in query_info.variables.values() {
        if let pyre::typecheck::ParamInfo::Defined {
            from_session: true,
            session_name: Some(name),
            used: true,
            ..
        } = param
        {
            args.push(name.clone());
        }
    }
    args.sort();
    args.dedup();
    args
}

fn input_dir(options: &Options<'_>, arguments: &JsonValue) -> Result<PathBuf, String> {
    match string_arg(arguments, "dir") {
        Some(path) => {
            validate_safe_path("dir", &path)?;
            Ok(PathBuf::from(path))
        }
        None => Ok(options.in_dir.to_path_buf()),
    }
}

fn validate_safe_path(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }

    let path = Path::new(value);
    if path.is_absolute() {
        return Err(format!("{name} must be relative to the workspace"));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("{name} must not contain '..' or root components"));
    }

    Ok(())
}

fn validate_database_ref(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }

    if value == ":memory:" {
        return Ok(());
    }

    if let Some(env_var_name) = value.strip_prefix('$') {
        if env_var_name.is_empty()
            || !env_var_name
                .chars()
                .all(|char| char.is_ascii_alphanumeric() || char == '_')
        {
            return Err(format!(
                "{name} environment reference must be like $PYRE_DB"
            ));
        }
        return Ok(());
    }

    if value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("libsql://")
    {
        if value.chars().any(char::is_whitespace) {
            return Err(format!("{name} URL must not contain whitespace"));
        }
        return Ok(());
    }

    validate_safe_path(name, value)
}

fn required_string_arg(arguments: &JsonValue, name: &str) -> Result<String, String> {
    string_arg(arguments, name).ok_or_else(|| format!("Missing required argument: {name}"))
}

fn string_arg(arguments: &JsonValue, name: &str) -> Option<String> {
    arguments
        .get(name)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn bool_arg(arguments: &JsonValue, name: &str) -> Option<bool> {
    arguments.get(name).and_then(JsonValue::as_bool)
}

fn string_array_arg(arguments: &JsonValue, name: &str) -> Result<Vec<String>, String> {
    match arguments.get(name) {
        None | Some(JsonValue::Null) => Ok(Vec::new()),
        Some(JsonValue::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(ToString::to_string)
                    .ok_or_else(|| format!("{name} must be an array of strings"))
            })
            .collect(),
        _ => Err(format!("{name} must be an array of strings")),
    }
}

fn push_optional_arg(arguments: &JsonValue, args: &mut Vec<String>, name: &str, flag: &str) {
    if let Some(value) = string_arg(arguments, name) {
        args.push(flag.to_string());
        args.push(value);
    }
}

fn sorted_keys<T>(map: &HashMap<String, T>) -> Vec<String> {
    let mut keys = map.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn tools() -> Vec<JsonValue> {
    vec![
        tool("pyre_init", "Initialize a new Pyre schema directory from provided schema source. Fails if the target directory already exists or the schema is invalid.", json!({"type":"object","required":["schema"],"properties":{"dir":{"type":"string","description":"Directory to initialize. Defaults to pyre."},"schema":{"type":"string","description":"Initial namespace schema source to write."},"session":{"type":"string","description":"Optional shared session declaration source to write to session.pyre."},"namespace":{"type":"string","description":"Optional schema namespace. If omitted, uses the default namespace."},"database":{"type":"string","description":"Optional local database path to create and migrate, for example pyre.db."}}})),
        tool("pyre_project_info", "Report basic information about the current Pyre project.", json!({"type":"object","properties":{"dir":{"type":"string"},"generated":{"type":"string"},"migration_dir":{"type":"string"}}})),
        tool("pyre_docs", "Return bundled Pyre documentation by topic.", json!({"type":"object","properties":{"topic":{"type":"string","enum":["getting-started","schema","query","namespacing","sync","migrations","project-structure","serve","troubleshooting","mcp"]}}})),
        tool("pyre_schema", "Return all Pyre schema files and their contents.", json!({"type":"object","properties":{"dir":{"type":"string"}}})),
        tool("pyre_check", "Typecheck the current Pyre schema and query files.", json!({"type":"object","properties":{"dir":{"type":"string"}}})),
        tool("pyre_format", "Format Pyre files. This may write files unless to_stdout is true.", json!({"type":"object","properties":{"dir":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"to_stdout":{"type":"boolean"}}})),
        tool("pyre_generate", "Generate Pyre artifacts.", json!({"type":"object","properties":{"dir":{"type":"string"},"out":{"type":"string"}}})),
        tool("pyre_generate_migration", "Generate a Pyre migration against a database.", json!({"type":"object","required":["name","database"],"properties":{"dir":{"type":"string"},"name":{"type":"string"},"database":{"type":"string"},"auth":{"type":"string"},"namespace":{"type":"string"},"migration_dir":{"type":"string"}}})),
        tool("pyre_migrate", "Apply Pyre migrations to a database. This can modify the database.", json!({"type":"object","required":["database"],"properties":{"dir":{"type":"string"},"database":{"type":"string"},"auth":{"type":"string"},"namespace":{"type":"string"},"migration_dir":{"type":"string"},"push":{"type":"boolean"}}})),
        tool("pyre_introspect", "Introspect a database and generate a Pyre schema file.", json!({"type":"object","required":["database"],"properties":{"dir":{"type":"string"},"database":{"type":"string","description":"A local path, URL, or $ENV_VAR database reference."},"auth":{"type":"string","description":"Optional auth token for remote libSQL/Turso databases."},"namespace":{"type":"string","description":"Optional namespace for the generated schema."}}})),
        tool("pyre_db_status", "Check database connectivity and current schema/migration status.", json!({"type":"object","required":["database"],"properties":{"dir":{"type":"string"},"database":{"type":"string"},"auth":{"type":"string"},"namespace":{"type":"string"},"migration_dir":{"type":"string"}}})),
        tool("pyre_preview_query", "Typecheck raw dynamic Pyre query text and return generated SQL, input schema, and session requirements without connecting to a database or executing it.", json!({"type":"object","required":["query"],"properties":{"dir":{"type":"string"},"query":{"type":"string"}}})),
        tool("pyre_explain_query", "Typecheck raw dynamic Pyre query text, validate params/session like pyre_query, and run EXPLAIN QUERY PLAN against a database without executing the query.", json!({"type":"object","required":["database","query"],"properties":{"dir":{"type":"string"},"database":{"type":"string"},"auth":{"type":"string"},"query":{"type":"string"},"params":{"type":"object"},"session":{"type":"object"}}})),
        tool("pyre_query", "Typecheck and execute raw dynamic Pyre query text against a database. Supports reads and mutations.", json!({"type":"object","required":["database","query"],"properties":{"dir":{"type":"string"},"database":{"type":"string"},"auth":{"type":"string"},"query":{"type":"string"},"params":{"type":"object"},"session":{"type":"object"}}})),
    ]
}

fn resources() -> Vec<JsonValue> {
    let mut resources = vec![json!({
        "uri": "pyre://project/schema",
        "name": "Project Schema",
        "description": "Current project schema files and source content.",
        "mimeType": "application/json"
    })];
    resources.extend(DOC_RESOURCES.iter().map(|doc| {
        json!({
            "uri": doc.uri,
            "name": doc.name,
            "description": doc.description,
            "mimeType": "text/markdown"
        })
    }));
    resources
}

fn doc_resource_content(uri: &str) -> Option<&'static str> {
    DOC_RESOURCES
        .iter()
        .find_map(|doc: &DocResource| (doc.uri == uri).then_some(doc.content))
}

fn tool(name: &str, description: &str, input_schema: JsonValue) -> JsonValue {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn success_response(id: JsonValue, result: JsonValue) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(id: JsonValue, code: i64, message: &str) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn error_response_data(id: JsonValue, code: i64, message: &str, data: JsonValue) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": data
        }
    })
}

fn parse_error(message: String) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": -32700,
            "message": message
        }
    })
}

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let bytes_read = reader.read_until(b'\n', &mut line)?;
    if bytes_read == 0 {
        return Ok(None);
    }

    Ok(Some(line))
}

fn write_message<W: Write>(writer: &mut W, message: &JsonValue) -> io::Result<()> {
    let body = serde_json::to_vec(message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    writer.write_all(&body)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
