use std::fs;
use std::io::{self, Read};
use std::path::Path;

use super::shared::{display_path, parse_database_schemas, Options};
use pyre::ast;
use pyre::error;
use pyre::filesystem;
use pyre::generate;
use pyre::generated_queries;
use pyre::parser;
use pyre::typecheck;

pub fn generate(options: &Options, out: &str) -> io::Result<()> {
    execute(
        options,
        crate::filesystem::collect_filepaths(&options.in_dir)?,
        Path::new(out),
    )
}

fn clear(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn execute(options: &Options, paths: filesystem::Found, out_dir: &Path) -> io::Result<()> {
    let schema = parse_database_schemas(&paths, options.enable_color)?;

    // Build a map of schema filepaths to their contents for error formatting
    let mut schema_file_contents: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (_, schema_files) in paths.schema_files.iter() {
        for schema_file in schema_files.iter() {
            schema_file_contents.insert(schema_file.path.clone(), schema_file.content.clone());
        }
    }

    match typecheck::check_schema(&schema) {
        Err(error_list) => {
            error::report_and_exit(error_list, &paths, options.enable_color);
        }
        Ok(mut context) => {
            // Clear the generated folder
            clear(&out_dir)?;

            // Ensure dir is present
            crate::filesystem::create_dir_if_not_exists(&out_dir)?;

            let mut files: Vec<filesystem::GeneratedFile<String>> = Vec::new();

            // Generate schema files
            generate::generate_schema(&context, &schema, &mut files);

            // Collect all queries from all files first
            let mut all_queries = ast::QueryList {
                queries: Vec::new(),
            };
            let mut all_query_info_combined: std::collections::HashMap<
                String,
                typecheck::QueryInfo,
            > = std::collections::HashMap::new();

            for query_file_path in paths.query_files {
                let mut query_file = fs::File::open(query_file_path.clone())?;
                let mut query_source_str = String::new();
                query_file.read_to_string(&mut query_source_str)?;

                context.current_filepath = query_file_path.clone();
                match parser::parse_query(&query_file_path, &query_source_str) {
                    Ok(query_list) => {
                        let collision_errors =
                            generated_queries::validate_generated_crud_name_collisions(
                                &query_list,
                                &context,
                            );
                        if !collision_errors.is_empty() {
                            let mut errors = String::new();
                            for err in collision_errors {
                                let formatted_error = error::format_error(
                                    &query_source_str,
                                    &err,
                                    options.enable_color,
                                );
                                errors.push_str(&formatted_error);
                            }
                            eprintln!("{}", errors);
                            std::process::exit(1);
                        }

                        // Typecheck
                        let typecheck_result = typecheck::check_queries(&query_list, &context);

                        match typecheck_result {
                            Ok(all_query_info) => {
                                // Collect queries and query info
                                all_queries.queries.extend(query_list.queries);
                                all_query_info_combined.extend(all_query_info);
                            }
                            Err(error_list) => {
                                let mut errors = "".to_string();
                                for err in error_list {
                                    // Use schema file contents if error is from a schema file, otherwise use query file contents
                                    let source = schema_file_contents
                                        .get(&err.filepath)
                                        .map(|s| s.as_str())
                                        .unwrap_or(&query_source_str);
                                    let formatted_error =
                                        error::format_error(source, &err, options.enable_color);
                                    errors.push_str(&formatted_error);
                                }

                                eprintln!("{}", errors);
                                std::process::exit(1);
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!(
                            "{}",
                            parser::render_error(&query_source_str, err, options.enable_color)
                        );
                        std::process::exit(1);
                    }
                }
            }

            generated_queries::append_generated_crud_queries(&mut all_queries, &context);
            let all_query_info_combined = match typecheck::check_queries(&all_queries, &context) {
                Ok(info) => info,
                Err(error_list) => {
                    let mut errors = String::new();
                    for err in error_list {
                        let source = schema_file_contents
                            .get(&err.filepath)
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        let formatted_error =
                            error::format_error(source, &err, options.enable_color);
                        errors.push_str(&formatted_error);
                    }
                    eprintln!("{}", errors);
                    std::process::exit(1);
                }
            };

            // Generate all queries at once (including runner file)
            if !all_queries.queries.is_empty() {
                generate::write_queries(
                    &context,
                    &all_queries,
                    &all_query_info_combined,
                    &mut files,
                );
            }

            let file_count = files.len();
            crate::filesystem::write_generated_files(out_dir, files)?;
            println!(
                "Generated {} {} from {} into {}.",
                file_count,
                if file_count == 1 { "file" } else { "files" },
                display_path(options.in_dir),
                display_path(out_dir),
            );
        }
    }

    Ok(())
}
