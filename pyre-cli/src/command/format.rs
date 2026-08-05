use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use super::shared::{
    display_path, get_stdin, parse_database_schemas, parse_single_schema,
    parse_single_schema_from_source, pyre_file_count, write_db_schema, write_schema, Options,
};
use crate::filesystem;
use pyre::ast;
use pyre::format;
use pyre::generate;
use pyre::parser;

pub(super) fn format_source(
    options: &Options,
    file_path: &str,
    source: &str,
) -> io::Result<Option<String>> {
    let mut paths = filesystem::collect_filepaths(&options.in_dir)?;
    if source_is_schema(&paths, options.in_dir, file_path) {
        if replace_schema_source(&mut paths, file_path, Some(source)) {
            let mut database = parse_database_schemas(&paths, false)?;
            format::database(&mut database);
            for schema in &database.schemas {
                for schema_file in &schema.files {
                    if paths_match(&schema_file.path, file_path) {
                        return Ok(Some(generate::to_string::schemafile_to_string(
                            &schema.namespace,
                            schema_file,
                        )));
                    }
                }
            }
        }

        let mut schema = parse_single_schema_from_source(file_path, source, false)?;
        format::schema(&mut schema);
        return Ok(schema.files.first().map(|schema_file| {
            generate::to_string::schemafile_to_string(&schema.namespace, schema_file)
        }));
    }

    let database = parse_database_schemas(&paths, false)?;
    let mut queries = match parser::parse_query(file_path, source) {
        Ok(queries) => queries,
        Err(_) => return Ok(None),
    };
    format::query_list(&database, &mut queries);
    Ok(Some(generate::to_string::query(&queries)))
}

pub fn format(options: &Options, files: &Vec<String>, to_stdout: bool) -> io::Result<()> {
    match files.len() {
        0 => match get_stdin()? {
            Some(stdin) => {
                let paths = filesystem::collect_filepaths(&options.in_dir)?;
                let schema = parse_database_schemas(&paths, options.enable_color)?;

                // We're assuming this file is a query because we don't have a filepath
                format_query_to_std_out(&options, &schema, &stdin)?;
            }
            None => {
                let paths = filesystem::collect_filepaths(&options.in_dir)?;
                let file_count = pyre_file_count(&paths);
                format_all(&options, paths)?;
                print_summary(options, file_count);
            }
        },
        1 => {
            let file_path = &files[0];

            match get_stdin()? {
                Some(stdin) => {
                    if pyre::filesystem::is_schema_file(file_path) {
                        if !format_schema_file_with_context(options, file_path, Some(&stdin), true)?
                        {
                            let mut schema = parse_single_schema_from_source(
                                file_path,
                                &stdin,
                                options.enable_color,
                            )?;
                            format::schema(&mut schema);
                            // Always write to stdout if stdin is provided
                            write_schema(&options, &true, &schema)?;
                        }
                    } else {
                        let paths = filesystem::collect_filepaths(&options.in_dir)?;
                        let schema = parse_database_schemas(&paths, options.enable_color)?;

                        format_query_to_std_out(&options, &schema, &stdin)?;
                    }
                }
                None => {
                    if pyre::filesystem::is_schema_file(file_path) {
                        if !format_schema_file_with_context(options, file_path, None, to_stdout)? {
                            let mut schema = parse_single_schema(file_path, options.enable_color)?;
                            format::schema(&mut schema);
                            write_schema(&options, &to_stdout, &schema)?;
                        }
                    } else {
                        let paths = filesystem::collect_filepaths(&options.in_dir)?;
                        let database = parse_database_schemas(&paths, options.enable_color)?;

                        format_query(&options, &database, &to_stdout, file_path)?;
                    }
                    if !to_stdout {
                        print_summary(options, 1);
                    }
                }
            }
        }
        _ => {
            let mut pending_schema_files: Vec<String> = vec![];
            for file_path in files {
                if !file_path.ends_with(".pyre") && !to_stdout {
                    println!("{} doesn't end in .pyre, skipping", file_path);
                    continue;
                }

                if pyre::filesystem::is_schema_file(&file_path) {
                    pending_schema_files.push(file_path.clone());
                } else {
                    let paths = filesystem::collect_filepaths(&options.in_dir)?;
                    let database = parse_database_schemas(&paths, options.enable_color)?;

                    format_query(&options, &database, &to_stdout, &file_path)?;
                }
            }

            if !pending_schema_files.is_empty() {
                format_schema_files_with_context(options, &pending_schema_files, to_stdout)?;
            }

            if !to_stdout {
                let file_count = files.iter().filter(|path| path.ends_with(".pyre")).count();
                print_summary(options, file_count);
            }
        }
    }
    Ok(())
}

fn print_summary(options: &Options, file_count: usize) {
    println!(
        "Formatted {} Pyre {} in {}.",
        file_count,
        if file_count == 1 { "file" } else { "files" },
        display_path(options.in_dir),
    );
}

fn format_all(options: &Options, paths: pyre::filesystem::Found) -> io::Result<()> {
    let mut database = parse_database_schemas(&paths, options.enable_color)?;

    format::database(&mut database);
    write_db_schema(options, &database)?;
    if let Some(session_file) = paths.session_file {
        let mut session = parse_single_schema(&session_file.path, options.enable_color)?;
        format::schema(&mut session);
        write_schema(options, &false, &session)?;
    }

    // Format queries
    for query_file_path in paths.query_files {
        format_query(&options, &database, &false, &query_file_path)?;
    }

    Ok(())
}

fn format_schema_files_with_context(
    options: &Options,
    file_paths: &[String],
    to_stdout: bool,
) -> io::Result<()> {
    let paths = filesystem::collect_filepaths(&options.in_dir)?;
    let mut database = parse_database_schemas(&paths, options.enable_color)?;
    format::database(&mut database);

    for file_path in file_paths {
        write_selected_schema_file(&database, file_path, to_stdout)?;
    }

    Ok(())
}

fn format_schema_file_with_context(
    options: &Options,
    file_path: &str,
    override_source: Option<&str>,
    to_stdout: bool,
) -> io::Result<bool> {
    let mut paths = filesystem::collect_filepaths(&options.in_dir)?;

    if !replace_schema_source(&mut paths, file_path, override_source) {
        return Ok(false);
    }

    let mut database = parse_database_schemas(&paths, options.enable_color)?;
    format::database(&mut database);
    write_selected_schema_file(&database, file_path, to_stdout)?;

    Ok(true)
}

fn replace_schema_source(
    paths: &mut pyre::filesystem::Found,
    file_path: &str,
    override_source: Option<&str>,
) -> bool {
    let mut matched = false;
    for schema_files in paths.schema_files.values_mut() {
        for schema_file in schema_files.iter_mut() {
            if paths_match(&schema_file.path, file_path) {
                matched = true;
                if let Some(source) = override_source {
                    schema_file.content = source.to_string();
                }
            }
        }
    }

    matched
}

fn write_selected_schema_file(
    database: &ast::Database,
    file_path: &str,
    to_stdout: bool,
) -> io::Result<()> {
    for schema in &database.schemas {
        for schema_file in &schema.files {
            let schema_path = Path::new(&schema_file.path);
            if paths_match(&schema_file.path, file_path) {
                let formatted =
                    generate::to_string::schemafile_to_string(&schema.namespace, schema_file);

                if to_stdout {
                    println!("{}", formatted);
                } else {
                    let mut output = fs::File::create(schema_path)?;
                    output.write_all(formatted.as_bytes())?;
                }

                return Ok(());
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("Schema file not found: {}", file_path),
    ))
}

fn paths_match(left: &str, right: &str) -> bool {
    left == right || Path::new(left).ends_with(right) || Path::new(right).ends_with(left)
}

fn source_is_schema(paths: &pyre::filesystem::Found, in_dir: &Path, file_path: &str) -> bool {
    if paths
        .schema_files
        .values()
        .flatten()
        .any(|file| paths_match(&file.path, file_path))
        || paths
            .session_file
            .as_ref()
            .is_some_and(|file| paths_match(&file.path, file_path))
    {
        return true;
    }

    let path = Path::new(file_path);
    let absolute_in_dir = if in_dir.is_absolute() {
        in_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(in_dir))
            .unwrap_or_else(|_| in_dir.to_path_buf())
    };
    let Ok(relative) = path.strip_prefix(absolute_in_dir) else {
        return path.file_name().and_then(|name| name.to_str()) == Some("session.pyre")
            || path.file_name().and_then(|name| name.to_str()) == Some("schema.pyre");
    };
    relative.file_name().and_then(|name| name.to_str()) == Some("session.pyre")
        || relative.file_name().and_then(|name| name.to_str()) == Some("schema.pyre")
        || relative
            .components()
            .any(|component| component.as_os_str() == "schema")
}

fn format_query_to_std_out(
    _options: &Options,
    database: &ast::Database,
    query_source_str: &str,
) -> io::Result<()> {
    match parser::parse_query("stdin", query_source_str) {
        Ok(mut query_list) => {
            // Format query
            format::query_list(database, &mut query_list);

            // Convert to string
            let formatted = generate::to_string::query(&query_list);

            println!("{}", formatted);
            return Ok(());
        }
        Err(_) => {
            println!("{}", query_source_str);
            return Ok(());
        }
    }
}

fn format_query(
    options: &Options,
    database: &ast::Database,
    to_stdout: &bool,
    query_file_path: &str,
) -> io::Result<()> {
    let mut query_file = fs::File::open(query_file_path)?;
    let mut query_source_str = String::new();
    query_file.read_to_string(&mut query_source_str)?;

    match parser::parse_query(query_file_path, &query_source_str) {
        Ok(mut query_list) => {
            // Format query
            format::query_list(database, &mut query_list);

            // Convert to string
            let formatted = generate::to_string::query(&query_list);
            if *to_stdout {
                println!("{}", formatted);
                return Ok(());
            }
            let path = Path::new(&query_file_path);
            let mut output = fs::File::create(path).expect("Failed to create file");
            output
                .write_all(formatted.as_bytes())
                .expect("Failed to write to file");
        }
        Err(err) => eprintln!(
            "{}",
            parser::render_error(&query_source_str, err, options.enable_color)
        ),
    }

    Ok(())
}
