use crate::ast;
use crate::ext::string;
use crate::generate::sql::to_sql;
use crate::typecheck;

/*

Given a query, we have 3 choices for generating sql.
1. Normal: A normal join
2. Batch: Flatten and batch the queries
3. CTE: Use a CTE

Batches are basically like a CTE, but where we have to do the join in the application layer.

So, our first approach is going to be using a CTE.

For selects, here's how we choose what strategy to take.

1. We default to using the join.
2. If there is a limit, we use the CTE form.
3. If there is a @where on anything but the top-level table, we need to use a CTE


2 is because the limit applies to the result, but conceptually we want it to apply to the table it's attached to.
So, if we add an @limit 1 to our query for users and their blogposts, we will only return 1 user and maybe 1 blogpost.
And if the limit is 2, we could return 1-2 users and 1-2 blogposts.

With 'where' it's the same conceptual problem.




*/
pub fn select_to_string(
    context: &typecheck::Context,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
) -> Vec<to_sql::Prepared> {
    let mut statements = to_sql::format_attach(query_info);

    let mut selection = String::new();
    selection.push_str("select\n");

    // Selection

    let selected = &to_selection(
        context,
        &ast::get_aliased_name(&query_field),
        table,
        query_info,
        &ast::collect_query_fields(&query_field.fields),
        &TableAliasKind::Normal,
        true, // is_top_level
    );
    selection.push_str("  ");
    selection.push_str(&selected.join(",\n  "));
    selection.push_str("\n");

    // FROM
    render_from(
        context,
        table,
        query_info,
        query_field,
        &TableAliasKind::Normal,
        &mut selection,
    );

    // WHERE
    to_sql::render_where(
        context,
        table,
        query_info,
        query_field,
        &ast::QueryOperation::Query,
        &mut selection,
    );

    // Order by
    to_sql::render_order_by(Some(table), Some(query_info), query_field, &mut selection);

    // LIMIT
    to_sql::render_limit(query_field, &mut selection);

    statements.push(to_sql::include(selection));

    statements
}

pub enum TableAliasKind {
    Normal,
    Insert,
}

// SELECT
pub fn to_selection(
    context: &typecheck::Context,
    table_alias: &str,
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    fields: &Vec<&ast::QueryField>,
    table_alias_kind: &TableAliasKind,
    is_top_level: bool,
) -> Vec<String> {
    let mut result = vec![];
    let mut explicit_columns = std::collections::HashSet::new();

    for field in fields {
        if field.name == "*" {
            continue;
        }
        if let Some(table_field) = table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &field.name))
        {
            if let ast::Field::Column(column) = table_field {
                explicit_columns.insert(column.name.clone());
            }
        }
    }

    for field in fields {
        if field.name == "*" {
            for table_field in &table.record.fields {
                if let ast::Field::Column(column) = table_field {
                    if explicit_columns.contains(&column.name) {
                        continue;
                    }
                    let wildcard_field = ast::QueryField {
                        name: column.name.clone(),
                        alias: None,
                        set: None,
                        directives: vec![],
                        fields: vec![],
                        start: None,
                        end: None,
                        start_fieldname: None,
                        end_fieldname: None,
                    };
                    result.append(&mut to_subselection(
                        context,
                        table,
                        table_alias,
                        &table_field,
                        query_info,
                        &wildcard_field,
                        table_alias_kind,
                        is_top_level,
                    ));
                }
            }
            continue;
        }

        if let Some(table_field) = table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &field.name))
        {
            result.append(&mut to_subselection(
                context,
                table,
                table_alias,
                &table_field,
                query_info,
                &field,
                table_alias_kind,
                is_top_level,
            ));
        }
    }

    result
}

fn to_subselection(
    context: &typecheck::Context,
    table: &typecheck::Table,
    table_alias: &str,
    table_field: &ast::Field,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
    table_alias_kind: &TableAliasKind,
    is_top_level: bool,
) -> Vec<String> {
    match table_field {
        ast::Field::Column(table_column) => {
            let source_field = match table_alias_kind {
                TableAliasKind::Normal => {
                    if is_top_level {
                        // Top-level table - use render_real_field for schema qualification
                        // The FROM clause doesn't use an alias, so we use the actual table name
                        to_sql::render_real_field(table, query_info, query_field)
                    } else {
                        // Linked table - use the alias to avoid collisions when multiple fields point to the same table
                        format!(
                            "{}.{}",
                            string::quote(table_alias),
                            string::quote(&query_field.name),
                        )
                    }
                }
                TableAliasKind::Insert => {
                    let table_name = get_tablename(table_alias_kind, table, table_alias);
                    format!(
                        "{}.{}",
                        string::quote(&table_name),
                        string::quote(&query_field.name),
                    )
                }
            };
            match table_column.type_.to_serialization_type() {
                ast::SerializationType::Concrete(_) => {
                    // A single concrete type
                    let str = format!(
                        "{} as {}",
                        source_field,
                        string::quote(&ast::get_select_alias(table_alias, query_field))
                    );
                    return vec![str];
                }
                ast::SerializationType::FromType(typename) => {
                    // For union types, we need to select the discriminator column and any variant-specific columns
                    let mut selected = vec![];
                    select_type_columns(
                        context,
                        &typename,
                        &source_field,
                        &ast::get_select_alias(table_alias, query_field),
                        &mut selected,
                    );
                    return selected;
                }
            }
        }
        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
            if let Some(link_table) = typecheck::get_linked_table(context, &link) {
                return to_selection(
                    context,
                    &ast::get_aliased_name(&query_field),
                    link_table,
                    query_info,
                    &ast::collect_query_fields(&query_field.fields),
                    table_alias_kind,
                    false, // Not top-level - this is a linked table
                );
            } else {
                return vec![];
            }
        }

        _ => vec![],
    }
}

fn select_type_columns(
    context: &typecheck::Context,
    typename: &str,
    source_field: &str,
    alias: &str,
    selection: &mut Vec<String>,
) {
    match context.types.get(typename) {
        None => {
            // Unknown type, just select the base field
            selection.push(format!("{} as {}", source_field, string::quote(alias)));
        }
        Some((_definfo, type_)) => {
            match type_ {
                typecheck::Type::OneOf { variants } => {
                    // For union types, generate a CASE statement that creates JSON objects
                    // This matches the pattern used in the JSON SQL generation
                    let is_enum = variants.iter().all(|v| v.fields.is_none());

                    if is_enum {
                        // Simple enum - just select the discriminator
                        selection.push(format!("{} as {}", source_field, string::quote(alias)));
                    } else {
                        // Union type with fields - generate JSON object with CASE statement
                        let mut case_sql = format!("case\n");

                        for variant in variants {
                            case_sql.push_str(&format!(
                                "  when {} = '{}' then",
                                source_field, variant.name
                            ));

                            match &variant.fields {
                                None => {
                                    // Simple variant - just the tag
                                    case_sql.push_str(&format!(
                                        " json_object('_type', '{}')",
                                        variant.name
                                    ));
                                }
                                Some(fields) => {
                                    // Variant with fields - include them in the JSON object
                                    case_sql.push_str(&format!("\n    json_object("));
                                    case_sql
                                        .push_str(&format!("\n      '_type', '{}',", variant.name));

                                    let mut first_field = true;
                                    for field in fields {
                                        match field {
                                            ast::Field::Column(inner_column) => {
                                                if !first_field {
                                                    case_sql.push_str(",");
                                                }
                                                // Variant fields are stored as {columnName}__{fieldName}
                                                // Extract column name from source_field (everything after last dot, or whole string)
                                                // Handle quoted identifiers like "users"."status" by extracting the unquoted column name
                                                let base_column_unquoted =
                                                    if source_field.contains('.') {
                                                        // Split on "." and get the last part, which is the column name
                                                        // For "users"."status", this gives us "status" (with quotes), so strip them
                                                        let parts: Vec<&str> =
                                                            source_field.split('.').collect();
                                                        parts
                                                            .last()
                                                            .unwrap_or(&source_field)
                                                            .trim_matches('"')
                                                    } else {
                                                        source_field.trim_matches('"')
                                                    };
                                                // Construct the variant field column name (unquoted)
                                                let variant_field_name = format!(
                                                    "{}__{}",
                                                    base_column_unquoted, inner_column.name
                                                );
                                                // If source_field was qualified (e.g., "table.column"), preserve the qualification
                                                let qualified_variant_field =
                                                    if source_field.contains('.') {
                                                        // Extract table name (first part before the dot)
                                                        let table_part = source_field
                                                            .split('.')
                                                            .next()
                                                            .unwrap_or(source_field);
                                                        // Construct "table"."column__field" format with proper quoting
                                                        format!(
                                                            "{}.{}",
                                                            table_part,
                                                            string::quote(&variant_field_name)
                                                        )
                                                    } else {
                                                        string::quote(&variant_field_name)
                                                    };
                                                case_sql.push_str(&format!(
                                                    "\n      '{}', {}",
                                                    inner_column.name, qualified_variant_field
                                                ));
                                                first_field = false;
                                            }
                                            _ => {}
                                        }
                                    }

                                    case_sql.push_str("\n    )");
                                }
                            }
                            case_sql.push_str("\n");
                        }

                        case_sql.push_str("end");
                        selection.push(format!("{} as {}", case_sql, string::quote(alias)));
                    }
                }
                _ => {
                    // Other types - just select the base field
                    selection.push(format!("{} as {}", source_field, string::quote(alias)));
                }
            }
        }
    }
}

// FROM
//
pub fn render_from(
    context: &typecheck::Context,
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    query_table_field: &ast::QueryField,
    table_alias_kind: &TableAliasKind,
    result: &mut String,
) {
    result.push_str("from\n");

    let table_name = get_tablename(
        table_alias_kind,
        table,
        &ast::get_aliased_name(&query_table_field),
    );

    let from_vals = &mut to_from(
        context,
        // &get_temp_table_alias(table_alias_kind, &query_table_field),
        &table_name,
        table_alias_kind,
        table,
        query_info,
        &ast::collect_query_fields(&query_table_field.fields),
    );

    // the from statements are naturally in reverse order
    // Because we're walking outwards from the root, and `.push` ing the join statements
    // Now re reverse them so they're in the correct order.
    from_vals.reverse();

    result.push_str(&format!("  {}", string::quote(&table_name)));
    if from_vals.is_empty() {
        result.push_str("\n");
    } else {
        result.push_str("\n  ");
        result.push_str(&from_vals.join("\n  "));
        result.push_str("\n");
    }
}

fn to_from(
    context: &typecheck::Context,
    table_alias: &str,
    table_alias_kind: &TableAliasKind,
    table: &typecheck::Table,
    query_info: &typecheck::QueryInfo,
    fields: &Vec<&ast::QueryField>,
) -> Vec<String> {
    let mut result: Vec<String> = vec![];

    for query_field in fields {
        if let Some(table_field) = table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
        {
            result.append(&mut to_subfrom(
                context,
                table,
                table_alias,
                table_alias_kind,
                table_field,
                query_info,
                query_field,
            ));
        }
    }

    result
}

pub fn get_tablename(
    table_alias_kind: &TableAliasKind,
    table: &typecheck::Table,
    table_alias: &str,
) -> String {
    match table_alias_kind {
        TableAliasKind::Normal => ast::get_tablename(&table.record.name, &table.record.fields),
        TableAliasKind::Insert => {
            // If this is an insert, we are selecting from a temp table
            // format!("temp_inserted_{}", &ast::get_aliased_name(&query_field))
            format!("temp_inserted_{}", table_alias)
        }
    }
}

fn to_subfrom(
    context: &typecheck::Context,
    table: &typecheck::Table,
    table_alias: &str,
    table_alias_kind: &TableAliasKind,
    table_field: &ast::Field,
    query_info: &typecheck::QueryInfo,
    query_field: &ast::QueryField,
) -> Vec<String> {
    match table_field {
        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
            let table_name = get_tablename(
                table_alias_kind,
                table,
                &ast::get_aliased_name(&query_field),
            );

            if let Some(link_table) = typecheck::get_linked_table(context, &link) {
                let foreign_table_name = get_tablename(
                    table_alias_kind,
                    link_table,
                    &ast::get_aliased_name(&query_field),
                );

                let mut inner_list = to_from(
                    context,
                    &table_name,
                    table_alias_kind,
                    link_table,
                    query_info,
                    &ast::collect_query_fields(&query_field.fields),
                );

                let local_table_identifier = match table_alias_kind {
                    TableAliasKind::Normal => to_sql::render_real_where_field(
                        table,
                        query_info,
                        false,
                        &link.local_ids.join(" "),
                    ),
                    TableAliasKind::Insert => {
                        format!(
                            "{}.{}",
                            string::quote(&table_alias),
                            string::quote(&link.local_ids.join(" "))
                        )
                    }
                };

                // Use the query field's aliased name as a table alias to avoid collisions
                // when multiple fields point to the same table
                let table_alias = ast::get_aliased_name(query_field);
                let foreign_table_identifier = match table_alias_kind {
                    TableAliasKind::Normal => {
                        // For normal queries, use the table alias in the identifier
                        format!(
                            "{}.{}",
                            string::quote(&table_alias),
                            string::quote(&link.foreign.fields.join(""))
                        )
                    }
                    TableAliasKind::Insert => {
                        format!(
                            "{}.{}",
                            string::quote(&table_alias),
                            string::quote(&link.foreign.fields.join(""))
                        )
                    }
                };

                let join = format!(
                    "left join {} {} on {} = {}",
                    string::quote(&foreign_table_name),
                    string::quote(&table_alias),
                    local_table_identifier,
                    foreign_table_identifier
                );
                inner_list.push(join);
                inner_list
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}
