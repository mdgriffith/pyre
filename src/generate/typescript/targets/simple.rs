use crate::ast;
use crate::ext::string;
use crate::filesystem;
use crate::filesystem::generate_text_file;
use crate::typecheck;
use std::collections::HashMap;
use std::path::Path;

pub fn generate_schema(
    database: &ast::Database,
    base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    files.push(generate_text_file(
        base_out_dir.join("types.ts"),
        crate::generate::server::typescript::schema(database),
    ));
}

pub fn generate_queries(
    _context: &typecheck::Context,
    all_query_info: &HashMap<String, typecheck::QueryInfo>,
    query_list: &ast::QueryList,
    base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    let mut query_names: Vec<String> = Vec::new();
    for operation in &query_list.queries {
        if let ast::QueryDef::Query(q) = operation {
            if all_query_info.contains_key(&q.name) {
                query_names.push(q.name.clone());
            } else {
                eprintln!(
                    "Warning: Query '{}' was found but not in typecheck results. Skipping.",
                    q.name
                );
            }
        }
    }

    files.push(generate_text_file(
        base_out_dir.join("run.ts"),
        generate_run_file(&query_names),
    ));
}

fn generate_run_file(query_names: &[String]) -> String {
    let mut result = String::new();
    result.push_str("import { toRunner } from '@pyre/server/runtime/runner';\n");
    result.push_str("export type { Session } from './core/decode';\n");

    for query_name in query_names {
        let file_name = string::decapitalize(query_name);
        result.push_str(&format!(
            "import type {{ Input as {0}InputType, Result as {0}ResultType }} from './core/queries/metadata/{1}';\n",
            query_name, file_name
        ));
        result.push_str(&format!(
            "import {{ meta as {0}Meta }} from './core/queries/metadata/{1}';\n",
            query_name, file_name
        ));
        result.push_str(&format!(
            "import {{ sql as {0}Sql }} from './core/queries/sql/{1}';\n",
            query_name, file_name
        ));
    }

    result.push_str("\n");

    for query_name in query_names {
        result.push_str(&format!(
            "export type {}Input = {}InputType;\n",
            query_name, query_name
        ));
        result.push_str(&format!(
            "export type {}Result = {}ResultType;\n",
            query_name, query_name
        ));
        result.push_str(&format!(
            "export const {0} = toRunner<{0}Input, {0}Result>({0}Meta, {0}Sql);\n\n",
            query_name
        ));
    }

    result
}
