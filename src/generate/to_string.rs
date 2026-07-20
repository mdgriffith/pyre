use crate::ast;
use nom::ToUsize;
use std::collections::BTreeMap;

pub fn schema_to_string(namespace: &str, schema: &ast::Schema) -> String {
    let mut result = String::new();
    let has_session_definition = schema.files.iter().any(|file| {
        file.definitions
            .iter()
            .any(|definition| matches!(definition, ast::Definition::Session(_)))
    });
    if let Some(session) = &schema.session {
        if !has_session_definition {
            result.push_str(&to_string_definition(
                namespace,
                &ast::Definition::Session(session.clone()),
            ));
        }
    }
    for schema_file in &schema.files {
        result.push_str(&schemafile_to_string(namespace, schema_file));
    }
    result
}

pub fn schemafile_to_string(namespace: &str, schema_file: &ast::SchemaFile) -> String {
    let mut result = String::new();

    for definition in &schema_file.definitions {
        result.push_str(&to_string_definition(namespace, definition));
    }
    result
}

fn to_string_definition(namespace: &str, definition: &ast::Definition) -> String {
    match definition {
        ast::Definition::Lines { count } => "\n".repeat((*count).min(2) as usize),
        ast::Definition::Comment { text } => format!("//{}\n", text),
        ast::Definition::SyncMode(sync_mode) => {
            format!("@syncable({})\n", sync_mode.syncable_literal())
        }
        ast::Definition::Session(session) => {
            let indent_collection: Indentation = collect_indentation(&session.fields, 4);

            let mut result = "session {\n".to_string();
            for field in &session.fields {
                result.push_str(&to_string_field(namespace, &indent_collection, &field));
            }
            result.push_str("}\n");
            result
        }
        ast::Definition::Tagged { name, variants, .. } => {
            let mut result = format!("type {}\n", name);
            let mut is_first = true;
            for variant in variants {
                result.push_str(&to_string_variant(namespace, is_first, variant));
                is_first = false;
            }
            result
        }
        ast::Definition::Record { name, fields, .. } => {
            let indent_collection: Indentation = collect_indentation(&fields, 4);

            let mut result = format!("record {} {{\n", name);
            for field in fields {
                result.push_str(&to_string_field(namespace, &indent_collection, &field));
            }
            result.push_str("}\n");
            result
        }
    }
}

#[derive(Debug)]
struct Indentation {
    minimum: usize,
    levels: BTreeMap<usize, FieldIndent>,
}

fn collect_indentation(fields: &Vec<ast::Field>, indent_minimum: usize) -> Indentation {
    let mut indent_collection: BTreeMap<usize, FieldIndent> = BTreeMap::new();
    let mut previous_linenumber: usize = 0;
    for field in fields {
        let maybe_field_indent = get_field_indent(indent_minimum, field);
        match maybe_field_indent {
            Some(indent) => match indent_collection.get(&previous_linenumber) {
                Some(previous_indent) => {
                    if previous_indent.line_end + 1 == indent.line_start {
                        let merged = merge_indents(previous_indent, &indent);

                        indent_collection.insert(previous_linenumber, merged);
                    } else {
                        indent_collection.insert(indent.line_start, indent.clone());
                        previous_linenumber = indent.line_start.clone();
                    }
                }
                None => {
                    indent_collection.insert(indent.line_start, indent.clone());
                    previous_linenumber = indent.line_start.clone();
                }
            },
            None => {
                previous_linenumber = 0;
            }
        }
    }

    // Second pass: recalculate directive_column based on actual field lengths at aligned positions
    let mut updated_collection = BTreeMap::new();
    for (line_num, mut field_indent) in indent_collection {
        // Find the maximum directive column by checking actual field lengths in this group
        let max_directive_col = calculate_directive_column(fields, &field_indent, indent_minimum);
        field_indent.directive_column = max_directive_col;
        updated_collection.insert(line_num, field_indent);
    }

    Indentation {
        minimum: indent_minimum,
        levels: updated_collection,
    }
}

fn merge_indents(previous_indent: &FieldIndent, indent: &FieldIndent) -> FieldIndent {
    FieldIndent {
        line_start: previous_indent.line_start,
        line_end: indent.line_end,
        name_column: std::cmp::max(previous_indent.name_column, indent.name_column),
        type_column: std::cmp::max(previous_indent.type_column, indent.type_column),
        directive_column: std::cmp::max(previous_indent.directive_column, indent.directive_column),
    }
}

fn calculate_directive_column(
    fields: &Vec<ast::Field>,
    field_indent: &FieldIndent,
    _indent_minimum: usize,
) -> usize {
    let mut max_directive_start = 0;

    // Find all columns in this alignment group (between line_start and line_end)
    for field in fields {
        match field {
            ast::Field::Column(column) => {
                if let Some(name_loc) = &column.start_name {
                    let line = name_loc.line.to_usize();
                    if line >= field_indent.line_start && line <= field_indent.line_end {
                        // This column is in the current alignment group
                        // Calculate where its directive would start after alignment
                        // type_column is where the type starts, add type length, nullable marker, and space
                        let nullable_space = if column.nullable { 1 } else { 0 };
                        let directive_start = field_indent.type_column
                            + column.type_.to_string().len()
                            + nullable_space
                            + 1; // space before directive
                        max_directive_start = std::cmp::max(max_directive_start, directive_start);
                    }
                }
            }
            _ => {}
        }
    }

    max_directive_start
}

#[derive(Clone, Debug)]
struct FieldIndent {
    line_start: usize,
    line_end: usize,
    name_column: usize,
    type_column: usize,
    directive_column: usize,
}

fn get_field_indent(indent_minimum: usize, field: &ast::Field) -> Option<FieldIndent> {
    match field {
        ast::Field::Column(column) => {
            match (&column.start_name, &column.end_name, &column.end_typename) {
                (Some(name_loc), Some(name_end_loc), Some(end_typename)) => {
                    let apply_offset = |column: usize| -> usize {
                        if indent_minimum > name_loc.column {
                            indent_minimum - name_loc.column
                        } else {
                            if column == 0 {
                                return 0;
                            } else {
                                column - 1
                            }
                        }
                    };

                    let nullable_space = if column.nullable { 1 } else { 0 };

                    return Some(FieldIndent {
                        line_start: name_loc.line.to_usize(),
                        line_end: end_typename.line.to_usize(),
                        name_column: apply_offset(name_loc.column),
                        type_column: apply_offset(name_end_loc.column + 2),
                        directive_column: apply_offset(end_typename.column + 1 + nullable_space),
                    });
                }
                _ => (),
            }
        }
        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
            match (&link.start_name, &link.end_name) {
                (Some(name_loc), Some(name_end_loc)) => {
                    let apply_offset = |column: usize| -> usize {
                        if indent_minimum > name_loc.column {
                            indent_minimum - name_loc.column
                        } else {
                            if column == 0 {
                                return 0;
                            } else {
                                column - 1
                            }
                        }
                    };

                    return Some(FieldIndent {
                        line_start: name_loc.line.to_usize(),
                        line_end: name_loc.line.to_usize(),
                        name_column: apply_offset(name_loc.column),
                        type_column: apply_offset(name_end_loc.column + 2),
                        directive_column: apply_offset(name_end_loc.column + 2),
                    });
                }
                _ => (),
            }
        }
        _ => (),
    }

    None
}

fn to_string_variant(namespace: &str, is_first: bool, variant: &ast::Variant) -> String {
    let prefix = if is_first { " = " } else { " | " };

    match &variant.fields {
        Some(fields) => {
            // Check if variant should be formatted inline
            if should_format_variant_inline(fields, &variant.name, prefix) {
                format_variant_inline(prefix, &variant.name, fields, &variant.inline_comment)
            } else {
                // Format as multiline
                let mut result = format!("  {}{} {{\n", prefix, variant.name);
                let indent_collection: Indentation = collect_indentation(&fields, 8);
                for field in fields {
                    result.push_str(&to_string_field(namespace, &indent_collection, &field));
                }
                result.push_str("     }\n");
                result
            }
        }
        None => {
            let inline_comment = match &variant.inline_comment {
                Some(comment) => format!(" //{}", comment),
                None => String::new(),
            };
            format!("  {}{}{}\n", prefix, variant.name, inline_comment)
        }
    }
}

fn should_format_variant_inline(
    fields: &Vec<ast::Field>,
    variant_name: &str,
    prefix: &str,
) -> bool {
    // If there are any ColumnLines, user explicitly wants multiline
    for field in fields {
        if matches!(field, ast::Field::ColumnLines { .. }) {
            return false;
        }
        if matches!(field, ast::Field::ColumnComment { .. }) {
            return false;
        }
    }

    // Check if all fields are on the same line in the source
    let mut first_line: Option<usize> = None;
    for field in fields {
        if let ast::Field::Column(col) = field {
            if let Some(start) = &col.start {
                match first_line {
                    None => first_line = Some(start.line.to_usize()),
                    Some(line) => {
                        if line != start.line.to_usize() {
                            // Fields are on different lines
                            return false;
                        }
                    }
                }
            }
        }
    }

    // Calculate the length if formatted inline
    let inline_str = format_variant_inline(prefix, variant_name, fields, &None);
    // Check length of the variant line (should be <= 80)
    let variant_line = inline_str.trim_end();

    variant_line.len() <= 80
}

fn format_variant_inline(
    prefix: &str,
    variant_name: &str,
    fields: &Vec<ast::Field>,
    inline_comment: &Option<String>,
) -> String {
    let mut result = format!("  {}{} {{ ", prefix, variant_name);

    let mut first = true;
    for field in fields {
        if let ast::Field::Column(col) = field {
            if !first {
                result.push_str(", ");
            }
            result.push_str(&col.name);
            result.push(' ');
            result.push_str(&col.type_.to_string());
            if col.nullable {
                result.push('?');
            }
            first = false;
        }
    }

    result.push_str(" }");

    if let Some(comment) = inline_comment {
        result.push_str(" //");
        result.push_str(comment);
    }

    result.push_str("\n");
    result
}

fn to_string_field(namespace: &str, indent: &Indentation, field: &ast::Field) -> String {
    match field {
        ast::Field::ColumnLines { count } => "\n".repeat((*count).min(2) as usize),
        ast::Field::Column(column) => to_string_column(indent, column),
        ast::Field::ColumnComment { text } => {
            format!("{}//{}\n", " ".repeat(indent.minimum), text)
        }
        ast::Field::FieldDirective(directive) => {
            to_string_field_directive(namespace, indent, directive)
        }
    }
}

fn to_string_column(indentation: &Indentation, column: &ast::Column) -> String {
    let initial_indent = " ".repeat(indentation.minimum);
    let nullable = if column.nullable { "?" } else { "" };
    let schema_type = schema_type_to_string(&column.type_);

    let mut type_indent_len = 1;
    let mut directive_indent_len = 0;

    let line_number: usize = match &column.start_name {
        Some(loc) => loc.line.to_usize(),
        None => 0,
    };

    let maybe_indent = indentation
        .levels
        .range(..=line_number)
        .next_back()
        .map(|(_, v)| v);

    match maybe_indent {
        Some(indent) => {
            let name_plus_colon = indentation.minimum + 1 + column.name.len();

            if name_plus_colon < indent.type_column {
                type_indent_len = indent.type_column - name_plus_colon;
            }

            let name_plus_colon_plus_type =
                name_plus_colon + type_indent_len + schema_type.len() + nullable.len() + 1;
            if name_plus_colon_plus_type < indent.directive_column && column.directives.len() > 0 {
                directive_indent_len = indent.directive_column - name_plus_colon_plus_type;
            }
        }
        None => (),
    }

    let type_indent = " ".repeat(type_indent_len);
    let directive_indent = " ".repeat(directive_indent_len);
    let directives = to_string_directives(&column.directives);

    let inline_comment = match &column.inline_comment {
        Some(comment) => format!(" //{}", comment),
        None => String::new(),
    };

    format!(
        "{initial_indent}{name}{type_indent}{type_}{nullable}{directive_indent}{directives}{inline_comment}\n",
        initial_indent = initial_indent,
        name = column.name,
        type_indent = type_indent,
        type_ = schema_type,
        nullable = nullable,
        directive_indent = directive_indent,
        directives = directives,
        inline_comment = inline_comment
    )
}

fn schema_type_to_string(type_: &ast::ColumnType) -> String {
    match type_ {
        ast::ColumnType::IdInt { .. } => "Id.Int".to_string(),
        ast::ColumnType::IdUuid { .. } => "Id.Uuid".to_string(),
        _ => type_.to_string(),
    }
}

fn to_string_field_directive(
    namespace: &str,
    indent: &Indentation,
    directive: &ast::FieldDirective,
) -> String {
    let spaces = " ".repeat(indent.minimum);
    match directive {
        ast::FieldDirective::Watched(_) => format!("{}@watch\n", spaces),
        ast::FieldDirective::TableName((_, name)) => {
            format!("{}@tablename(\"{}\")\n", spaces, name)
        }
        ast::FieldDirective::Link(details) => {
            to_string_link_details_shorthand(namespace, indent, details)
        }
        ast::FieldDirective::Index(details) => {
            format!("{}@index{}\n", spaces, index_directive_to_string(details))
        }
        ast::FieldDirective::Unique(details) => {
            format!("{}@unique{}\n", spaces, index_directive_to_string(details))
        }
        ast::FieldDirective::Permissions(info) => {
            to_string_permissions_details(namespace, indent, info)
        }
        ast::FieldDirective::Timestamps => format!("{}@timestamps\n", spaces),
    }
}

fn sort_direction_to_string(direction: &ast::SortDirection) -> &'static str {
    match direction {
        ast::SortDirection::Asc => "asc",
        ast::SortDirection::Desc => "desc",
    }
}

fn index_directive_to_string(details: &ast::IndexDirective) -> String {
    let columns = details
        .columns
        .iter()
        .map(|c| format!("{} {}", c.name, sort_direction_to_string(&c.direction)))
        .collect::<Vec<String>>()
        .join(", ");

    let mut result = format!("({})", columns);

    if let Some(where_) = &details.where_ {
        result.push_str(" where ");
        result.push_str(&format_where_for_braces(where_, 0));
    }

    result
}

fn to_string_permissions_details(
    _namespace: &str,
    indentation: &Indentation,
    details: &ast::PermissionDetails,
) -> String {
    let spaces = " ".repeat(indentation.minimum);
    match details {
        ast::PermissionDetails::Public => {
            format!("{}@public\n", spaces)
        }
        ast::PermissionDetails::Star(where_) => format_permissions_where(spaces, where_),
        ast::PermissionDetails::OnOperation(operations) => {
            let mut result = String::new();

            // For each operation group, output a separate @allow(query, update) { ... } directive
            for op in operations {
                let ops = op
                    .operations
                    .iter()
                    .map(|o| match o {
                        ast::QueryOperation::Query => "query",
                        ast::QueryOperation::Insert => "insert",
                        ast::QueryOperation::Update => "update",
                        ast::QueryOperation::Delete => "delete",
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let where_content =
                    format_where_for_braces_with_mode(&op.where_, indentation.minimum, true);
                result.push_str(&format!("{}@allow({}) {}\n", spaces, ops, where_content));
            }
            result
        }
    }
}

fn format_permissions_where(indent: String, where_arg: &ast::WhereArg) -> String {
    let content = format_where_for_braces_with_mode(where_arg, indent.len(), true);
    format!("{}@allow(*) {}\n", indent, content)
}

fn format_where_for_braces(where_arg: &ast::WhereArg, base_indent: usize) -> String {
    format_where_for_braces_with_mode(where_arg, base_indent, false)
}

fn format_where_for_braces_with_mode(
    where_arg: &ast::WhereArg,
    base_indent: usize,
    force_logical_multiline: bool,
) -> String {
    let content = format_where_content(where_arg, base_indent, force_logical_multiline);
    format!("{{{} }}", content)
}

fn format_where_content(
    where_arg: &ast::WhereArg,
    base_indent: usize,
    force_logical_multiline: bool,
) -> String {
    // Check if this is a single expression (Column) or multiple expressions (And/Or)
    match where_arg {
        ast::WhereArg::Column(..) => {
            // Single expression: format as  userId = Session.userId  with spaces
            format!(" {} ", format_where(where_arg))
        }
        ast::WhereArg::Exists(path, body) => format!(
            "\n{}{}\n{}",
            " ".repeat(base_indent + 4),
            format_exists(path, body, base_indent + 4),
            " ".repeat(base_indent)
        ),
        ast::WhereArg::And(args) => {
            if args.len() == 1 {
                // Single item in And - treat as single expression
                format_where_content(&args[0], base_indent, force_logical_multiline)
            } else {
                // Multiple expressions in permission directives: force multi-line with explicit operators.
                if force_logical_multiline {
                    let mut result = String::from("\n");
                    let inner_indent = " ".repeat(base_indent + 4);
                    for (i, arg) in args.iter().enumerate() {
                        if i != 0 {
                            result.push_str(&format!("{}&& ", inner_indent));
                        } else {
                            result.push_str(&inner_indent);
                        }
                        result.push_str(&format_where_at(arg, base_indent + 4));
                        result.push_str("\n");
                    }
                    result.push_str(&" ".repeat(base_indent));
                    return result;
                }

                // Multiple expressions: format as multi-line (newlines act as separators, no commas)
                let mut result = String::from("\n");
                let inner_indent = " ".repeat(base_indent + 4);
                for arg in args {
                    result.push_str(&format!(
                        "{}{}\n",
                        inner_indent,
                        format_where_at(arg, base_indent + 4)
                    ));
                }
                result.push_str(&format!("{}", " ".repeat(base_indent)));
                result
            }
        }
        ast::WhereArg::Or(args) => {
            if args.len() == 1 {
                // Single item in Or - treat as single expression
                format_where_content(&args[0], base_indent, force_logical_multiline)
            } else {
                if force_logical_multiline {
                    let mut result = String::from("\n");
                    let inner_indent = " ".repeat(base_indent + 4);
                    for (i, arg) in args.iter().enumerate() {
                        if i != 0 {
                            result.push_str(&format!("{}|| ", inner_indent));
                        } else {
                            result.push_str(&inner_indent);
                        }
                        result.push_str(&format_where_at(arg, base_indent + 4));
                        result.push_str("\n");
                    }
                    result.push_str(&" ".repeat(base_indent));
                    return result;
                }

                // Check if all items are simple (Column or And/Or with only Column items) - if so, format as single-line
                let all_simple = args.iter().all(|arg| match arg {
                    ast::WhereArg::Column(..) => true,
                    ast::WhereArg::And(items) => {
                        items.len() <= 2
                            && items.iter().all(|a| matches!(a, ast::WhereArg::Column(..)))
                    }
                    ast::WhereArg::Or(items) => {
                        items.len() <= 2
                            && items.iter().all(|a| matches!(a, ast::WhereArg::Column(..)))
                    }
                    ast::WhereArg::Exists(..) => false,
                });

                if all_simple {
                    // Format as single-line using format_where
                    format!(" {} ", format_where_at(where_arg, base_indent))
                } else {
                    // Multiple expressions: format as multi-line with || separator
                    let mut result = String::from("\n");
                    let inner_indent = " ".repeat(base_indent + 4);
                    for (i, arg) in args.iter().enumerate() {
                        if i != 0 {
                            result.push_str(&format!("{}|| ", inner_indent));
                        } else {
                            result.push_str(&inner_indent);
                        }
                        result.push_str(&format_where_at(arg, base_indent + 4));
                        result.push_str("\n");
                    }
                    result.push_str(&format!("{}", " ".repeat(base_indent)));
                    result
                }
            }
        }
    }
}

fn to_string_link_details_shorthand(
    namespace: &str,
    indentation: &Indentation,
    details: &ast::LinkDetails,
) -> String {
    let effective_namespace = if namespace.is_empty() {
        ast::DEFAULT_SCHEMANAME
    } else {
        namespace
    };

    let spaces = " ".repeat(indentation.minimum);
    let mut result = format!("{}{}", spaces, details.link_name);

    let line_number: usize = match &details.start_name {
        Some(loc) => loc.line.to_usize(),
        None => 0,
    };

    let mut type_indent_len = 1;

    let maybe_indent = indentation
        .levels
        .range(..=line_number)
        .next_back()
        .map(|(_, v)| v);

    match maybe_indent {
        Some(indent) => {
            let name_plus_colon = indentation.minimum + 1 + details.link_name.len();

            if name_plus_colon < indent.type_column {
                type_indent_len = indent.type_column - name_plus_colon;
            }

            result.push_str(&" ".repeat(type_indent_len));
        }
        None => result.push_str(" "),
    }

    result.push_str("@link(");
    let mut added = false;
    for id in &details.local_ids {
        if added {
            result.push_str(", ");
        }
        if id == "id" {
            continue;
        } else {
            result.push_str(id);
        }
        added = true
    }
    for id in &details.foreign.fields {
        if added {
            result.push_str(", ");
        }

        if details.foreign.schema != effective_namespace {
            result.push_str(&details.foreign.schema);
            result.push('.');
        }
        result.push_str(&details.foreign.table);
        result.push_str(".");
        result.push_str(id);
        added = true
    }

    result.push_str(")");

    if let Some(comment) = &details.inline_comment {
        result.push_str(" //");
        result.push_str(comment);
    }

    result.push_str("\n");

    result
}

fn to_string_directives(directives: &Vec<ast::ColumnDirective>) -> String {
    let mut result = String::new();
    for directive in directives {
        result.push_str(" ");
        result.push_str(&to_string_directive(directive));
    }
    result
}

fn to_string_directive(directive: &ast::ColumnDirective) -> String {
    match directive {
        ast::ColumnDirective::PrimaryKey => "@id".to_string(),
        ast::ColumnDirective::Unique => "@unique".to_string(),
        ast::ColumnDirective::Index => "@index".to_string(),
        ast::ColumnDirective::CreatedAt => "@createdAt".to_string(),
        ast::ColumnDirective::UpdatedAt => "@updatedAt".to_string(),
        ast::ColumnDirective::Default { id: _, value, .. } => match value {
            ast::DefaultValue::Now => "@default(now)".to_string(),
            ast::DefaultValue::Value(value) => {
                format!("@default({})", &value_to_string(value))
            }
        },
    }
}

//
pub fn query(query_list: &ast::QueryList) -> String {
    if query_list.queries.is_empty() {
        return "\n\n".to_string();
    }

    let mut result = String::new();
    // Skip trailing QueryLines - we'll handle them with normalization
    let mut last_non_lines_idx = None;

    // Find the last non-QueryLines element
    for (idx, operation) in query_list.queries.iter().enumerate().rev() {
        match operation {
            ast::QueryDef::QueryLines { .. } => continue,
            _ => {
                last_non_lines_idx = Some(idx);
                break;
            }
        }
    }

    // Convert all queries up to and including the last non-QueryLines element
    // Skip QueryLines elements as they're just formatting whitespace that we'll normalize
    if let Some(last_idx) = last_non_lines_idx {
        for operation in query_list.queries.iter().take(last_idx + 1) {
            match operation {
                ast::QueryDef::QueryLines { .. } => {
                    // Skip QueryLines - we'll handle trailing newlines below
                }
                _ => {
                    result.push_str(&to_string_query_definition(operation));
                }
            }
        }
    }

    // Ensure exactly 2 newlines at the end
    // Remove all trailing newlines first
    while result.ends_with('\n') {
        result.pop();
    }
    // Add exactly 2 newlines
    result.push_str("\n\n");

    result
}

fn to_string_query_definition(definition: &ast::QueryDef) -> String {
    match definition {
        ast::QueryDef::Query(q) => to_string_query(q),
        ast::QueryDef::QueryComment { text } => format!("//{}\n", text),
        ast::QueryDef::QueryLines { count } => "\n".repeat((*count).min(2) as usize),
    }
}

fn to_string_query(query: &ast::Query) -> String {
    let operation_name = match &query.operation {
        ast::QueryOperation::Query => "query",
        ast::QueryOperation::Insert => "insert",
        ast::QueryOperation::Delete => "delete",
        ast::QueryOperation::Update => "update",
    };
    let mut result = format!("{} {}", operation_name, query.name);

    if query.args.len() > 0 {
        result.push_str("(");
    }
    let mut first = true;
    for param in &query.args {
        result.push_str(&to_string_param_definition(first, &param));
        first = false;
    }
    if query.args.len() > 0 {
        result.push_str(")");
    }

    // Fields
    result.push_str(" {\n");

    for field in &query.fields {
        result.push_str(&to_string_toplevel_query_field(4, &field));
    }
    result.push_str("}\n");
    result
}

fn to_string_toplevel_query_field(indent: usize, field: &ast::TopLevelQueryField) -> String {
    match field {
        ast::TopLevelQueryField::Field(query_field) => to_string_query_field(indent, query_field),
        ast::TopLevelQueryField::Lines { count } => "\n".repeat((*count).min(2) as usize),
        ast::TopLevelQueryField::Comment { text } => format!("//{}\n", text),
    }
}

// Example: ($arg: String)
fn to_string_param_definition(is_first: bool, param: &ast::QueryParamDefinition) -> String {
    if is_first {
        match &param.type_ {
            None => return format!("${}", param.name),
            Some(type_) => {
                let nullable_marker = if param.nullable { "?" } else { "" };
                return format!("${}: {}{}", param.name, type_, nullable_marker);
            }
        }
    } else {
        match &param.type_ {
            None => return format!(", ${}", param.name),
            Some(type_) => {
                let nullable_marker = if param.nullable { "?" } else { "" };
                return format!(", ${}: {}{}", param.name, type_, nullable_marker);
            }
        }
    }
}

fn to_string_field_arg(indent: usize, field_arg: &ast::ArgField) -> String {
    match field_arg {
        ast::ArgField::Arg(arg) => to_string_param(indent, &arg.arg),
        ast::ArgField::Field(field) => to_string_query_field(indent, field),
        ast::ArgField::Lines { count } => "\n".repeat((*count).min(2) as usize),
        ast::ArgField::QueryComment { text } => {
            format!("{}//{}\n", " ".repeat(indent), text)
        }
    }
}

fn to_string_query_field(indent: usize, field: &ast::QueryField) -> String {
    let spaces = " ".repeat(indent);
    let alias_string = match &field.alias {
        Some(alias) => format!("{}: ", alias),
        None => "".to_string(),
    };

    let mut result = format!("{}{}{}", spaces, alias_string, field.name);

    match &field.set {
        Some(val) => {
            result.push_str(" = ");
            result.push_str(&value_to_string(val));
        }
        None => {}
    }

    if field.fields.len() > 0 {
        result.push_str(" {\n");
    }

    // Fields
    for inner_field in &field.fields {
        result.push_str(&to_string_field_arg(indent + 4, &inner_field));
    }
    if field.fields.len() > 0 {
        result.push_str(&spaces);
        result.push_str("}");
    }
    result.push_str("\n");
    result
}

// Example: (arg = $id)
fn to_string_param(indent_size: usize, arg: &ast::Arg) -> String {
    let indent = " ".repeat(indent_size);
    match arg {
        ast::Arg::Limit(lim) => {
            format!("{}@limit({})\n", indent, value_to_string(lim))
        }
        ast::Arg::OrderBy(direction, column) => {
            format!(
                "{}@sort({}, {})\n",
                indent,
                column,
                ast::direction_to_string(direction)
            )
        }
        ast::Arg::Where(where_arg) => {
            let content = format_where_for_braces(where_arg, indent_size);
            format!("{}@where {}\n", indent, content)
        }
    }
}

fn format_where(where_arg: &ast::WhereArg) -> String {
    format_where_at(where_arg, 0)
}

fn format_exists(
    path: &[(String, ast::Range)],
    body: &ast::WhereArg,
    base_indent: usize,
) -> String {
    format!(
        "exists {} {}",
        path.iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        format_where_for_braces_with_mode(body, base_indent, true)
    )
}

fn format_where_at(where_arg: &ast::WhereArg, base_indent: usize) -> String {
    match where_arg {
        ast::WhereArg::Exists(path, body) => format_exists(path, body, base_indent),
        ast::WhereArg::Column(is_session_var, column, operator, value, _field_name_range) => {
            let column_name = if *is_session_var {
                format!("Session.{}", column)
            } else {
                column.clone()
            };
            let operator = operator_to_string(&operator);
            let value = value_to_string(&value);
            format!("{} {} {}", column_name, operator, value)
        }
        ast::WhereArg::And(and) => {
            let mut result = String::new();
            let last_index = and.len() - 1;
            for (i, arg) in and.iter().enumerate() {
                result.push_str(&format_where_at(arg, base_indent));
                if i != last_index {
                    result.push_str(" && ");
                }
            }
            result
        }
        ast::WhereArg::Or(or) => {
            let mut result = String::new();
            let last_index = or.len() - 1;
            for (i, arg) in or.iter().enumerate() {
                result.push_str(&format_where_at(arg, base_indent));
                if i != last_index {
                    result.push_str(" || ");
                }
            }
            result
        }
    }
}

fn value_to_string(value: &ast::QueryValue) -> String {
    match value {
        ast::QueryValue::Fn(func) => format!(
            "{}({})",
            func.name,
            func.args
                .iter()
                .map(|value| value_to_string(value))
                .collect::<Vec<String>>()
                .join(", ")
        ),
        ast::QueryValue::Variable((_, var)) => ast::to_pyre_variable_name(var),
        ast::QueryValue::String((_, value)) => format!("\"{}\"", value),
        ast::QueryValue::Int((_, value)) => value.to_string(),
        ast::QueryValue::Float((_, value)) => value.to_string(),
        ast::QueryValue::Bool((_, true)) => "True".to_string(),
        ast::QueryValue::Bool((_, false)) => "False".to_string(),
        ast::QueryValue::Null(_) => "null".to_string(),
        ast::QueryValue::LiteralTypeValue((_, details)) => details.name.clone(),
    }
}

fn operator_to_string(operator: &ast::Operator) -> &str {
    match operator {
        ast::Operator::Equal => "==",
        ast::Operator::NotEqual => "!=",
        ast::Operator::GreaterThan => ">",
        ast::Operator::LessThan => "<",
        ast::Operator::GreaterThanOrEqual => ">=",
        ast::Operator::LessThanOrEqual => "<=",
        ast::Operator::In => "in",
        ast::Operator::NotIn => "not in",
        ast::Operator::Like => "like",
        ast::Operator::NotLike => "not like",
    }
}
