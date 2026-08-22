use crate::ast;
use crate::ext::string;
use crate::typecheck;

use std::path::Path;

pub fn operation_name(operation: &ast::QueryOperation) -> String {
    match operation {
        ast::QueryOperation::Query => "Queried",
        ast::QueryOperation::Insert => "Added",
        ast::QueryOperation::Update => "Updated",
        ast::QueryOperation::Delete => "Deleted",
        ast::QueryOperation::Transaction => unreachable!("transactions are not watch operations"),
    }
    .to_string()
}

pub fn generate(
    files: &mut Vec<crate::filesystem::GeneratedFile<String>>,
    context: &typecheck::Context,
    base_out_dir: &Path,
) {
    let mut content = String::new();

    // Collect all watched operations first to check if any exist
    let mut watched_ops = Vec::new();
    for (_, table) in &context.tables {
        for watched_operation in ast::to_watched_operations(&table.record) {
            watched_ops.push((table.record.name.clone(), watched_operation));
        }
    }

    watched_ops.sort_by(|a, b| {
        let table_cmp = a.0.cmp(&b.0);
        if table_cmp != std::cmp::Ordering::Equal {
            return table_cmp;
        }
        operation_name(&a.1).cmp(&operation_name(&b.1))
    });

    content
        .push_str("\n\n// All tables that are currently being watched\nexport enum WatchedKind {");

    if watched_ops.is_empty() {
        // Add a placeholder value to make the enum valid
        content.push_str("\n  _None = \"_none\",");
    } else {
        for (table_name, watched_operation) in &watched_ops {
            content.push_str(&format!(
                "\n  {}{} = {},",
                table_name,
                operation_name(watched_operation),
                string::quote(&format!(
                    "{}{}",
                    table_name,
                    operation_name(watched_operation)
                ))
            ));
        }
    }
    content.push_str("\n}");

    if !watched_ops.is_empty() {
        for (table_name, watched_operation) in &watched_ops {
            let name = format!("{}{}", table_name, operation_name(watched_operation));
            content.push_str(&format!(
                "\n\nexport interface {} {{\n  kind: WatchedKind.{};\n  data: {};\n}}",
                name, name, "{}"
            ));
        }

        content.push_str("\n\nexport type Watched");
        let mut is_first = true;
        for (table_name, watched_operation) in &watched_ops {
            let name = format!("{}{}", table_name, operation_name(watched_operation));
            if is_first {
                content.push_str(&format!("\n    = {}", name));
                is_first = false;
            } else {
                content.push_str(&format!("\n    | {}", name));
            }
        }
        content.push_str("\n\n");
        write_runner(context, &mut content, &watched_ops);
    } else {
        // Generate minimal valid types when nothing is watched
        content.push_str("\n\nexport type Watched = {};");
    }

    files.push(crate::filesystem::generate_text_file(
        base_out_dir.join("watched.ts"),
        content,
    ));
}

fn write_runner(
    _context: &typecheck::Context,
    content: &mut String,
    watched_ops: &[(String, ast::QueryOperation)],
) {
    content.push_str("export interface Operations {\n");

    for (table_name, watched_operation) in watched_ops {
        content.push_str(&format!(
            "  {}{}: (env: any) => void;\n",
            string::decapitalize(table_name),
            operation_name(watched_operation)
        ));
    }

    content.push_str("}\n\n\n");

    // Executor
    content.push_str("export default function exec(\n");
    content.push_str("  env: any,\n");
    content.push_str("  operations: Operations,\n");
    content.push_str("  watched: Watched[],\n");
    content.push_str(") {\n");
    content.push_str("  watched.forEach((event) => {\n");
    content.push_str("    switch (event.kind) {\n");

    for (table_name, watched_operation) in watched_ops {
        content.push_str(&format!(
            "\n      case WatchedKind.{}{}:\n        {}",
            table_name,
            operation_name(watched_operation),
            &format!(
                "operations.{}{}(env);\n",
                string::decapitalize(table_name),
                operation_name(watched_operation)
            )
        ));

        content.push_str("        break;\n")
    }

    content.push_str("    }\n");
    content.push_str("  });\n");
    content.push_str("}\n");
}
