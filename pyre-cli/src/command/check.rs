use serde_json;
use std::io::{self, Read};

use super::shared::{parse_database_schemas, FileError, Options};
use pyre::error;
use pyre::filesystem;
use pyre::generated_queries;
use pyre::parser;
use pyre::typecheck;

pub fn check(options: &Options, _files: Vec<String>, json: bool) -> io::Result<()> {
    let paths = crate::filesystem::collect_filepaths(&options.in_dir)?;
    let schema_count = paths.schema_files.len();
    let query_file_count = paths.query_files.len();

    // Build a map of schema filepaths to their contents for error formatting
    let mut schema_file_contents: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (_, schema_files) in paths.schema_files.iter() {
        for schema_file in schema_files.iter() {
            schema_file_contents.insert(schema_file.path.clone(), schema_file.content.clone());
        }
    }
    if let Some(session_file) = &paths.session_file {
        schema_file_contents.insert(session_file.path.clone(), session_file.content.clone());
    }

    match run_check(paths, options.enable_color) {
        Ok(errors) => {
            let has_errors = !errors.is_empty();
            if json {
                let mut formatted_errors = Vec::new();
                for file_error in errors {
                    for error in &file_error.errors {
                        formatted_errors.push(error::format_json(error));
                    }
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&formatted_errors).unwrap()
                );
            } else {
                for file_error in errors {
                    for err in &file_error.errors {
                        // Use schema file contents if error is from a schema file, otherwise use query file contents
                        let source = schema_file_contents
                            .get(&err.filepath)
                            .map(|s| s.as_str())
                            .unwrap_or(&file_error.source);
                        let formatted_error =
                            error::format_error(source, err, options.enable_color);
                        eprintln!("{}", formatted_error);
                    }
                }
            }
            if has_errors {
                std::process::exit(1);
            } else if !json {
                println!(
                    "Success: checked {} schema(s) and {} query files",
                    schema_count, query_file_count,
                );
            }
        }
        Err(e) => {
            eprintln!("Error checking files: {}", e);
            std::process::exit(1);
        }
    }
    Ok(())
}

fn run_check(paths: filesystem::Found, enable_color: bool) -> io::Result<Vec<FileError>> {
    let schema = parse_database_schemas(&paths, enable_color)?;
    let mut all_file_errors = Vec::new();

    match typecheck::check_schema(&schema) {
        Err(errors) => {
            all_file_errors.push(FileError {
                source: "schema.pyre".to_string(),
                errors: errors,
            });
        }
        Ok(mut context) => {
            let mut all_queries = pyre::ast::QueryList {
                queries: Vec::new(),
            };
            for query_file_path in paths.query_files {
                let mut file_errors = Vec::new();
                let mut query_file = std::fs::File::open(query_file_path.clone())?;
                let mut query_source_str = String::new();
                query_file.read_to_string(&mut query_source_str)?;

                context.current_filepath = query_file_path.clone();
                match parser::parse_query(&query_file_path, &query_source_str) {
                    Ok(query_list) => {
                        all_queries.queries.extend(query_list.queries.clone());

                        let collision_errors =
                            generated_queries::validate_generated_crud_name_collisions(
                                &query_list,
                                &context,
                            );
                        if !collision_errors.is_empty() {
                            file_errors.extend(collision_errors);
                        }

                        let typecheck_result = typecheck::check_queries(&query_list, &mut context);

                        match typecheck_result {
                            Ok(_) => {}
                            Err(errors) => {
                                file_errors.extend(errors);
                            }
                        }
                    }
                    Err(err) => {
                        if let Some(parsing_error) = parser::convert_parsing_error(err) {
                            file_errors.push(parsing_error);
                        }
                    }
                }

                if !file_errors.is_empty() {
                    all_file_errors.push(FileError {
                        source: query_source_str,
                        errors: file_errors,
                    });
                }
            }

            if all_file_errors.is_empty() {
                generated_queries::append_generated_crud_queries(&mut all_queries, &context);
                if let Err(errors) = typecheck::check_queries(&all_queries, &context) {
                    all_file_errors.push(FileError {
                        source: String::new(),
                        errors,
                    });
                }
            }
        }
    }

    Ok(all_file_errors)
}
