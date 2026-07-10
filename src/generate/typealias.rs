use crate::ast;
use crate::ext::string;
use crate::typecheck;
pub struct TypeFormatter {
    pub to_comment: Box<dyn Fn(&str) -> String>,
    pub to_type_def_start: Box<dyn Fn(&str) -> String>,
    pub to_field: Box<dyn Fn(&str, &str, FieldMetadata) -> String>,
    pub to_type_def_end: Box<dyn Fn() -> String>,
    pub to_field_separator: Box<dyn Fn(bool) -> String>,
}

#[derive(Clone, Copy)]
pub struct FieldMetadata {
    pub is_link: bool,
    pub is_optional: bool,
    /// If true, this relationship should be an array (one-to-many).
    /// If false and is_link is true, it's many-to-one or one-to-one (optional object).
    pub is_array_relationship: bool,
}

/// Generates type alias definitions for query return types using the provided formatting functions
///
/// # Example
/// ```rust,no_run
/// use pyre::generate::typealias::{TypeFormatter, FieldMetadata, return_data_aliases};
/// use pyre::typecheck;
/// use pyre::ast;
///
/// # let context: &typecheck::Context = todo!();
/// # let query: &ast::Query = todo!();
/// let elm_formatter = TypeFormatter {
///     to_comment: Box::new(|s| format!("{{-| {} -}}\n", s)),
///     to_type_def_start: Box::new(|name| format!("type alias {} =\n", name)),
///     to_field: Box::new(|name, type_, metadata: FieldMetadata| {
///         let type_str = type_.to_string();
///         format!("    {} : {}", name, type_str)
///     }),
///     to_type_def_end: Box::new(|| "    }\n".to_string()),
///     to_field_separator: Box::new(|_| ",\n".to_string()),
/// };
///
/// let mut result = String::new();
/// return_data_aliases(context, query, &mut result, &elm_formatter);
/// ```
pub fn return_data_aliases(
    context: &typecheck::Context,
    query: &ast::Query,
    result: &mut String,
    formatter: &TypeFormatter,
) {
    // Add comment and type definition start
    result.push_str(&(formatter.to_comment)("The Return Data!"));

    // Children aliases
    for field in &query.fields {
        match field {
            ast::TopLevelQueryField::Field(query_field) => {
                match context.tables.get(&query_field.name) {
                    Some(table) => {
                        to_query_type_alias(
                            context,
                            &table.record,
                            "",
                            query_field,
                            query.operation.clone(),
                            formatter,
                            result,
                        );
                    }
                    None => {
                        eprintln!("Error: Table '{}' referenced in query was not found in typecheck context. This should not happen after successful typechecking. Skipping type alias generation.", query_field.name);
                    }
                }
            }
            ast::TopLevelQueryField::Lines { .. } => {}
            ast::TopLevelQueryField::Comment { .. } => {}
        }
    }

    // Global Return Data Alias
    result.push_str(&(formatter.to_type_def_start)(
        &crate::ext::string::capitalize("ReturnData"),
    ));

    let last_field_index = query
        .fields
        .iter()
        .rposition(|field| matches!(field, ast::TopLevelQueryField::Field(_)))
        .unwrap_or(0);

    for (i, field) in query.fields.iter().enumerate() {
        let is_last = i == last_field_index;
        match field {
            ast::TopLevelQueryField::Field(query_field) => {
                let field_name: String = ast::get_aliased_name(query_field);

                result.push_str(&(formatter.to_field)(
                    &crate::ext::string::decapitalize(&field_name),
                    &string::capitalize(&field_name),
                    FieldMetadata {
                        is_link: true,
                        is_optional: false,
                        is_array_relationship: true, // Top-level query fields are always arrays
                    },
                ));

                result.push_str(&(formatter.to_field_separator)(is_last));
            }
            _ => {}
        }
    }

    result.push_str(&(formatter.to_type_def_end)());
    result.push_str("\n\n");
}

fn get_name(alias_stack: &str, field_name: &str) -> String {
    if alias_stack.is_empty() {
        crate::ext::string::capitalize(field_name)
    } else {
        format!(
            "{}_{}",
            alias_stack,
            crate::ext::string::capitalize(field_name)
        )
    }
}

fn to_query_type_alias(
    context: &typecheck::Context,
    table: &ast::RecordDetails,
    alias_stack: &str,
    query_field: &ast::QueryField,
    operation: ast::QueryOperation,
    formatter: &TypeFormatter,
    //
    result: &mut String,
) {
    let child_alias_stack = push_alias_stack(query_field, alias_stack);
    // Children first
    let fields = &ast::collect_query_fields(&query_field.fields);
    for field in fields {
        if operation == ast::QueryOperation::Insert {
            continue;
        }

        if field.fields.is_empty() {
            continue;
        }

        let fieldname_match = table
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(f, &field.name));

        match fieldname_match {
            Some(ast::Field::FieldDirective(ast::FieldDirective::Link(link))) => {
                if let Some(link_table) = typecheck::get_linked_table(context, &link) {
                    to_query_type_alias(
                        context,
                        &link_table.record,
                        &child_alias_stack,
                        field,
                        operation.clone(),
                        formatter,
                        result,
                    );
                }
            }
            _ => continue,
        }
    }

    // Local Return Data Alias
    result.push_str(&(formatter.to_type_def_start)(&get_name(
        alias_stack,
        &ast::get_aliased_name(query_field),
    )));

    let alias_stack = push_alias_stack(query_field, alias_stack);

    let mut explicit_columns: std::collections::HashSet<String> = std::collections::HashSet::new();
    for field in fields.iter() {
        if field.name == "*" {
            continue;
        }
        if let Some(ast::Field::Column(column)) = table
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(f, &field.name))
        {
            explicit_columns.insert(column.name.clone());
        }
    }

    let mut rendered_fields: Vec<(String, String, FieldMetadata)> = Vec::new();
    let mut wildcard_rendered = false;

    for field in fields.iter() {
        if field.name == "*" {
            if wildcard_rendered {
                continue;
            }
            wildcard_rendered = true;

            for table_field in &table.fields {
                if let ast::Field::Column(column) = table_field {
                    if explicit_columns.contains(&column.name) {
                        continue;
                    }
                    rendered_fields.push((
                        column.name.clone(),
                        column.type_.query_type_string(),
                        FieldMetadata {
                            is_link: false,
                            is_optional: column.nullable,
                            is_array_relationship: false,
                        },
                    ));
                }
            }
            continue;
        }

        if let Some(table_field) = table
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &field.name))
        {
            let aliased_name = ast::get_aliased_name(field);
            match table_field {
                ast::Field::Column(col) => {
                    rendered_fields.push((
                        aliased_name,
                        col.type_.query_type_string(),
                        FieldMetadata {
                            is_link: false,
                            is_optional: col.nullable,
                            is_array_relationship: false,
                        },
                    ));
                }
                ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                    if operation == ast::QueryOperation::Insert {
                        continue;
                    }

                    let primary_key_name = ast::get_primary_id_field_name(&table.fields);
                    let is_one_to_many = link.local_ids.iter().all(|id| {
                        primary_key_name
                            .as_ref()
                            .map(|pk| id == pk)
                            .unwrap_or(false)
                    });

                    let linked_to_unique =
                        if let Some(linked_table) = typecheck::get_linked_table(context, link) {
                            ast::linked_to_unique_field_with_record(link, &linked_table.record)
                        } else {
                            ast::linked_to_unique_field(link)
                        };

                    rendered_fields.push((
                        aliased_name.clone(),
                        get_name(&alias_stack, &aliased_name),
                        FieldMetadata {
                            is_link: true,
                            is_optional: !is_one_to_many && linked_to_unique,
                            is_array_relationship: is_one_to_many,
                        },
                    ));
                }
                _ => {}
            }
        }
    }

    let last_index = rendered_fields.len().saturating_sub(1);
    for (i, (name, type_, metadata)) in rendered_fields.iter().enumerate() {
        let is_last = i == last_index;
        result.push_str(&(formatter.to_field)(name, type_, metadata.clone()));
        result.push_str(&(formatter.to_field_separator)(is_last));
    }

    result.push_str(&(formatter.to_type_def_end)());
    result.push_str("\n\n");
}

pub fn push_alias_stack(field: &ast::QueryField, alias_stack: &str) -> String {
    let name = field.alias.as_ref().unwrap_or(&field.name);
    let capitalized = crate::ext::string::capitalize(name);

    if alias_stack.is_empty() {
        capitalized
    } else {
        format!("{}_{}", alias_stack, capitalized)
    }
}
