use crate::ast;
use crate::typecheck;
use nom::ToUsize;
use std::collections::HashSet;

pub fn standalone_schema_to_string(context: &typecheck::Context, schema: &ast::Schema) -> String {
    let mut standalone = schema.clone();
    standalone.session = context.session.clone();

    let local_types = standalone
        .files
        .iter()
        .flat_map(|file| &file.definitions)
        .filter_map(|definition| match definition {
            ast::Definition::Tagged { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut required_types = Vec::new();

    if let Some(session) = &standalone.session {
        collect_field_type_names(&session.fields, &mut required_types);
    }
    for file in &standalone.files {
        for definition in &file.definitions {
            match definition {
                ast::Definition::Record { fields, .. } => {
                    collect_field_type_names(fields, &mut required_types)
                }
                ast::Definition::Tagged { variants, .. } => {
                    collect_variant_type_names(variants, &mut required_types)
                }
                _ => {}
            }
        }
    }

    let mut included_types = local_types.clone();
    let mut external_definitions = Vec::new();
    while let Some(name) = required_types.pop() {
        if !included_types.insert(name.clone()) {
            continue;
        }
        let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(&name) else {
            continue;
        };

        collect_variant_type_names(variants, &mut required_types);
        external_definitions.push(ast::Definition::Tagged {
            name,
            variants: variants.clone(),
            start: None,
            end: None,
        });
    }

    if !external_definitions.is_empty() {
        external_definitions.sort_by(|left, right| {
            let ast::Definition::Tagged { name: left, .. } = left else {
                unreachable!()
            };
            let ast::Definition::Tagged { name: right, .. } = right else {
                unreachable!()
            };
            left.cmp(right)
        });
        standalone.files.insert(
            0,
            ast::SchemaFile {
                path: "session-types.pyre".to_string(),
                definitions: external_definitions,
            },
        );
    }

    schema_to_string("", &standalone)
}

fn collect_field_type_names(fields: &[ast::Field], names: &mut Vec<String>) {
    for field in fields {
        if let ast::Field::Column(column) = field {
            column.type_.collect_custom_type_names(names);
        }
    }
}

fn collect_variant_type_names(variants: &[ast::Variant], names: &mut Vec<String>) {
    for variant in variants {
        if let Some(fields) = &variant.fields {
            collect_field_type_names(fields, names);
        }
    }
}

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
            for (index, field) in session.fields.iter().enumerate() {
                result.push_str(&to_string_field(
                    namespace,
                    &indent_collection,
                    index,
                    field,
                ));
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
            for (index, field) in fields.iter().enumerate() {
                result.push_str(&to_string_field(
                    namespace,
                    &indent_collection,
                    index,
                    field,
                ));
            }
            result.push_str("}\n");
            result
        }
    }
}

#[derive(Debug)]
struct Indentation {
    minimum: usize,
    levels: Vec<Option<FieldIndent>>,
}

fn collect_indentation(fields: &Vec<ast::Field>, indent_minimum: usize) -> Indentation {
    let mut levels = vec![None; fields.len()];
    let mut group_start = None;

    for index in 0..=fields.len() {
        let alignable = fields.get(index).is_some_and(|field| {
            matches!(
                field,
                ast::Field::Column(_) | ast::Field::FieldDirective(ast::FieldDirective::Link(_))
            )
        });

        if alignable {
            group_start.get_or_insert(index);
            continue;
        }

        if let Some(start) = group_start.take() {
            let group = &fields[start..index];
            let indent = FieldIndent {
                name_width: group
                    .iter()
                    .filter_map(field_name)
                    .map(str::len)
                    .max()
                    .unwrap_or(0),
                type_width: group
                    .iter()
                    .filter_map(|field| match field {
                        ast::Field::Column(column) => Some(
                            schema_type_to_string(&column.type_).len()
                                + usize::from(column.nullable),
                        ),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0),
            };

            for level in &mut levels[start..index] {
                *level = Some(indent.clone());
            }
        }
    }

    Indentation {
        minimum: indent_minimum,
        levels,
    }
}

#[derive(Clone, Debug)]
struct FieldIndent {
    name_width: usize,
    type_width: usize,
}

fn field_name(field: &ast::Field) -> Option<&str> {
    match field {
        ast::Field::Column(column) => Some(&column.name),
        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => Some(&link.link_name),
        _ => None,
    }
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
                for (index, field) in fields.iter().enumerate() {
                    result.push_str(&to_string_field(
                        namespace,
                        &indent_collection,
                        index,
                        field,
                    ));
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

fn to_string_field(
    namespace: &str,
    indent: &Indentation,
    field_index: usize,
    field: &ast::Field,
) -> String {
    match field {
        ast::Field::ColumnLines { count } => "\n".repeat((*count).min(2) as usize),
        ast::Field::Column(column) => to_string_column(indent, field_index, column),
        ast::Field::ColumnComment { text } => {
            format!("{}//{}\n", " ".repeat(indent.minimum), text)
        }
        ast::Field::FieldDirective(directive) => {
            to_string_field_directive(namespace, indent, field_index, directive)
        }
    }
}

fn to_string_column(indentation: &Indentation, field_index: usize, column: &ast::Column) -> String {
    let initial_indent = " ".repeat(indentation.minimum);
    let nullable = if column.nullable { "?" } else { "" };
    let schema_type = schema_type_to_string(&column.type_);

    let mut type_indent_len = 1;
    let mut directive_indent_len = 0;

    let maybe_indent = indentation.levels.get(field_index).and_then(Option::as_ref);

    match maybe_indent {
        Some(indent) => {
            type_indent_len = indent.name_width - column.name.len() + 1;
            if !column.directives.is_empty() {
                directive_indent_len = indent.type_width - schema_type.len() - nullable.len();
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
    field_index: usize,
    directive: &ast::FieldDirective,
) -> String {
    let spaces = " ".repeat(indent.minimum);
    match directive {
        ast::FieldDirective::Watched(_) => format!("{}@watch\n", spaces),
        ast::FieldDirective::TableName((_, name)) => {
            format!("{}@tablename(\"{}\")\n", spaces, name)
        }
        ast::FieldDirective::Link(details) => {
            to_string_link_details_shorthand(namespace, indent, field_index, details)
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
        ast::FieldDirective::Singleton => format!("{}@singleton\n", spaces),
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
                        ast::QueryOperation::Transaction => "transaction",
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let where_content = format_where_for_braces(&op.where_, indentation.minimum);
                result.push_str(&format!("{}@allow({}) {}\n", spaces, ops, where_content));
            }
            result
        }
    }
}

fn format_permissions_where(indent: String, where_arg: &ast::WhereArg) -> String {
    let content = format_where_for_braces(where_arg, indent.len());
    format!("{}@allow(*) {}\n", indent, content)
}

fn format_where_for_braces(where_arg: &ast::WhereArg, base_indent: usize) -> String {
    match where_arg {
        ast::WhereArg::Constant(value) => {
            format!("{{ {} }}", if *value { "True" } else { "False" })
        }
        ast::WhereArg::Column(_, _, _, value, _) if value_is_multiline(value) => format!(
            "{{\n{}\n{}}}",
            format_where_at(where_arg, base_indent + 4),
            " ".repeat(base_indent)
        ),
        ast::WhereArg::Column(..) => {
            format!("{{ {} }}", format_where_leaf(where_arg, base_indent))
        }
        ast::WhereArg::And(args) => {
            if let [arg] = args.as_slice() {
                return format_where_for_braces(arg, base_indent);
            }

            let mut result = String::from("{\n");
            for arg in args {
                result.push_str(&format_where_at(arg, base_indent + 4));
                result.push('\n');
            }
            result.push_str(&" ".repeat(base_indent));
            result.push('}');
            result
        }
        _ => format!(
            "{{\n{}\n{}}}",
            format_where_at(where_arg, base_indent + 4),
            " ".repeat(base_indent)
        ),
    }
}

fn to_string_link_details_shorthand(
    namespace: &str,
    indentation: &Indentation,
    field_index: usize,
    details: &ast::LinkDetails,
) -> String {
    let effective_namespace = if namespace.is_empty() {
        ast::DEFAULT_SCHEMANAME
    } else {
        namespace
    };

    let spaces = " ".repeat(indentation.minimum);
    let mut result = format!("{}{}", spaces, details.link_name);

    let type_indent_len = indentation
        .levels
        .get(field_index)
        .and_then(Option::as_ref)
        .map(|indent| indent.name_width - details.link_name.len() + 1)
        .unwrap_or(1);
    result.push_str(&" ".repeat(type_indent_len));

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
        ast::ColumnDirective::Immutable => "@immutable".to_string(),
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
        ast::QueryOperation::Transaction => "transaction",
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
        if query.operation == ast::QueryOperation::Transaction {
            result.push_str(&to_string_transaction_field(4, field));
        } else {
            result.push_str(&to_string_toplevel_query_field(4, field));
        }
    }
    result.push_str("}\n");
    result
}

fn to_string_transaction_field(indent: usize, field: &ast::TopLevelQueryField) -> String {
    match field {
        ast::TopLevelQueryField::Field(query_field) => {
            let mut result = format!(
                "{}{} ",
                " ".repeat(indent),
                query_field
                    .operation
                    .as_ref()
                    .expect("transaction field operation")
                    .as_str()
            );
            result.push_str(to_string_query_field(0, query_field).trim_start());
            result
        }
        _ => to_string_toplevel_query_field(indent, field),
    }
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
            result.push_str(&value_to_string_at(val, indent));
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
            format!(
                "{}@limit({})\n",
                indent,
                value_to_string_at(lim, indent_size)
            )
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

fn format_where_at(where_arg: &ast::WhereArg, base_indent: usize) -> String {
    let indent = " ".repeat(base_indent);
    match where_arg {
        ast::WhereArg::Constant(value) => {
            format!("{}{}", indent, if *value { "True" } else { "False" })
        }
        ast::WhereArg::Exists(path, body) => format!(
            "{}exists {} {}",
            indent,
            path.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            format_where_for_braces(body, base_indent)
        ),
        ast::WhereArg::Column(..) => {
            format!("{}{}", indent, format_where_leaf(where_arg, base_indent))
        }
        ast::WhereArg::And(args) => format_logical_where("And", args, base_indent),
        ast::WhereArg::Or(args) => format_logical_where("Or", args, base_indent),
    }
}

fn format_where_leaf(where_arg: &ast::WhereArg, base_indent: usize) -> String {
    match where_arg {
        ast::WhereArg::Column(is_session_var, path, operator, value, _field_name_range) => {
            let column = path.authored();
            let column_name = if *is_session_var {
                format!("Session.{}", column)
            } else {
                column
            };
            let operator = operator_to_string(&operator);
            let value = value_to_string_at(value, base_indent);
            format!("{} {} {}", column_name, operator, value)
        }
        _ => unreachable!("compound predicates are formatted recursively"),
    }
}

fn format_logical_where(name: &str, args: &[ast::WhereArg], base_indent: usize) -> String {
    if let [arg] = args {
        return format_where_at(arg, base_indent);
    }

    let indent = " ".repeat(base_indent);
    let mut result = format!("{}{}(\n", indent, name);
    for arg in args {
        result.push_str(&format_where_at(arg, base_indent + 4));
        result.push_str(",\n");
    }
    result.push_str(&indent);
    result.push(')');
    result
}

fn value_to_string(value: &ast::QueryValue) -> String {
    value_to_string_at(value, 0)
}

fn value_to_string_at(value: &ast::QueryValue, base_indent: usize) -> String {
    match value {
        ast::QueryValue::Fn(func) => format!(
            "{}({})",
            func.name,
            func.args
                .iter()
                .map(|value| value_to_string_at(value, base_indent))
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
        ast::QueryValue::LiteralTypeValue((_, details)) => match &details.fields {
            Some(fields) if fields.len() == 1 && !value_is_multiline(&fields[0].1) => format!(
                "{} {{ {} = {} }}",
                details.name,
                fields[0].0,
                value_to_string_at(&fields[0].1, base_indent)
            ),
            Some(fields) if !fields.is_empty() => {
                let field_indent = " ".repeat(base_indent + 4);
                let closing_indent = " ".repeat(base_indent);
                let mut result = format!("{} {{\n", details.name);
                for (name, value) in fields {
                    result.push_str(&field_indent);
                    result.push_str(name);
                    result.push_str(" = ");
                    result.push_str(&value_to_string_at(value, base_indent + 4));
                    result.push('\n');
                }
                result.push_str(&closing_indent);
                result.push('}');
                result
            }
            _ => details.name.clone(),
        },
    }
}

fn value_is_multiline(value: &ast::QueryValue) -> bool {
    match value {
        ast::QueryValue::Fn(func) => func.args.iter().any(value_is_multiline),
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            details.fields.as_ref().is_some_and(|fields| {
                fields.len() > 1 || fields.iter().any(|(_, value)| value_is_multiline(value))
            })
        }
        _ => false,
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
