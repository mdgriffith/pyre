use crate::ast;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

fn collect_tagged_types(
    database: &ast::Database,
) -> HashMap<String, (Vec<ast::Variant>, HashSet<String>)> {
    let mut types = HashMap::new();

    for schema in &database.schemas {
        for file in &schema.files {
            for def in &file.definitions {
                if let ast::Definition::Tagged { name, variants, .. } = def {
                    let mut deps = HashSet::new();

                    for variant in variants {
                        if let Some(fields) = &variant.fields {
                            for field in fields {
                                if let ast::Field::Column(col) = field {
                                    let mut type_names = Vec::new();
                                    col.type_.collect_custom_type_names(&mut type_names);
                                    deps.extend(type_names);
                                }
                            }
                        }
                    }

                    types.insert(name.clone(), (variants.clone(), deps));
                }
            }
        }
    }

    types
}

fn reaches_type(
    current: &str,
    target: &str,
    types: &HashMap<String, (Vec<ast::Variant>, HashSet<String>)>,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(current.to_string()) {
        return false;
    }

    types.get(current).is_some_and(|(_, deps)| {
        deps.iter().any(|dep| {
            types.contains_key(dep) && (dep == target || reaches_type(dep, target, types, visited))
        })
    })
}

/// Return every tagged type that belongs to a recursive dependency group.
pub fn recursive_type_names(database: &ast::Database) -> HashSet<String> {
    let types = collect_tagged_types(database);

    types
        .iter()
        .filter_map(|(name, (_, deps))| {
            let recursive = deps.iter().any(|dep| {
                dep == name
                    || (types.contains_key(dep)
                        && reaches_type(dep, name, &types, &mut HashSet::new()))
            });
            recursive.then(|| name.clone())
        })
        .collect()
}

/// Collect all type definitions and sort them by dependency order
pub fn sort_types_by_dependency(database: &ast::Database) -> Vec<(String, Vec<ast::Variant>)> {
    let mut types = collect_tagged_types(database);

    // Topological sort using Kahn's algorithm
    let mut sorted = Vec::new();
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();

    // Build graph and calculate in-degrees
    for (name, (_, deps)) in &types {
        in_degree.entry(name.clone()).or_insert(0);
        for dep in deps {
            if types.contains_key(dep) && dep != name {
                graph.entry(dep.clone()).or_default().push(name.clone());
                *in_degree.entry(name.clone()).or_insert(0) += 1;
            }
        }
    }

    for dependents in graph.values_mut() {
        dependents.sort();
    }

    // Start with nodes that have no dependencies
    let mut queue: BinaryHeap<Reverse<String>> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(name, _)| Reverse(name.clone()))
        .collect();

    while let Some(Reverse(name)) = queue.pop() {
        if let Some((variants, _)) = types.remove(&name) {
            sorted.push((name.clone(), variants));
        }

        if let Some(dependents) = graph.get(&name) {
            for dependent in dependents {
                if let Some(deg) = in_degree.get_mut(dependent) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(Reverse(dependent.clone()));
                    }
                }
            }
        }
    }

    // Handle any remaining types (cycles or missing deps)
    let mut remaining: Vec<(String, Vec<ast::Variant>)> = types
        .into_iter()
        .map(|(name, (variants, _))| (name, variants))
        .collect();
    remaining.sort_by(|a, b| a.0.cmp(&b.0));
    sorted.extend(remaining);

    sorted
}

pub fn column_type_to_ts_type(type_: &ast::ColumnType, qualify_custom: bool) -> String {
    match type_ {
        ast::ColumnType::String => "string".to_string(),
        ast::ColumnType::Int | ast::ColumnType::Float => "number".to_string(),
        ast::ColumnType::Bool => "boolean".to_string(),
        ast::ColumnType::DateTime => "Date".to_string(),
        ast::ColumnType::Date => "string".to_string(),
        ast::ColumnType::Json => "unknown".to_string(),
        ast::ColumnType::JsonTyped(inner) => column_type_to_ts_type(inner, qualify_custom),
        ast::ColumnType::List(inner) => {
            format!("Array<{}>", column_type_to_ts_type(inner, qualify_custom))
        }
        ast::ColumnType::Dict(inner) => {
            format!(
                "Record<string, {}>",
                column_type_to_ts_type(inner, qualify_custom)
            )
        }
        ast::ColumnType::Nullable(inner) => {
            format!("{} | null", column_type_to_ts_type(inner, qualify_custom))
        }
        ast::ColumnType::IdInt { .. } => "number".to_string(),
        ast::ColumnType::IdUuid { .. } => "string".to_string(),
        ast::ColumnType::ForeignKey {
            serialization_type: Some(ast::ConcreteSerializationType::IdUuid),
            ..
        } => "string".to_string(),
        ast::ColumnType::ForeignKey { .. } => "number".to_string(),
        ast::ColumnType::Custom(name) => {
            if qualify_custom {
                format!("Db.{}", name)
            } else {
                name.clone()
            }
        }
    }
}

pub fn column_type_to_zod_validator(type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::String => "z.string()".to_string(),
        ast::ColumnType::Int | ast::ColumnType::Float => "z.number()".to_string(),
        ast::ColumnType::Bool => "CoercedBool".to_string(),
        ast::ColumnType::DateTime => "CoercedDate".to_string(),
        ast::ColumnType::Date => "z.string()".to_string(),
        ast::ColumnType::Json => "Json".to_string(),
        ast::ColumnType::JsonTyped(inner) => column_type_to_zod_validator(inner),
        ast::ColumnType::List(inner) => {
            format!("z.array({})", column_type_to_zod_validator(inner))
        }
        ast::ColumnType::Dict(inner) => {
            format!(
                "z.record(z.string(), {})",
                column_type_to_zod_validator(inner)
            )
        }
        ast::ColumnType::Nullable(inner) => {
            format!("{}.nullable()", column_type_to_zod_validator(inner))
        }
        ast::ColumnType::IdInt { .. } => "z.number()".to_string(),
        ast::ColumnType::IdUuid { .. } => "z.string()".to_string(),
        ast::ColumnType::ForeignKey {
            serialization_type: Some(ast::ConcreteSerializationType::IdUuid),
            ..
        } => "z.string()".to_string(),
        ast::ColumnType::ForeignKey { .. } => "z.number()".to_string(),
        ast::ColumnType::Custom(name) => format!("z.lazy(() => {})", name),
    }
}

/// Generate the shared JSON type definition and schema
pub fn json_type_definition() -> &'static str {
    r#"// JSON values are decoded as unknown for type safety
export type Json = unknown;

export const Json: z.ZodType<Json> = z.unknown();

"#
}

/// Generate the coercion helpers
pub fn coercion_helpers() -> &'static str {
    r#"function invalidDate(ctx: z.RefinementCtx, message: string): never {
  ctx.addIssue({ code: 'custom', message });
  return z.NEVER;
}

function parseRfc3339(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})[Tt](?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?(?:[Zz]|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/.exec(value);
  if (!match) {
    return null;
  }

  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (month < 1 || month > 12 || day < 1 || day > daysInMonth[month - 1]) {
    return null;
  }

  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? null : parsed;
}

export const CoercedDate = z.union([z.number(), z.string(), z.date()]).transform((val, ctx) => {
  if (val instanceof Date) {
    return val;
  }

  if (typeof val === 'number') {
    if (!Number.isSafeInteger(val)) {
      return invalidDate(ctx, 'Expected whole Unix seconds');
    }
    const parsed = new Date(val * 1000);
    return Number.isNaN(parsed.getTime()) ? invalidDate(ctx, 'Unix seconds are outside the supported range') : parsed;
  }

  const trimmed = val.trim();
  if (/^[+-]?\d+$/.test(trimmed)) {
    const seconds = Number(trimmed);
    if (!Number.isSafeInteger(seconds)) {
      return invalidDate(ctx, 'Invalid Unix seconds');
    }
    const parsed = new Date(seconds * 1000);
    return Number.isNaN(parsed.getTime()) ? invalidDate(ctx, 'Unix seconds are outside the supported range') : parsed;
  }

  return parseRfc3339(trimmed) ?? invalidDate(ctx, 'Expected whole Unix seconds or an RFC 3339 timestamp');
});
export const CoercedBool = z.union([z.boolean(), z.number()]).transform((val) => typeof val === 'number' ? val !== 0 : val);

"#
}

/// Generate a tagged union decoder using Zod
pub fn generate_tagged_union(name: &str, variants: &[ast::Variant], recursive: bool) -> String {
    let mut result = String::new();

    let is_enum = variants.iter().all(|variant| variant.fields.is_none());

    if is_enum {
        let variants_as_literals = variants
            .iter()
            .map(|variant| format!("\"{}\"", variant.name))
            .collect::<Vec<String>>()
            .join(", ");

        result.push_str(&format!(
            "const {0}Enum = z.enum([{1}]);\n\n",
            name, variants_as_literals
        ));
        result.push_str(&format!(
            "export const {0} = z.preprocess((value) => {{\n",
            name
        ));
        result.push_str("  if (typeof value === 'string') {\n");
        result.push_str("    return value;\n");
        result.push_str("  }\n\n");
        result.push_str(
            "  if (value != null && typeof value === 'object' && !Array.isArray(value)) {\n",
        );
        result.push_str("    const record = value as Record<string, unknown>;\n");
        result.push_str("    if (typeof record._type === 'string') {\n");
        result.push_str("      return record._type;\n");
        result.push_str("    }\n");
        result.push_str("  }\n\n");
        result.push_str("  return value;\n");
        result.push_str(&format!("}}, {0}Enum);\n\n", name));
        result.push_str(&format!(
            "export type {} = z.infer<typeof {}>;\n\n",
            name, name
        ));
        return result;
    }

    if recursive {
        result.push_str(&format!("export type {} =\n", name));
        for variant in variants {
            result.push_str(&format!("  | {{ _type: \"{}\"", variant.name));
            if let Some(fields) = &variant.fields {
                for field in fields {
                    if let ast::Field::Column(col) = field {
                        let mut ts_type = column_type_to_ts_type(&col.type_, false);
                        if col.nullable {
                            ts_type.push_str(" | null");
                        }
                        result.push_str(&format!("; {}?: {}", col.name, ts_type));
                    }
                }
            }
            result.push_str(" }\n");
        }
        result.push_str(";\n\n");
    }

    let mut variant_field_names: Vec<String> = Vec::new();
    for variant in variants {
        if let Some(fields) = &variant.fields {
            for field in fields {
                if let ast::Field::Column(col) = field {
                    if !variant_field_names.contains(&col.name) {
                        variant_field_names.push(col.name.clone());
                    }
                }
            }
        }
    }
    let variant_field_names_literal = variant_field_names
        .iter()
        .map(|field_name| format!("\"{}\"", field_name))
        .collect::<Vec<String>>()
        .join(", ");

    let discriminated_annotation = if recursive {
        format!(": z.ZodType<{}>", name)
    } else {
        String::new()
    };
    result.push_str(&format!(
        "const {0}Discriminated{1} = z.discriminatedUnion(\"_type\", [\n",
        name, discriminated_annotation
    ));
    for variant in variants {
        result.push_str("  z.object({\n");
        result.push_str(&format!("    _type: z.literal(\"{}\"),\n", variant.name));

        if let Some(fields) = &variant.fields {
            for field in fields {
                if let ast::Field::Column(col) = field {
                    let validator = column_type_to_zod_validator(&col.type_);
                    let validator = if col.nullable {
                        format!("{}.nullish()", validator)
                    } else {
                        format!("{}.optional()", validator)
                    };
                    result.push_str(&format!("    {}: {},\n", col.name, validator));
                }
            }
        }
        result.push_str("  }),\n");
    }
    result.push_str("]);\n\n");

    let validator_annotation = if recursive {
        format!(": z.ZodType<{}>", name)
    } else {
        String::new()
    };
    result.push_str(&format!(
        "export const {0}{1} = z.preprocess((value) => {{\n",
        name, validator_annotation
    ));
    result
        .push_str("  if (value != null && typeof value === 'object' && !Array.isArray(value)) {\n");
    result.push_str("    const record = value as Record<string, unknown>;\n");
    result.push_str("    const normalized = { ...record };\n");
    result.push_str(&format!(
        "    const variantFields = [{}];\n",
        variant_field_names_literal
    ));
    result.push_str("    for (const fieldName of variantFields) {\n");
    result.push_str(
        "      const prefixedKey = Object.keys(normalized).find((key) => key.endsWith(`__${fieldName}`));\n",
    );
    result.push_str("      if (prefixedKey) {\n");
    result.push_str("        normalized[fieldName] = normalized[prefixedKey];\n");
    result.push_str("      }\n");
    result.push_str("    }\n\n");
    result.push_str("    return normalized;\n");
    result.push_str("  }\n\n");
    result.push_str("  return value;\n");
    result.push_str(&format!("}}, {0}Discriminated);\n\n", name));

    if !recursive {
        result.push_str(&format!(
            "export type {} = z.infer<typeof {}>;\n\n",
            name, name
        ));
    }

    result
}

/// Convert a type string to its Zod validator representation
pub fn type_to_zod_validator(type_str: &str, nullable: bool) -> String {
    let validator = column_type_to_zod_validator(&ast::ColumnType::from_str(type_str));

    if nullable {
        format!("{}.optional()", validator)
    } else {
        validator
    }
}

/// Convert a type string to its TypeScript type representation
pub fn type_to_ts_type(type_str: &str) -> String {
    column_type_to_ts_type(&ast::ColumnType::from_str(type_str), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, type_: ast::ColumnType) -> ast::Field {
        ast::Field::Column(ast::Column {
            name: name.to_string(),
            type_,
            nullable: false,
            directives: vec![],
            start: None,
            end: None,
            start_name: None,
            end_name: None,
            start_typename: None,
            end_typename: None,
            inline_comment: None,
        })
    }

    #[test]
    fn tagged_union_foreign_key_field_uses_primitive_decoder() {
        let variants = vec![ast::Variant {
            name: "InviteUser".to_string(),
            fields: Some(vec![column(
                "userId",
                ast::ColumnType::ForeignKey {
                    schema: None,
                    table: "User".to_string(),
                    field: "id".to_string(),
                    serialization_type: None,
                },
            )]),
            start: None,
            end: None,
            start_name: None,
            end_name: None,
            inline_comment: None,
        }];

        let generated = generate_tagged_union("InviteTarget", &variants, false);

        assert!(generated.contains("userId: z.number().optional()"));
        assert!(!generated.contains("userId: User.id.optional()"));
        assert!(!generated.contains("return { _type: value };"));
    }

    #[test]
    fn tagged_union_custom_field_uses_lazy_decoder_for_recursive_types() {
        let variants = vec![ast::Variant {
            name: "AttributeCustom".to_string(),
            fields: Some(vec![column(
                "fields",
                ast::ColumnType::Dict(Box::new(ast::ColumnType::Custom("Attribute".to_string()))),
            )]),
            start: None,
            end: None,
            start_name: None,
            end_name: None,
            inline_comment: None,
        }];

        let generated = generate_tagged_union("Attribute", &variants, true);

        assert!(
            generated.contains("fields: z.record(z.string(), z.lazy(() => Attribute)).optional()")
        );
    }
}
