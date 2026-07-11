pub mod watched;
use crate::ast;
use crate::filesystem;
use crate::filesystem::generate_text_file;
use crate::generate::typescript::common;
use crate::typecheck;

use std::path::Path;

const DB_ENGINE: &str = include_str!("./static/typescript/db.ts");

/// Collect all unique brands from ID columns in the database
fn collect_brands(database: &ast::Database) -> Vec<String> {
    use std::collections::HashSet;
    let mut brands = HashSet::new();

    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                if let ast::Definition::Record { fields, .. } = definition {
                    for field in fields {
                        if let ast::Field::Column(column) = field {
                            // Check if this is an ID type with a brand (non-empty table name)
                            if let Some(brand) = column.type_.table_name() {
                                brands.insert(brand.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    let mut brands_vec: Vec<String> = brands.into_iter().collect();
    brands_vec.sort();
    brands_vec
}

/// Convert a column to its TypeScript type representation
/// For ID types with brands, generates branded types like `UserId` or `string & Post`
fn column_to_ts_type(column: &ast::Column) -> String {
    match &column.type_ {
        ast::ColumnType::IdInt { table } => {
            if !table.is_empty() {
                format!("{}Id", table)
            } else {
                "number".to_string()
            }
        }
        ast::ColumnType::IdUuid { table } => {
            if !table.is_empty() {
                format!("string & {}", table)
            } else {
                "string".to_string()
            }
        }
        _ => to_ts_typename(false, &column.type_.to_string()),
    }
}

/// Generate all typescript files
pub fn generate(
    context: &typecheck::Context,
    database: &ast::Database,
    base_out_dir: &Path,
    files: &mut Vec<filesystem::GeneratedFile<String>>,
) {
    // Generate core files
    files.push(generate_text_file(
        base_out_dir.join("db.ts"),
        DB_ENGINE.to_string(),
    ));

    if let Some(config_ts) = to_env(context, database) {
        files.push(generate_text_file(
            base_out_dir.join("db/env.ts"),
            config_ts,
        ));
    }

    files.push(generate_text_file(
        base_out_dir.join("db/data.ts"),
        schema(database),
    ));
    files.push(generate_text_file(
        base_out_dir.join("db/decode.ts"),
        to_schema_decoders(database),
    ));

    // Generate watched.ts file
    watched::generate(files, context, base_out_dir);
}

pub fn schema(database: &ast::Database) -> String {
    let mut result = String::new();

    // Collect all unique brands from ID columns
    let brands = collect_brands(database);

    // Generate phantom type definitions using the brand pattern
    if !brands.is_empty() {
        result.push_str("// Branded ID types using intersection types\n");
        for brand in &brands {
            result.push_str(&format!(
                "type {} = {{ readonly __brand: '{}' }}\n",
                brand, brand
            ));
        }
        result.push_str("\n");

        // Generate type aliases for each brand
        result.push_str("// ID type aliases\n");
        for brand in &brands {
            result.push_str(&format!("type {}Id = number & {}\n", brand, brand));
        }
        result.push_str("\n\n");
    }

    for schema in &database.schemas {
        result.push_str("\n\n");
        for file in &schema.files {
            for definition in &file.definitions {
                result.push_str(&to_string_definition(definition));
            }
        }
    }
    result
}

fn to_string_definition(definition: &ast::Definition) -> String {
    match definition {
        ast::Definition::Lines { count } => {
            if *count > 2 {
                "\n\n".to_string()
            } else {
                "\n".repeat(*count as usize)
            }
        }
        ast::Definition::Comment { .. } => "".to_string(),
        ast::Definition::SyncMode(_) => "".to_string(),
        ast::Definition::Session(_) => "".to_string(),
        ast::Definition::Tagged { name, variants, .. } => {
            let mut result = format!("type {} =", name);

            for variant in variants {
                result.push_str("\n");
                result.push_str(&to_string_variant(2, variant));
            }
            result.push_str(";\n\n");
            result
        }
        ast::Definition::Record { name, fields, .. } => to_type_alias(name, fields),
    }
}

fn to_type_alias(name: &str, fields: &Vec<ast::Field>) -> String {
    let mut result = format!("type {} = {{\n  ", name);

    let mut is_first = true;
    for field in fields {
        if ast::is_column_space(field) {
            continue;
        }

        result.push_str(&to_string_field(is_first, 2, &field));

        if is_first & ast::is_column(field) {
            is_first = false;
        }
    }
    result.push_str("};\n");
    result
}

fn to_string_variant(indent_size: usize, variant: &ast::Variant) -> String {
    let prefix = " | ";

    match &variant.fields {
        Some(fields) => {
            let indent = " ".repeat(indent_size + 4);

            let mut result = format!(
                " {}{{\n{}\"_type\": {};\n{}",
                prefix,
                indent,
                crate::ext::string::quote(&variant.name),
                indent
            );

            let mut is_first_field = true;
            for field in fields {
                result.push_str(&to_string_field(is_first_field, indent_size + 4, &field));
                is_first_field = false
            }
            result.push_str("    }");
            result
        }
        None => format!(
            " {}{{ \"_type\": {} }}",
            prefix,
            crate::ext::string::quote(&variant.name)
        ),
    }
}

fn to_string_field(is_first: bool, indent: usize, field: &ast::Field) -> String {
    match field {
        ast::Field::ColumnLines { count } => {
            if *count > 2 {
                "\n\n".to_string()
            } else {
                "\n".repeat(*count as usize)
            }
        }
        ast::Field::Column(column) => to_string_column(is_first, indent, column),
        ast::Field::ColumnComment { .. } => "".to_string(),
        ast::Field::FieldDirective(_) => "".to_string(),
    }
}

fn to_string_column(is_first: bool, indent: usize, column: &ast::Column) -> String {
    let type_str = column_to_ts_type(column);
    if is_first {
        return format!(
            "{}: {};\n",
            crate::ext::string::quote(&column.name),
            type_str
        );
    } else {
        let spaces = " ".repeat(indent);
        return format!(
            "{}{}: {};\n",
            spaces,
            crate::ext::string::quote(&column.name),
            type_str
        );
    }
}

// DECODE
//

pub fn to_schema_decoders(database: &ast::Database) -> String {
    let mut result = String::new();

    result.push_str("import { z } from 'zod';");

    result.push_str("\n\n");

    result.push_str(common::coercion_helpers());
    result.push_str(common::json_type_definition());

    // Generate types in dependency order
    let sorted_types = common::sort_types_by_dependency(database);
    for (name, variants) in sorted_types {
        result.push_str(&common::generate_tagged_union(&name, &variants));
    }

    // Generate non-tagged definitions (Comments, Lines, Session, Records)
    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                if !matches!(definition, ast::Definition::Tagged { .. }) {
                    result.push_str(&to_decoder_definition(definition));
                }
            }
        }
    }
    result
}

fn to_decoder_definition(definition: &ast::Definition) -> String {
    match definition {
        ast::Definition::Lines { count } => {
            if *count > 2 {
                "\n\n".to_string()
            } else {
                "\n".repeat(*count as usize)
            }
        }
        ast::Definition::Comment { .. } => "".to_string(),
        ast::Definition::SyncMode(_) => "".to_string(),
        ast::Definition::Session(_) => "".to_string(),
        ast::Definition::Tagged { name, variants, .. } => {
            // Use the shared generate_tagged_union function for consistency
            common::generate_tagged_union(name, variants)
        }
        ast::Definition::Record { .. } => "".to_string(),
    }
}

pub fn literal_quote(s: &str) -> String {
    format!("`{}`", s)
}

fn to_ts_typename(qualified: bool, type_: &str) -> String {
    common::column_type_to_ts_type(&ast::ColumnType::from_str(type_), qualified)
}

pub fn to_env(context: &typecheck::Context, database: &ast::Database) -> Option<String> {
    let mut result = String::new();

    result.push_str("import { z } from 'zod';\n");
    result.push_str("import * as Db from './decode';\n");
    let session = database
        .schemas
        .iter()
        .find_map(|schema| schema.session.clone())
        .unwrap_or_else(|| ast::default_session_details());

    if database.schemas.len() == 1 {
        result.push_str("import type { Config } from '@libsql/client';\n");
        result.push_str("export type { Config };\n");
    } else {
        result.push_str("import type { Config as LibSqlConfig } from '@libsql/client';\n");
    }

    // Generate session types
    result.push_str("\n\nexport const Session = z.object({\n");
    for field in &session.fields {
        match field {
            ast::Field::Column(column) => {
                let validator = session_validator(context, &column.type_);
                let validator = if column.nullable {
                    format!("{}.optional()", validator)
                } else {
                    validator.to_string()
                };
                result.push_str(&format!("  {}: {},\n", column.name, validator));
            }
            _ => (),
        }
    }
    result.push_str("});\n\n");
    result.push_str("export type Session = z.infer<typeof Session>;\n\n");

    // Database namespaces
    if database.schemas.len() > 1 {
        result.push_str("export interface DatabaseConfig extends LibSqlConfig {\n");
        result.push_str("  /** Unique identifier for the database */\n");
        result.push_str("  id: string;\n");
        result.push_str("}\n");

        result.push_str("export interface Config {\n");
        for schema in &database.schemas {
            result.push_str(&format!("  {}?: DatabaseConfig;\n", schema.namespace));
        }
        result.push_str("}\n\n");
    }

    if database.schemas.len() == 1 {
        result.push_str("export type DatabaseKey = string;\n");
    } else {
        result.push_str("export enum DatabaseKey {\n");
        for schema in &database.schemas {
            result.push_str(&format!(
                "  {} = '{}',\n",
                schema.namespace, schema.namespace
            ));
        }
        result.push_str("}\n");
    }

    if database.schemas.len() == 1 {
        result.push_str(
            "export const to_libSql_config = (env: Config, primary: DatabaseKey): Config | undefined => {\n",
        );
        result.push_str("  return env\n");
    } else {
        result.push_str(
            "export const to_libSql_config = (env: Config, primary: DatabaseKey): LibSqlConfig | undefined => {\n",
        );
        result.push_str("  return env[primary]\n");
    }
    result.push_str("};\n\n");

    Some(result)
}

fn session_validator(context: &typecheck::Context, type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String | ast::ColumnType::Date => "z.string()".to_string(),
        ast::ColumnType::Int | ast::ColumnType::Float | ast::ColumnType::IdInt { .. } => {
            "z.number()".to_string()
        }
        ast::ColumnType::IdUuid { .. } => "z.string()".to_string(),
        ast::ColumnType::ForeignKey {
            serialization_type: Some(serialization_type),
            ..
        } => session_validator_for_serialization_type(serialization_type),
        // Parsed but untypechecked foreign keys retain Pyre's historical integer fallback.
        ast::ColumnType::ForeignKey { .. } => "z.number()".to_string(),
        ast::ColumnType::Bool => "z.boolean()".to_string(),
        ast::ColumnType::DateTime => "z.union([z.date(), z.string(), z.number()])".to_string(),
        ast::ColumnType::Json => "z.unknown()".to_string(),
        ast::ColumnType::JsonTyped(inner) => session_validator(context, inner),
        ast::ColumnType::List(inner) => format!("z.array({})", session_validator(context, inner)),
        ast::ColumnType::Dict(inner) => format!("z.record({})", session_validator(context, inner)),
        ast::ColumnType::Nullable(inner) => {
            format!("{}.nullable()", session_validator(context, inner))
        }
        ast::ColumnType::Custom(name) if context.types.contains_key(name) => format!("Db.{}", name),
        ast::ColumnType::Custom(_) => "z.unknown()".to_string(),
    }
}

fn session_validator_for_serialization_type(type_: &ast::ConcreteSerializationType) -> String {
    match type_ {
        ast::ConcreteSerializationType::Integer
        | ast::ConcreteSerializationType::Real
        | ast::ConcreteSerializationType::IdInt => "z.number()".to_string(),
        ast::ConcreteSerializationType::Text
        | ast::ConcreteSerializationType::Date
        | ast::ConcreteSerializationType::IdUuid => "z.string()".to_string(),
        ast::ConcreteSerializationType::DateTime => {
            "z.union([z.date(), z.string(), z.number()])".to_string()
        }
        ast::ConcreteSerializationType::Blob
        | ast::ConcreteSerializationType::VectorBlob { .. }
        | ast::ConcreteSerializationType::JsonB => "z.unknown()".to_string(),
    }
}
