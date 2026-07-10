use pyre::ast;
use pyre::ast::resolve_id_brands;
use pyre::error;
use pyre::filesystem;
use pyre::generate;
use pyre::parser;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

pub struct Options<'a> {
    pub in_dir: &'a Path,
    pub enable_color: bool,
}

pub fn id_column() -> ast::Column {
    ast::Column {
        name: "id".to_string(),
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
    }
}

#[derive(Debug)]
pub struct FileError {
    pub source: String,
    pub errors: Vec<error::Error>,
}

pub fn check_namespace_requirements(namespace: &Option<String>, options: &Options) {
    let namespaces_result = crate::filesystem::read_namespaces(Path::new(&options.in_dir));
    match namespaces_result {
        Ok(namespaces_found) => match namespaces_found {
            filesystem::NamespacesFound::Default => {
                if let Some(_) = namespace {
                    println!("{}", error::format_custom_error("Namespace is not needed", "It looks like you only have one schema, which means you don't need to provide a namespace."));
                    std::process::exit(1);
                }
            }
            filesystem::NamespacesFound::MultipleNamespaces(namespaces) => {
                if let Some(namespace) = namespace {
                    if !namespaces.contains(namespace.as_str()) {
                        let error_body = format!(
                            "{} is not one of the allowed namespaces:\n{}",
                            error::yellow_if(true, namespace),
                            error::format_yellow_list(
                                true,
                                namespaces.into_iter().collect::<Vec<_>>()
                            )
                        );
                        let error_message =
                            error::format_custom_error("Unknown Schema", &error_body);
                        println!("{}", error_message);
                        std::process::exit(1);
                    }
                } else {
                    let error_body = format!("It looks like you have multiple schemas:\n{}\n Let me know which one you want to migrate by passing {}",
                            error::format_yellow_list(true, namespaces.into_iter().collect::<Vec<_>>()),
                            error::cyan_if(true, "--namespace SCHEMA_TO_MIGRATE")
                        );
                    let error_message = error::format_custom_error("Unknown Schema", &error_body);
                    println!("{}", error_message);
                    std::process::exit(1);
                }
            }
            filesystem::NamespacesFound::EmptySchemaDir
            | filesystem::NamespacesFound::NothingFound => {
                println!(
                    "{}",
                    error::format_custom_error(
                        "Schema Not Found",
                        "I was trying to find the schema, but it's not available."
                    )
                );
                std::process::exit(1);
            }
        },
        Err(err) => {
            println!("Error reading namespaces: {:?}", err);
            std::process::exit(1);
        }
    }
}

pub fn parse_single_schema(
    schema_file_path: &String,
    enable_color: bool,
) -> io::Result<ast::Schema> {
    let mut schema = ast::Schema {
        namespace: ast::DEFAULT_SCHEMANAME.to_string(),
        sync_mode: ast::SyncMode::Synced,
        files: vec![],
        session: None,
    };

    let mut file = fs::File::open(schema_file_path.clone())?;
    let mut schema_source = String::new();
    file.read_to_string(&mut schema_source)?;

    match parser::run(&schema_file_path, &schema_source, &mut schema) {
        Ok(()) => Ok(schema),
        Err(err) => {
            eprintln!(
                "{}",
                parser::render_error(&schema_source, err, enable_color)
            );
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Failed to parse schema",
            ))
        }
    }
}

pub fn parse_single_schema_from_source(
    schema_file_path: &str,
    schema_source: &str,
    enable_color: bool,
) -> io::Result<ast::Schema> {
    let mut schema = ast::Schema {
        namespace: ast::DEFAULT_SCHEMANAME.to_string(),
        sync_mode: ast::SyncMode::Synced,
        session: None,
        files: vec![],
    };

    match parser::run(&schema_file_path, &schema_source, &mut schema) {
        Ok(()) => Ok(schema),
        Err(err) => {
            eprintln!(
                "{}",
                parser::render_error(&schema_source, err, enable_color)
            );
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Failed to parse schema",
            ))
        }
    }
}

pub fn parse_database_schemas(
    paths: &filesystem::Found,
    enable_color: bool,
) -> io::Result<ast::Database> {
    let mut database = ast::Database {
        schemas: Vec::new(),
    };

    let session_source = paths.session_file.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Missing pyre/session.pyre. Define the shared session there.",
        )
    })?;
    let session =
        parse_session_source(&session_source.path, &session_source.content, enable_color)?;

    let mut schema_namespaces: Vec<_> = paths.schema_files.iter().collect();
    schema_namespaces.sort_by(|a, b| a.0.cmp(b.0));

    for (namespace, schema_files) in schema_namespaces {
        let mut schema = ast::Schema {
            namespace: namespace.clone(),
            sync_mode: ast::SyncMode::Synced,
            session: Some(session.clone()),
            files: vec![],
        };

        for source in schema_files.iter() {
            match parser::run(&source.path, &source.content, &mut schema) {
                Ok(()) => {
                    if schema.files.last().is_some_and(|file| {
                        file.definitions
                            .iter()
                            .any(|definition| matches!(definition, ast::Definition::Session(_)))
                    }) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "{} contains a session declaration. Define the shared session in pyre/session.pyre.",
                                source.path
                            ),
                        ));
                    }
                }
                Err(err) => {
                    eprintln!(
                        "{}",
                        parser::render_error(&source.content, err, enable_color)
                    );
                    std::process::exit(1);
                }
            }
        }

        database.schemas.push(schema);
    }

    // Post-process: resolve ID brands for all columns
    resolve_id_brands(&mut database);

    Ok(database)
}

pub fn parse_session_source(
    path: &str,
    source: &str,
    enable_color: bool,
) -> io::Result<ast::SessionDetails> {
    let mut schema = ast::Schema::default();
    match parser::run(path, source, &mut schema) {
        Ok(()) => {
            let definitions = schema
                .files
                .iter()
                .flat_map(|file| &file.definitions)
                .collect::<Vec<_>>();
            if definitions
                .iter()
                .filter(|definition| matches!(definition, ast::Definition::Session(_)))
                .count()
                != 1
                || definitions.iter().any(|definition| {
                    !matches!(
                        definition,
                        ast::Definition::Session(_)
                            | ast::Definition::Lines { .. }
                            | ast::Definition::Comment { .. }
                    )
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pyre/session.pyre may only contain one session declaration, comments, and blank lines.",
                ));
            }
            schema.session.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pyre/session.pyre must contain a session declaration.",
                )
            })
        }
        Err(err) => {
            eprintln!("{}", parser::render_error(source, err, enable_color));
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Failed to parse session",
            ))
        }
    }
}

pub fn write_schema(_options: &Options, to_stdout: &bool, schema: &ast::Schema) -> io::Result<()> {
    for schema_file in &schema.files {
        if *to_stdout {
            println!(
                "{}",
                generate::to_string::schemafile_to_string(&schema.namespace, schema_file)
            );
            continue;
        }
        let target_filepath = schema_file.path.to_string();
        let mut output = fs::File::create(&target_filepath)?;
        let formatted = generate::to_string::schemafile_to_string(&schema.namespace, schema_file);
        output.write_all(formatted.as_bytes())?;
    }
    Ok(())
}

pub fn write_db_schema(options: &Options, database: &ast::Database) -> io::Result<()> {
    for schema in database.schemas.iter() {
        write_schema(options, &false, &schema)?;
    }
    Ok(())
}

pub fn get_stdin() -> io::Result<Option<String>> {
    if io::stdin().is_terminal() {
        Ok(None)
    } else {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input)?;
        if input.is_empty() {
            Ok(None)
        } else {
            Ok(Some(input))
        }
    }
}
