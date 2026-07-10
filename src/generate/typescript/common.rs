use crate::ast;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Collect all type definitions and sort them by dependency order
pub fn sort_types_by_dependency(database: &ast::Database) -> Vec<(String, Vec<ast::Variant>)> {
    // Collect all tagged union types
    let mut types: HashMap<String, (Vec<ast::Variant>, HashSet<String>)> = HashMap::new();

    for schema in &database.schemas {
        for file in &schema.files {
            for def in &file.definitions {
                if let ast::Definition::Tagged { name, variants, .. } = def {
                    let mut deps = HashSet::new();

                    // Collect dependencies from all variant fields
                    for variant in variants {
                        if let Some(fields) = &variant.fields {
                            for field in fields {
                                if let ast::Field::Column(col) = field {
                                    let mut type_names = Vec::new();
                                    col.type_.collect_custom_type_names(&mut type_names);
                                    for type_name in type_names {
                                        deps.insert(type_name);
                                    }
                                }
                            }
                        }
                    }

                    types.insert(name.clone(), (variants.clone(), deps));
                }
            }
        }
    }

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
            format!("z.record({})", column_type_to_zod_validator(inner))
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
    r#"export const CoercedDate = z.union([z.number(), z.string(), z.date()]).transform((val) => {
  if (val instanceof Date) {
    return val;
  }

  if (typeof val === 'number') {
    return new Date(val * 1000);
  }

  const trimmed = val.trim();
  if (trimmed.length > 0) {
    const asNumber = Number(trimmed);
    if (!Number.isNaN(asNumber)) {
      return new Date(asNumber * 1000);
    }
  }

  const parsed = new Date(val);
  if (Number.isNaN(parsed.getTime())) {
    throw new Error(`Invalid date value: ${val}`);
  }

  return parsed;
});
export const CoercedBool = z.union([z.boolean(), z.number()]).transform((val) => typeof val === 'number' ? val !== 0 : val);

"#
}

/// Generate a tagged union decoder using Zod
pub fn generate_tagged_union(name: &str, variants: &[ast::Variant]) -> String {
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

    result.push_str(&format!(
        "const {0}Discriminated = z.discriminatedUnion(\"_type\", [\n",
        name
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

    result.push_str(&format!(
        "export const {0} = z.preprocess((value) => {{\n",
        name
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

    // Type inference
    result.push_str(&format!(
        "export type {} = z.infer<typeof {}>;\n\n",
        name, name
    ));

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

        let generated = generate_tagged_union("InviteTarget", &variants);

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

        let generated = generate_tagged_union("Attribute", &variants);

        assert!(generated.contains("fields: z.record(z.lazy(() => Attribute)).optional()"));
    }
}
