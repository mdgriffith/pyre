use std::collections::HashMap;
use std::path::Path;

use crate::ast;
use crate::generate;
use crate::typecheck;

pub mod client;
pub mod manifest;
pub mod server;
pub mod sql;
pub mod to_string;
pub mod typealias;
pub mod typescript;

pub fn generate_schema(
    context: &typecheck::Context,
    database: &ast::Database,
    files: &mut Vec<crate::filesystem::GeneratedFile<String>>,
) {
    write_client_schema(database, files);
    let typescript_core_dir = Path::new("typescript/core");
    let typescript_dir = Path::new("typescript");
    generate::typescript::core::generate_schema(context, database, typescript_core_dir, files);
    generate::typescript::targets::server::generate_schema(
        context,
        database,
        typescript_dir,
        files,
    );
    generate::typescript::targets::simple::generate_schema(database, typescript_dir, files);
    generate::server::rust::generate_schema(database, Path::new("rust"), files);
    generate::manifest::generate_schema(context, files);
}

// CLIENT

fn write_client_schema(
    database: &ast::Database,
    files: &mut Vec<crate::filesystem::GeneratedFile<String>>,
) {
    // Target directory is
    // {base_out_dir}/client/{lang}
    // Use relative path to avoid duplication when joined with base_path in write_generated_files
    let out_dir = Path::new("client/elm");
    // out_dir is
    // client/{lang} (relative path)
    generate::client::elm::generate(out_dir, database, files)
}

// WRITE QUERIES

pub fn write_queries(
    context: &typecheck::Context,
    query_list: &ast::QueryList,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    files: &mut Vec<crate::filesystem::GeneratedFile<String>>,
) {
    write_client_queries(context, query_list, all_query_info, files);
    let typescript_core_dir = Path::new("typescript/core");
    let typescript_dir = Path::new("typescript");
    generate::typescript::core::generate_queries(
        context,
        all_query_info,
        query_list,
        typescript_core_dir,
        files,
    );
    generate::typescript::targets::server::generate_queries(
        context,
        all_query_info,
        query_list,
        typescript_dir,
        files,
    );
    generate::typescript::targets::simple::generate_queries(
        context,
        all_query_info,
        query_list,
        typescript_dir,
        files,
    );
    generate::server::rust::generate_queries(context, query_list, Path::new("rust"), files);
    generate::manifest::generate_queries(context, query_list, all_query_info, files);
}

// CLIENT

fn write_client_queries(
    context: &typecheck::Context,
    query_list: &ast::QueryList,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    files: &mut Vec<crate::filesystem::GeneratedFile<String>>,
) {
    // Create relative path: client/{lang} (relative to base_out_dir)
    let out_dir = Path::new("client/elm");
    generate::client::elm::generate_queries(context, all_query_info, query_list, out_dir, files)
}
