use crate::ast;
use crate::ext::string;
use crate::generate::sql::json::select as json_select;
use crate::generate::sql::returning;
use crate::generate::sql::select;
use crate::generate::sql::to_sql;
use crate::typecheck;

/*

See the temp_tables/mod.rs to see an overview of the sql strategy we want here.


The general algorithm.

1. Insert a value into the current table.
2. If there is a nested insert, create a temporary table with the name format of _temp_inserted_{table_field_alias}
    3. recursively generate for next nested insert.
4. Delete temp table.




*/

// Structure to track affected tables during inserts
struct AffectedTable {
    table_name: String,
    column_names: Vec<String>,
    temp_table_name: String,
}

pub fn insert_to_string(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
    include_affected_rows: bool,
) -> Vec<to_sql::Prepared> {
    let all_query_fields = ast::collect_query_fields(&query_table_field.fields);

    let mut statements = to_sql::format_attach(query_info);
    let has_links = all_query_fields.iter().any(|query_field| {
        matches!(
            table
                .record
                .fields
                .iter()
                .find(|field| ast::has_field_or_linkname(field, &query_field.name)),
            Some(ast::Field::FieldDirective(ast::FieldDirective::Link(_)))
        )
    });

    if !has_links {
        let mut statement = initial_select(0, context, query, table, query_table_field);
        statement.push_str(&format!(
            " returning {} as {}",
            returning::response_expression(context, table, query_table_field),
            string::quote(&ast::get_aliased_name(query_table_field))
        ));
        if include_affected_rows {
            statement.push_str(&format!(
                ", json_array({}) as _affectedRows",
                returning::affected_rows_expression(context, table)
            ));
        }
        statements.push(to_sql::include(statement));
        return statements;
    }

    statements.push(to_sql::ignore(initial_select(
        0,
        context,
        query,
        table,
        query_table_field,
    )));

    let parent_temp_table_name = &get_temp_table_name(&query_table_field);
    let mut affected_tables: Vec<AffectedTable> = Vec::new();

    // Track parent table
    if include_affected_rows {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let column_names = collect_all_column_names(context, &table.record.fields);
        affected_tables.push(AffectedTable {
            table_name,
            column_names,
            temp_table_name: parent_temp_table_name.clone(),
        });
    }

    // Drop temp table if it exists (from previous batch) before creating a new one
    // This prevents "table already exists" errors when reusing the same client connection
    statements.push(to_sql::ignore(format!(
        "drop table if exists {}",
        parent_temp_table_name
    )));

    // Always create temp table - we need it for the typed response query
    statements.push(to_sql::ignore(format!(
        "create temp table {} as\n  select last_insert_rowid() as id",
        parent_temp_table_name
    )));

    for query_field in all_query_fields.iter() {
        let table_field = &table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
            .unwrap();

        match table_field {
            ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                // We are inserting a link, so we need to do a nested insert
                let linked_table = typecheck::get_linked_table(context, &link).unwrap();

                // Track nested table
                if include_affected_rows {
                    let nested_temp_table_name = get_temp_table_name(query_field);
                    let linked_table_name =
                        ast::get_tablename(&linked_table.record.name, &linked_table.record.fields);
                    let linked_column_names =
                        collect_all_column_names(context, &linked_table.record.fields);
                    affected_tables.push(AffectedTable {
                        table_name: linked_table_name,
                        column_names: linked_column_names,
                        temp_table_name: nested_temp_table_name.clone(),
                    });
                }

                insert_linked(
                    0,
                    context,
                    query,
                    parent_temp_table_name,
                    linked_table,
                    query_field,
                    link,
                    &mut statements,
                    include_affected_rows,
                    &mut affected_tables,
                );
            }
            _ => (),
        }
    }

    // Always generate the final selection query - mutations must return typed data
    let query_field_name = &query_table_field.name;
    let primary_table_name = select::get_tablename(
        &select::TableAliasKind::Normal,
        table,
        &ast::get_aliased_name(&query_table_field),
    );

    let mut final_statement = String::new();
    final_statement.push_str("select\n");
    final_statement.push_str("  coalesce(json_group_array(\n");
    final_statement.push_str("    json_object(\n");

    // Generate JSON object fields directly from table
    let mut first_field = true;
    for field in &query_table_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    let aliased_field_name = ast::get_aliased_name(query_field);

                    match table_field {
                        ast::Field::Column(column) => {
                            if !first_field {
                                final_statement.push_str(",\n");
                            }
                            final_statement.push_str(&format!("      '{}', ", aliased_field_name));

                            // Handle boolean types: SQLite stores booleans as 0/1, convert to JSON boolean
                            if column.type_.is_bool() {
                                final_statement.push_str(&format!(
                                    "json(case when t.{} = 1 then 'true' else 'false' end)",
                                    string::quote(&query_field.name)
                                ));
                            } else if matches!(
                                column.type_.to_serialization_type(),
                                ast::SerializationType::FromType(_)
                            ) {
                                final_statement.push_str(&json_select::select_type_expression(
                                    6,
                                    context,
                                    column,
                                    "t",
                                    &query_field.name,
                                    false,
                                ));
                            } else {
                                final_statement
                                    .push_str(&format!("t.{}", string::quote(&query_field.name)));
                            }
                            first_field = false;
                        }
                        _ => continue,
                    }
                }
            }
            _ => continue,
        }
    }

    final_statement.push_str("\n    )\n  ), json('[]')) as ");
    final_statement.push_str(query_field_name);
    final_statement.push_str("\nfrom ");
    final_statement.push_str(&primary_table_name);
    final_statement.push_str(" t\n");
    final_statement.push_str(&format!(
        "join {} temp_table on t.rowid = temp_table.id",
        parent_temp_table_name
    ));

    statements.push(to_sql::include(final_statement));

    // Generate affected rows query if requested
    // Execute this BEFORE the final selection to avoid lock conflicts
    if include_affected_rows && !affected_tables.is_empty() {
        let affected_rows_sql =
            generate_affected_rows_query_for_inserts(context, query_info, &affected_tables);
        // Always insert before the final selection (which now always exists)
        let final_idx = statements.len() - 1;
        statements.insert(final_idx, to_sql::include(affected_rows_sql));
    }

    // Drop temp tables when not tracking affected rows (no result sets = safe to drop).
    // When tracking affected rows, temp tables persist across batches when reusing the same
    // client connection. We don't drop them explicitly to avoid lock errors from dropping
    // while result sets are active, but this means temp tables will persist and can cause
    // "table already exists" errors in subsequent batches. See docs/sql_remote.md for details.
    if !include_affected_rows {
        drop_temp_tables(query_table_field, &mut statements);
    }

    statements
}

fn drop_temp_tables(query_field: &ast::QueryField, statements: &mut Vec<to_sql::Prepared>) {
    statements.push(to_sql::ignore(drop_table(query_field)));

    // Only the primary field has a temp table created for it now
    // for arg_field in query_field.fields.iter() {
    //     match arg_field {
    //         ast::ArgField::Field(field) => {
    //             if !field.fields.is_empty() {
    //                 drop_temp_tables(field, statements);
    //             }
    //         }
    //         _ => continue,
    //     }
    // }
}

fn drop_table(query_field: &ast::QueryField) -> String {
    format!("drop table {}", &get_temp_table_name(&query_field))
}

pub fn get_temp_table_name(query_field: &ast::QueryField) -> String {
    format!("temp_inserted_{}", &ast::get_aliased_name(&query_field))
}

pub fn initial_select(
    indent: usize,
    context: &typecheck::Context,
    query: &ast::Query,
    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
) -> String {
    let indent_str = " ".repeat(indent);
    let mut field_names: Vec<String> = Vec::new();

    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let new_fieldnames = &to_fieldnames(
        context,
        table,
        &ast::collect_query_fields(&query_table_field.fields),
    );
    field_names.append(&mut new_fieldnames.clone());

    let all_query_fields = ast::collect_query_fields(&query_table_field.fields);

    let managed_insert_timestamps = managed_insert_timestamp_columns(table);
    for column in &managed_insert_timestamps {
        field_names.push(column.name.clone());
    }

    // Preserve legacy auto-updatedAt behavior for the sync-added field.
    let has_legacy_updated_at_field = table.record.fields.iter().any(|f| {
        ast::has_fieldname(f, "updatedAt")
            && !matches!(f, ast::Field::Column(col) if ast::is_updated_at(col))
    });
    let updated_at_explicitly_set = has_explicit_insert_field(&all_query_fields, "updatedAt");

    if has_legacy_updated_at_field && !updated_at_explicitly_set {
        field_names.push("updatedAt".to_string());
    }

    let mut result = format!(
        "{}insert into {} ({})\n",
        indent_str,
        table_name,
        field_names.join(", ")
    );

    let values = &to_field_insert_values(context, query, table, &all_query_fields);

    let mut final_values = values.clone();
    for _ in &managed_insert_timestamps {
        final_values.push("unixepoch()".to_string());
    }

    if has_legacy_updated_at_field && !updated_at_explicitly_set {
        final_values.push("unixepoch()".to_string());
    }

    result.push_str(&format!(
        "{}values ({})",
        indent_str,
        final_values.join(", ")
    ));
    result
}

fn insert_linked(
    indent: usize,
    context: &typecheck::Context,
    query: &ast::Query,
    parent_table_name: &String,
    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
    link: &ast::LinkDetails,
    statements: &mut Vec<to_sql::Prepared>,
    include_affected_rows: bool,
    affected_tables: &mut Vec<AffectedTable>,
) {
    // INSERT INTO users (username, credit) VALUES ('john_doe', 100);
    let mut field_names: Vec<String> = Vec::new();

    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let new_fieldnames = &to_fieldnames(
        context,
        table,
        &ast::collect_query_fields(&query_table_field.fields),
    );
    field_names.push(link.foreign.fields.clone().join(", "));
    field_names.append(&mut new_fieldnames.clone());

    let all_query_fields = ast::collect_query_fields(&query_table_field.fields);

    let managed_insert_timestamps = managed_insert_timestamp_columns(table);
    for column in &managed_insert_timestamps {
        field_names.push(column.name.clone());
    }

    // Preserve legacy auto-updatedAt behavior for the sync-added field.
    let has_legacy_updated_at_field = table.record.fields.iter().any(|f| {
        ast::has_fieldname(f, "updatedAt")
            && !matches!(f, ast::Field::Column(col) if ast::is_updated_at(col))
    });
    let updated_at_explicitly_set = has_explicit_insert_field(&all_query_fields, "updatedAt");

    if has_legacy_updated_at_field && !updated_at_explicitly_set {
        field_names.push("updatedAt".to_string());
    }

    let mut insert_values = vec![];
    for local_id in &link.local_ids {
        insert_values.push(format!(
            "{}.{}",
            string::quote(parent_table_name),
            string::quote(&local_id)
        ));
    }

    insert_values.append(&mut to_field_insert_values(
        context,
        query,
        table,
        &all_query_fields,
    ));

    for _ in &managed_insert_timestamps {
        insert_values.push("unixepoch()".to_string());
    }

    if has_legacy_updated_at_field && !updated_at_explicitly_set {
        insert_values.push("unixepoch()".to_string());
    }

    statements.push(to_sql::ignore(format!(
        "insert into {} ({})\n  select {}\n  from {}",
        table_name,
        field_names.join(", "),
        insert_values.join(", "),
        parent_table_name
    )));

    let temp_table_name = &get_temp_table_name(&query_table_field);

    // Create temp table for nested inserts if tracking affected rows
    // This must happen AFTER the insert to capture the inserted rowids
    if include_affected_rows {
        // Drop temp table if it exists (from previous batch) before creating a new one
        statements.push(to_sql::ignore(format!(
            "drop table if exists {}",
            temp_table_name
        )));

        // Create temp table with rowids of inserted rows by joining on foreign key
        let foreign_key = &link.foreign.fields[0];
        let local_key = &link.local_ids[0];
        let quoted_foreign_key = string::quote(foreign_key);
        let quoted_local_key = string::quote(local_key);
        let quoted_table_name_for_temp = string::quote(&table_name);
        let quoted_parent_table = string::quote(parent_table_name);
        statements.push(to_sql::ignore(format!(
            "create temp table {} as\n  select t.rowid as id\n  from {} t\n  join {} p on t.{} = p.{}",
            temp_table_name,
            quoted_table_name_for_temp,
            quoted_parent_table,
            quoted_foreign_key,
            quoted_local_key
        )));
    }

    for query_field in all_query_fields {
        let table_field = &table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
            .unwrap();

        match table_field {
            ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                // We are inserting a link, so we need to do a nested insert
                let linked_table = typecheck::get_linked_table(context, &link).unwrap();

                // Track nested table
                if include_affected_rows {
                    let nested_temp_table_name = get_temp_table_name(query_field);
                    let linked_table_name =
                        ast::get_tablename(&linked_table.record.name, &linked_table.record.fields);
                    let linked_column_names =
                        collect_all_column_names(context, &linked_table.record.fields);
                    affected_tables.push(AffectedTable {
                        table_name: linked_table_name,
                        column_names: linked_column_names,
                        temp_table_name: nested_temp_table_name.clone(),
                    });
                }

                insert_linked(
                    indent + 2,
                    context,
                    query,
                    &temp_table_name,
                    linked_table,
                    query_field,
                    &link,
                    statements,
                    include_affected_rows,
                    affected_tables,
                );
            }
            _ => (),
        }
    }
}

fn generate_affected_rows_query_for_inserts(
    _context: &typecheck::Context,
    _query_info: &typecheck::QueryInfo,
    affected_tables: &Vec<AffectedTable>,
) -> String {
    let mut union_parts = Vec::new();

    for affected_table in affected_tables {
        let quoted_table_name = string::quote(&affected_table.table_name);

        // Build json_array call for each row - values in same order as headers
        let mut row_value_parts = Vec::new();
        for col in &affected_table.column_names {
            // Quote both table and column to handle special characters like __
            // Column names with __ are valid unquoted identifiers in SQLite, but we quote them for safety
            row_value_parts.push(format!("{}.{}", quoted_table_name, string::quote(col)));
        }

        // Build json_array call for headers
        // Headers should just be column names in single quotes (for JSON strings), not double-quoted
        let mut header_parts = Vec::new();
        for col in &affected_table.column_names {
            header_parts.push(format!("'{}'", col));
        }
        // Build the join condition - all tables use their temp table
        // Use table name directly instead of alias to avoid issues with quoted column names
        let join_condition = format!(
            "join {} temp_table on {}.rowid = temp_table.id",
            affected_table.temp_table_name, quoted_table_name
        );

        // Format: { table_name, headers, rows: [[...], [...]] }
        let select_part = format!(
            "select json_object(\n    'table_name', '{}',\n    'headers', json_array({}),\n    'rows', json_group_array(json_array({}))\n  ) as affected_row\n  from {}\n  {}",
            affected_table.table_name,
            header_parts.join(", "),
            row_value_parts.join(", "),
            quoted_table_name,
            join_condition
        );

        union_parts.push(select_part);
    }

    // Use json() to parse the JSON string before grouping, so we get an array of objects, not an array of strings
    format!(
        "select json_group_array(json(affected_row)) as _affectedRows\nfrom (\n  {}\n)",
        union_parts.join("\n  union all\n  ")
    )
}

// Collect all column names including union type variant columns
fn collect_all_column_names(context: &typecheck::Context, fields: &Vec<ast::Field>) -> Vec<String> {
    typecheck::to_sql_column_info(context, fields)
        .into_iter()
        .map(|column| column.name)
        .collect()
}

// Field names

fn to_fieldnames(
    context: &typecheck::Context,
    table: &typecheck::Table,
    query_fields: &Vec<&ast::QueryField>,
) -> Vec<String> {
    let mut result = vec![];

    for field in query_fields {
        if field.set.is_none() {
            continue;
        }

        let table_field = &table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &field.name))
            .unwrap();

        result.append(&mut to_table_fieldname(2, context, &table_field, &field));
    }

    result
}

fn has_explicit_insert_field(query_fields: &Vec<&ast::QueryField>, name: &str) -> bool {
    query_fields
        .iter()
        .any(|field| field.name == name && field.set.is_some())
}

fn managed_insert_timestamp_columns(table: &typecheck::Table) -> Vec<&ast::Column> {
    table
        .record
        .fields
        .iter()
        .filter_map(|field| match field {
            ast::Field::Column(column)
                if ast::is_created_at(column) || ast::is_updated_at(column) =>
            {
                Some(column)
            }
            _ => None,
        })
        .collect()
}

fn to_table_fieldname(
    _indent: usize,
    context: &typecheck::Context,
    table_field: &ast::Field,
    query_field: &ast::QueryField,
) -> Vec<String> {
    match table_field {
        ast::Field::Column(col) => {
            // Integer @id fields are auto-generated and shouldn't be in INSERT.
            if ast::is_integer_primary_key(col) {
                return vec![];
            }

            // Check if this is a union type column
            match col.type_.to_serialization_type() {
                ast::SerializationType::FromType(typename) => {
                    if matches!(query_field.set, Some(ast::QueryValue::Variable(_))) {
                        return collect_all_column_names(context, &vec![table_field.clone()]);
                    }

                    // Union type column - need discriminator + variant-specific columns
                    let mut result = vec![query_field.name.clone()]; // discriminator column

                    // Get variant-specific columns based on the value being set
                    if let Some(set_value) = &query_field.set {
                        if let ast::QueryValue::LiteralTypeValue((_, details)) = set_value {
                            // Get the union type definition
                            if let Some((_, type_)) = context.types.get(&typename) {
                                if let typecheck::Type::OneOf { variants } = type_ {
                                    // Find the variant being used
                                    if let Some(variant) =
                                        variants.iter().find(|v| v.name == details.name)
                                    {
                                        // Add variant-specific field columns
                                        if let Some(variant_fields) = &variant.fields {
                                            let base_name = format!("{}__", query_field.name);
                                            for variant_field in variant_fields {
                                                match variant_field {
                                                    ast::Field::Column(variant_col) => {
                                                        let column_name = format!(
                                                            "{}{}",
                                                            base_name, variant_col.name
                                                        );
                                                        result.push(column_name);
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    return result;
                }
                _ => {
                    // Regular column
                    let str = query_field.name.to_string();
                    return vec![str];
                }
            }
        }
        _ => vec![],
    }
}

// Insert
fn to_field_insert_values(
    context: &typecheck::Context,
    query: &ast::Query,
    table: &typecheck::Table,
    fields: &Vec<&ast::QueryField>,
) -> Vec<String> {
    let mut result = vec![];

    for field in fields {
        // Find the table field to check if it's a primary key or union type
        let table_field = table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &field.name));

        // Skip integer primary keys - they're auto-generated and shouldn't be in INSERT values
        if let Some(ast::Field::Column(col)) = table_field {
            if ast::is_integer_primary_key(col) {
                continue;
            }
        }

        match &field.set {
            None => (),
            Some(val) => {
                if let Some(ast::Field::Column(col)) = table_field {
                    if let ast::SerializationType::FromType(typename) =
                        col.type_.to_serialization_type()
                    {
                        if let ast::QueryValue::Variable((_, var)) = val {
                            result.append(&mut render_type_variable_insert_values(
                                context,
                                query,
                                col,
                                &field.name,
                                &var.name,
                                &typename,
                            ));
                            continue;
                        }
                    }
                }

                // Check if this is a union type value
                if let ast::QueryValue::LiteralTypeValue((_, details)) = val {
                    // Find the table field to check if it's a union type
                    if let Some(ast::Field::Column(col)) = table_field {
                        if let ast::SerializationType::FromType(_typename) =
                            col.type_.to_serialization_type()
                        {
                            // This is a union type - need discriminator + variant field values
                            // Add discriminator value (variant name)
                            result.push(format!("'{}'", details.name));

                            // Add variant field values in order
                            if let Some(variant_fields) = &details.fields {
                                // Get the union type definition to find variant field order
                                if let ast::SerializationType::FromType(typename) =
                                    col.type_.to_serialization_type()
                                {
                                    if let Some((_, type_)) = context.types.get(&typename) {
                                        if let typecheck::Type::OneOf { variants } = type_ {
                                            if let Some(variant) =
                                                variants.iter().find(|v| v.name == details.name)
                                            {
                                                if let Some(variant_field_defs) = &variant.fields {
                                                    // Create a map of field names to values for quick lookup
                                                    let field_map: std::collections::HashMap<
                                                        &String,
                                                        &ast::QueryValue,
                                                    > = variant_fields
                                                        .iter()
                                                        .map(|(name, val)| (name, val))
                                                        .collect();

                                                    // Add values in the order they appear in the variant definition
                                                    for variant_field_def in variant_field_defs {
                                                        if let ast::Field::Column(variant_col) =
                                                            variant_field_def
                                                        {
                                                            if let Some(value) =
                                                                field_map.get(&variant_col.name)
                                                            {
                                                                result.push(
                                                                    to_sql::render_column_value(
                                                                        variant_col,
                                                                        value,
                                                                    ),
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            continue; // Skip the regular render_value call
                        }
                    }
                }

                // Regular value (not a union type or union type without fields)
                match table_field {
                    Some(ast::Field::Column(col)) => {
                        let str = render_insert_column_value(query, col, &field.name, &val);
                        result.push(str);
                    }
                    _ => {
                        let str = to_sql::render_value(&val);
                        result.push(str);
                    }
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::{has_explicit_insert_field, initial_select};
    use crate::{ast, typecheck};

    #[test]
    fn initial_select_ignores_return_only_updated_at() {
        let context = typecheck::empty_context();
        let query = ast::Query {
            interface_hash: String::new(),
            full_hash: String::new(),
            operation: ast::QueryOperation::Insert,
            name: "UserCreate".to_string(),
            args: vec![],
            fields: vec![],
            start: None,
            end: None,
        };

        let table = typecheck::Table {
            schema: ast::DEFAULT_SCHEMANAME.to_string(),
            record: ast::RecordDetails {
                name: "User".to_string(),
                fields: vec![
                    ast::Field::Column(ast::Column {
                        name: "id".to_string(),
                        type_: ast::ColumnType::Int,
                        nullable: false,
                        directives: vec![ast::ColumnDirective::PrimaryKey],
                        start: None,
                        end: None,
                        start_name: None,
                        end_name: None,
                        start_typename: None,
                        end_typename: None,
                        inline_comment: None,
                    }),
                    ast::Field::Column(ast::Column {
                        name: "name".to_string(),
                        type_: ast::ColumnType::String,
                        nullable: false,
                        directives: vec![],
                        start: None,
                        end: None,
                        start_name: None,
                        end_name: None,
                        start_typename: None,
                        end_typename: None,
                        inline_comment: None,
                    }),
                    ast::Field::Column(ast::Column {
                        name: "updatedAt".to_string(),
                        type_: ast::ColumnType::DateTime,
                        nullable: false,
                        directives: vec![ast::ColumnDirective::Default {
                            id: "now".to_string(),
                            value: ast::DefaultValue::Now,
                            start: None,
                            end: None,
                        }],
                        start: None,
                        end: None,
                        start_name: None,
                        end_name: None,
                        start_typename: None,
                        end_typename: None,
                        inline_comment: None,
                    }),
                ],
                start: None,
                end: None,
                start_name: None,
                end_name: None,
            },
            sync_layer: 0,
            filepath: String::new(),
        };

        let query_table_field = ast::QueryField {
            name: "user".to_string(),
            alias: None,
            set: None,
            directives: vec![],
            fields: vec![
                ast::ArgField::Field(ast::QueryField {
                    name: "name".to_string(),
                    alias: None,
                    set: Some(ast::QueryValue::Variable((
                        ast::empty_range(),
                        ast::VariableDetails {
                            name: "name".to_string(),
                            session_field: None,
                        },
                    ))),
                    directives: vec![],
                    fields: vec![],
                    start_fieldname: None,
                    end_fieldname: None,
                    start: None,
                    end: None,
                }),
                ast::ArgField::Field(ast::QueryField {
                    name: "updatedAt".to_string(),
                    alias: None,
                    set: None,
                    directives: vec![],
                    fields: vec![],
                    start_fieldname: None,
                    end_fieldname: None,
                    start: None,
                    end: None,
                }),
            ],
            start_fieldname: None,
            end_fieldname: None,
            start: None,
            end: None,
        };

        let sql = initial_select(0, &context, &query, &table, &query_table_field);
        assert_eq!(
            sql,
            "insert into users (name, updatedAt)\nvalues ($name, unixepoch())"
        );
    }

    #[test]
    fn select_only_updated_at_is_not_treated_as_explicit_insert() {
        let query_field = ast::QueryField {
            name: "updatedAt".to_string(),
            alias: None,
            set: None,
            directives: vec![],
            fields: vec![],
            start_fieldname: None,
            end_fieldname: None,
            start: None,
            end: None,
        };

        assert!(!has_explicit_insert_field(&vec![&query_field], "updatedAt"));
    }
}

fn render_insert_column_value(
    query: &ast::Query,
    column: &ast::Column,
    field_name: &str,
    value: &ast::QueryValue,
) -> String {
    let rendered = to_sql::render_column_value(column, value);

    let is_omittable = query
        .args
        .iter()
        .find(|arg| arg.name == field_name)
        .map(|arg| arg.omittable)
        .unwrap_or(false);

    if !is_omittable {
        return rendered;
    }

    format!(
        "case when ${field_name}__is_set then {rendered} else {fallback} end",
        field_name = field_name,
        rendered = rendered,
        fallback = render_column_insert_fallback(column)
    )
}

fn render_column_insert_fallback(column: &ast::Column) -> String {
    for directive in &column.directives {
        if let ast::ColumnDirective::Default { value, .. } = directive {
            return match value {
                ast::DefaultValue::Now => match column.type_ {
                    ast::ColumnType::DateTime => "unixepoch()".to_string(),
                    _ => "CURRENT_TIMESTAMP".to_string(),
                },
                ast::DefaultValue::Value(default_value) => {
                    to_sql::render_column_value(column, default_value)
                }
            };
        }
    }

    "null".to_string()
}

fn render_type_variable_insert_values(
    context: &typecheck::Context,
    query: &ast::Query,
    column: &ast::Column,
    field_name: &str,
    variable_name: &str,
    typename: &str,
) -> Vec<String> {
    if is_enum_type(context, typename) {
        return vec![render_omittable_insert_value(
            query,
            field_name,
            &format!("${}", variable_name),
            &render_column_insert_fallback(column),
        )];
    }

    let mut result = vec![render_insert_type_json_extract(
        query,
        field_name,
        variable_name,
        "",
    )];

    if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(typename) {
        for variant in variants {
            if let Some(fields) = &variant.fields {
                for variant_field in fields {
                    append_type_field_variable_insert_values(
                        context,
                        query,
                        field_name,
                        variable_name,
                        "",
                        variant_field,
                        &mut result,
                    );
                }
            }
        }
    }

    result
}

fn is_enum_type(context: &typecheck::Context, typename: &str) -> bool {
    matches!(
        context.types.get(typename),
        Some((_, typecheck::Type::OneOf { variants }))
            if variants.iter().all(|variant| variant.fields.is_none())
    )
}

fn append_type_field_variable_insert_values(
    context: &typecheck::Context,
    query: &ast::Query,
    field_name: &str,
    variable_name: &str,
    json_prefix: &str,
    field: &ast::Field,
    result: &mut Vec<String>,
) {
    let ast::Field::Column(inner_column) = field else {
        return;
    };

    let json_path = if json_prefix.is_empty() {
        inner_column.name.clone()
    } else {
        format!("{}.{}", json_prefix, inner_column.name)
    };

    match inner_column.type_.to_serialization_type() {
        ast::SerializationType::Concrete(_) => {
            result.push(render_insert_json_extract(
                query,
                field_name,
                variable_name,
                &json_path,
            ));
        }
        ast::SerializationType::FromType(typename) => {
            result.push(render_insert_type_json_extract(
                query,
                field_name,
                variable_name,
                &json_path,
            ));

            if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(&typename) {
                for variant in variants {
                    if let Some(fields) = &variant.fields {
                        for variant_field in fields {
                            append_type_field_variable_insert_values(
                                context,
                                query,
                                field_name,
                                variable_name,
                                &json_path,
                                variant_field,
                                result,
                            );
                        }
                    }
                }
            }
        }
    }
}

fn render_insert_type_json_extract(
    query: &ast::Query,
    field_name: &str,
    variable_name: &str,
    json_path: &str,
) -> String {
    let type_path = if json_path.is_empty() {
        "$._type".to_string()
    } else {
        format!("$.{}._type", json_path)
    };
    let fallback = if json_path.is_empty() {
        format!("${variable_name}")
    } else {
        "null".to_string()
    };
    let rendered = format!(
        "case when json_valid(${variable_name}) then json_extract(${variable_name}, '{type_path}') else {fallback} end"
    );
    render_omittable_insert_value(query, field_name, &rendered, "null")
}

fn render_insert_json_extract(
    query: &ast::Query,
    field_name: &str,
    variable_name: &str,
    json_path: &str,
) -> String {
    let rendered = format!(
        "case when json_valid(${variable_name}) then json_extract(${variable_name}, '$.{json_path}') else null end"
    );
    render_omittable_insert_value(query, field_name, &rendered, "null")
}

fn render_omittable_insert_value(
    query: &ast::Query,
    field_name: &str,
    rendered: &str,
    fallback: &str,
) -> String {
    let is_omittable = query
        .args
        .iter()
        .find(|arg| arg.name == field_name)
        .map(|arg| arg.omittable)
        .unwrap_or(false);

    if is_omittable {
        format!(
            "case when ${field_name}__is_set then {rendered} else {fallback} end",
            field_name = field_name,
            rendered = rendered,
            fallback = fallback,
        )
    } else {
        rendered.to_string()
    }
}
