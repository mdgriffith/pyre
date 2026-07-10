use crate::ast;
use crate::ext::string;
use crate::filesystem;
use crate::filesystem::generate_text_file;
use crate::generate::typescript::common;
use crate::typecheck;
use std::collections::HashMap;
use std::path::Path;

pub fn generate_schema(
    _context: &typecheck::Context,
    _database: &ast::Database,
    _base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    let _ = files;
}

pub fn generate_queries(
    context: &typecheck::Context,
    _all_query_info: &HashMap<String, typecheck::QueryInfo>,
    query_list: &ast::QueryList,
    base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    let mut content = String::new();

    content.push_str("import type { Client } from '@libsql/client';\n");
    content.push_str("import { z } from 'zod';\n");
    content.push_str("import { seed as pyreSeed, type QueryMap, type QueryMetadata, type SeedResult, type SeedValidators } from '@pyre/server/query';\n");
    content.push_str("import * as Db from './core/decode';\n");
    content.push_str("import { schemaMetadata } from './core/schema';\n\n");

    for operation in &query_list.queries {
        if let ast::QueryDef::Query(q) = operation {
            let query_name = q.name.to_string();
            content.push_str(&format!(
                "import {{ meta as {} }} from './core/queries/metadata/{}';\n",
                query_name,
                crate::ext::string::decapitalize(&query_name)
            ));
            content.push_str(&format!(
                "import {{ sql as {}Sql, syncSql as {}SyncSql }} from './core/queries/sql/{}';\n",
                query_name,
                query_name,
                crate::ext::string::decapitalize(&query_name)
            ));
        }
    }

    content.push_str("\n");

    for operation in &query_list.queries {
        if let ast::QueryDef::Query(q) = operation {
            content.push_str(&format!(
                "const {}Query: QueryMetadata = {{\n  ...{},\n  sql: {}Sql,\n  ...(Array.isArray({}SyncSql) ? {{ syncSql: {}SyncSql }} : {{}})\n}};\n\n",
                q.name, q.name, q.name, q.name, q.name
            ));
        }
    }

    content.push_str("export const queries: QueryMap = {\n");
    let mut first = true;
    for operation in &query_list.queries {
        if let ast::QueryDef::Query(q) = operation {
            if !first {
                content.push_str(",\n");
            }
            content.push_str(&format!("  [{}.id]: {}Query", q.name, q.name));
            first = false;
        }
    }
    content.push_str("\n};\n\n");
    content.push_str(&to_seed_types(context));
    content.push_str(&to_seed_validators(context));
    content.push_str("export const seed = (db: Client, input: SeedInput): Promise<SeedResult> => pyreSeed(db, schemaMetadata, input as any, seedValidators);\n");

    files.push(generate_text_file(base_out_dir.join("server.ts"), content));
}

fn to_seed_validators(context: &typecheck::Context) -> String {
    let mut result = String::new();
    let mut tables: Vec<&typecheck::Table> = context.tables.values().collect();
    tables.sort_by(|a, b| {
        let a_table_name = ast::get_tablename(&a.record.name, &a.record.fields);
        let b_table_name = ast::get_tablename(&b.record.name, &b.record.fields);
        a_table_name.cmp(&b_table_name)
    });

    result.push_str("const seedValidators: SeedValidators = {\n");
    for table in &tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        result.push_str(&format!("  {}: {{\n", string::quote(&table_name)));
        for column in ast::collect_columns(&table.record.fields) {
            result.push_str(&format!(
                "    {}: {},\n",
                string::quote(&column.name),
                seed_column_validator(&column.type_, context)
            ));
        }
        result.push_str("  },\n");
    }
    result.push_str("};\n\n");

    result
}

fn seed_column_validator(type_: &ast::ColumnType, context: &typecheck::Context) -> String {
    match type_ {
        ast::ColumnType::String | ast::ColumnType::Date => "z.string()".to_string(),
        ast::ColumnType::Int | ast::ColumnType::Float | ast::ColumnType::IdInt { .. } => {
            "z.number()".to_string()
        }
        ast::ColumnType::ForeignKey {
            serialization_type: Some(ast::ConcreteSerializationType::IdUuid),
            ..
        } => "z.string()".to_string(),
        ast::ColumnType::ForeignKey { .. } => "z.number()".to_string(),
        ast::ColumnType::Bool => "Db.CoercedBool".to_string(),
        ast::ColumnType::DateTime => "Db.CoercedDate".to_string(),
        ast::ColumnType::Json => "Db.Json".to_string(),
        ast::ColumnType::JsonTyped(inner) => seed_column_validator(inner, context),
        ast::ColumnType::List(inner) => {
            format!("z.array({})", seed_column_validator(inner, context))
        }
        ast::ColumnType::Dict(inner) => {
            format!("z.record({})", seed_column_validator(inner, context))
        }
        ast::ColumnType::Nullable(inner) => {
            format!("{}.nullable()", seed_column_validator(inner, context))
        }
        ast::ColumnType::IdUuid { .. } => "z.string()".to_string(),
        ast::ColumnType::Custom(name) => {
            if context.types.contains_key(name) {
                format!("z.lazy(() => Db.{})", name)
            } else {
                "Db.Json".to_string()
            }
        }
    }
}

fn to_seed_types(context: &typecheck::Context) -> String {
    let mut result = String::new();
    let mut tables: Vec<&typecheck::Table> = context.tables.values().collect();
    tables.sort_by(|a, b| {
        let a_table_name = ast::get_tablename(&a.record.name, &a.record.fields);
        let b_table_name = ast::get_tablename(&b.record.name, &b.record.fields);
        a_table_name.cmp(&b_table_name)
    });

    result.push_str("type SeedConstructed<T> =\n");
    result.push_str("  T extends Array<infer U> ? Array<SeedConstructed<U>> :\n");
    result.push_str("  T extends object ? (T extends { _type: infer K extends string } ?\n");
    result.push_str(
        "    { _type: K } & { [P in Exclude<keyof T, '_type'>]?: SeedConstructed<T[P]> } :\n",
    );
    result.push_str("    { [P in keyof T]?: SeedConstructed<T[P]> }) :\n");
    result.push_str("  T;\n\n");

    for table in &tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let type_name = seed_row_type_name(&table_name);
        result.push_str(&format!("export type {} = {{\n", type_name));

        for column in ast::collect_columns(&table.record.fields) {
            let ts_type = seed_column_type(&column);
            result.push_str(&format!(
                "  {}?: {};\n",
                string::quote(&column.name),
                ts_type
            ));
        }

        let links = ast::collect_links(&table.record.fields);
        let primary_key_name = ast::get_primary_id_field_name(&table.record.fields);
        for link in links {
            let linked_table = typecheck::get_linked_table(context, &link).expect(&format!(
                "Failed to find linked table '{}' in context. This indicates a schema error.",
                link.foreign.table
            ));
            let linked_table_name =
                ast::get_tablename(&linked_table.record.name, &linked_table.record.fields);
            let linked_type_name = seed_row_type_name(&linked_table_name);
            let is_parent_to_child = link.local_ids.iter().all(|id| {
                primary_key_name
                    .as_ref()
                    .map(|pk| id == pk)
                    .unwrap_or(false)
            });
            let link_type = if is_parent_to_child {
                format!("{}[]", linked_type_name)
            } else {
                linked_type_name
            };
            result.push_str(&format!(
                "  {}?: {};\n",
                string::quote(&link.link_name),
                link_type
            ));
        }

        result.push_str("};\n\n");
    }

    result.push_str("export type SeedInput = {\n");
    for table in &tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        result.push_str(&format!(
            "  {}?: {}[];\n",
            string::quote(&table_name),
            seed_row_type_name(&table_name)
        ));
    }
    result.push_str("};\n\n");

    result
}

fn seed_row_type_name(table_name: &str) -> String {
    let mut result = String::from("Seed");
    let mut capitalize_next = true;
    for ch in table_name.chars() {
        if ch.is_ascii_alphanumeric() {
            if capitalize_next {
                result.push(ch.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(ch);
            }
        } else {
            capitalize_next = true;
        }
    }
    result.push_str("Row");
    result
}

fn seed_column_type(column: &ast::Column) -> String {
    let base = match &column.type_ {
        ast::ColumnType::Custom(name) => format!("SeedConstructed<Db.{}>", name),
        _ => seed_column_type_inner(&column.type_),
    };
    if column.nullable && !base.contains(" | null") {
        format!("{} | null", base)
    } else {
        base
    }
}

fn seed_column_type_inner(type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::DateTime => "number | string".to_string(),
        ast::ColumnType::Date => "string".to_string(),
        ast::ColumnType::Json => "unknown".to_string(),
        ast::ColumnType::JsonTyped(inner) => {
            format!("SeedConstructed<{}>", seed_column_type_inner(inner))
        }
        ast::ColumnType::List(inner) => {
            format!("Array<{}>", seed_column_type_inner(inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!("Record<string, {}>", seed_column_type_inner(inner))
        }
        ast::ColumnType::Nullable(inner) => {
            format!("{} | null", seed_column_type_inner(inner))
        }
        ast::ColumnType::Custom(name) => format!("Db.{}", name),
        _ => common::column_type_to_ts_type(type_, true).replace("Db.", "Db."),
    }
}
