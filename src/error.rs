use crate::ast;
use crate::color;
use nom::ToUsize;
use serde::{Deserialize, Serialize};
#[cfg(feature = "json")]
use serde_json;
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Error {
    pub error_type: ErrorType,
    pub filepath: String,
    pub locations: Vec<Location>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParsingErrorDetails {
    pub expecting: Expecting,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum Expecting {
    PyreFile,
    // Query stuff
    ParamDefinition,
    ParamDefType,
    AtDirective,

    // Schema stuff
    SchemaAtDirective,
    SchemaFieldAtDirective,
    SchemaColumn,

    LinkDirective,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum ErrorType {
    ParsingError(ParsingErrorDetails),

    UnknownFunction {
        found: String,
        known_functions: Vec<String>,
    },
    MultipleSessionDefinitions,
    MissingType,
    DuplicateDefinition(String),
    DefinitionIsBuiltIn(String),
    GeneratedCrudNameCollision {
        name: String,
        table: String,
        operation: ast::QueryOperation,
    },
    DuplicateField {
        record: String,
        field: String,
    },
    DuplicateVariant {
        base_variant: VariantDef,
        duplicates: Vec<VariantDef>,
    },
    VariantFieldTypeCollision {
        field: String,
        type_one: String,
        type_two: String,
        variant_one: String,
        variant_two: String,
    },
    UnknownType {
        found: String,
        known_types: Vec<String>,
    },
    InvalidTypeUsage {
        message: String,
    },
    NoPrimaryKey {
        record: String,
    },
    MultiplePrimaryKeys {
        record: String,
        field: String,
    },
    MultipleTableNames {
        record: String,
    },
    MultiplePermissions {
        record: String,
    },
    MissingPermissions {
        record: String,
    },
    InvalidRecordIndexField {
        record: String,
        directive: String,
        field: String,
        known_fields: Vec<String>,
    },
    DuplicateRecordIndexField {
        record: String,
        directive: String,
        field: String,
    },
    // Schema Link errors
    LinkToUnknownTable {
        link_name: String,
        unknown_table: String,
    },

    LinkToUnknownField {
        link_name: String,
        unknown_local_field: String,
    },
    LinkToUnknownForeignField {
        link_name: String,
        foreign_table: String,
        unknown_foreign_field: String,
    },
    LinkSelectionIsEmpty {
        link_name: String,
        foreign_table: String,
        foreign_table_fields: Vec<(String, String)>,
    },
    LinkToUnknownSchema {
        unknown_schema_name: String,
        known_schemas: HashSet<String>,
    },

    // Foreign Key Reference Errors
    ForeignKeyToUnknownTable {
        field_name: String,
        referenced_table: String,
        existing_tables: Vec<String>,
    },
    ForeignKeyToUnknownField {
        field_name: String,
        referenced_table: String,
        referenced_field: String,
        existing_fields: Vec<String>,
    },
    ForeignKeyToNonIdField {
        field_name: String,
        referenced_table: String,
        referenced_field: String,
        referenced_field_type: String,
    },

    // Query Validation Errors
    UnknownTable {
        found: String,
        existing: Vec<String>,
    },
    DuplicateQueryField {
        field: String,
    },
    InvalidWildcardSelection {
        reason: InvalidWildcardReason,
    },
    NoFieldsSelected,
    UnknownField {
        found: String,

        record_name: String,
        known_fields: Vec<(String, String)>,
    },
    MultipleLimits {
        query: String,
    },
    MultipleWheres {
        query: String,
    },
    WhereOnLinkIsntAllowed {
        link_name: String,
    },
    InvalidRelationalPermission {
        message: String,
    },
    SyncedRelationalQueryPermission,
    TypeMismatch {
        table: String,
        column_defined_as: String,
        variable_name: String,
        variable_defined_as: String,
    },
    LiteralTypeMismatch {
        expecting_type: String,
        found: String,
    },
    LiteralTypeMismatchVariant {
        found: String,
        expecting_type: String,
        variants: Vec<String>,
    },
    UnusedParam {
        param: String,
    },
    UndefinedParam {
        param: String,
        type_: Option<String>,
    },
    NoSetsInSelect {
        field: String,
    },
    NoSetsInDelete {
        field: String,
    },
    LinksDisallowedInInserts {
        field: String,
        table_name: String,
        local_ids: Vec<String>,
    },
    LinksDisallowedInDeletes {
        field: String,
    },
    LinksDisallowedInUpdates {
        field: String,
    },

    InsertColumnIsNotSet {
        field: String,
    },
    InsertMissingColumn {
        table_name: String,
        fields: Vec<String>,
    },
    InsertNestedValueAutomaticallySet {
        field: String,
    },
    ManagedColumnCannotBeSet {
        field: String,
        operation: ast::QueryOperation,
    },
    MultipleSchemaWrites {
        field_table: String,
        field_schema: String,
        operation: ast::QueryOperation,
        other_schemas: Vec<String>,
    },
    LimitOffsetOnlyInFlatRecord,

    // Schema Diff Errors
    MigrationTableDropped {
        table_name: String,
    },
    MigrationColumnDropped {
        table_name: String,
        column_name: String,
        added_columns: Vec<String>,
    },
    MigrationColumnModified {
        table_name: String,
        column_name: String,
        changes: ColumnDiff,
    },
    MigrationVariantRemoved {
        tagged_name: String,
        variant_name: String,
    },
    InvalidColumnDefault {
        field_name: String,
        field_type: String,
        default_value: String,
        expected: Vec<String>,
    },
    MigrationSchemaNotFound {
        namespace: Option<String>,
    },
    MigrationMissingSchema,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum DefInfo {
    Def(Option<Range>),
    Builtin,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VariantDef {
    pub typename: String,
    pub variant_name: String,
    pub filepath: String,
    pub range: Option<Range>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum InvalidWildcardReason {
    Aliased,
    Assigned,
    HasNestedSelections,
    UsedInInsertOrUpdate,
}

/*


    For tracking location errors, we have a few different considerations.

    1. Generally a language server takes a single range, so that should easily be retrievable.
    2. For error rendering in the terminal, we want a hierarchy of the contexts we're in.
        So, we want
            - The Query
            - The table field, etc.

*/

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Location {
    pub contexts: Vec<Range>,
    pub primary: Vec<Range>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Range {
    pub start: ast::Location,
    pub end: ast::Location,
}

pub fn format_custom_error(title: &str, body: &str) -> String {
    format!(
        "{}-------------{}\n\n{}",
        title,
        "-".repeat(title.len()),
        body
    )
}

/* Error formats!



{File name}-------------{Error title}

   | record User {
   |    ...
12 |    status: Stats
   |            ^^^^^
   | }

I don't recognize this type. Is it one of these?

   Status




*/

pub fn format_error(file_contents: &str, error: &Error, enable_color: bool) -> String {
    let filepath = &error.filepath;
    let path_length = filepath.len();
    let separator = "-".repeat(50usize.saturating_sub(path_length));

    let highlight = prepare_highlight(file_contents, &error, enable_color);
    let description = to_error_description(&error, enable_color);

    format!(
        "{} {}\n\n{}\n    {}\n",
        color::cyan(enable_color, filepath),
        color::cyan(enable_color, &separator),
        highlight,
        description
    )
}

fn prepare_highlight(file_contents: &str, error: &Error, enable_color: bool) -> String {
    let mut rendered = "".to_string();
    let mut has_rendered = false;
    for location in &error.locations {
        if has_rendered {
            rendered.push_str("\n\n");
        }
        render_highlight_location(file_contents, &mut rendered, &location, enable_color);
        has_rendered = true;
    }
    rendered
}

fn divider(indent: usize, enable_color: bool) -> String {
    color::gray(
        enable_color,
        &format!("    | {}...\n", " ".repeat(indent * 4)),
    )
}

fn join_hashset(set: &HashSet<String>, sep: &str) -> String {
    set.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(sep)
}

fn render_highlight_location(
    file_contents: &str,
    rendered: &mut String,
    location: &Location,
    enable_color: bool,
) {
    let mut indent: usize = 0;

    let mut last_line_index: usize = 0;
    let mut first_rendered = false;
    let mut rendered_contexts = vec![false; location.contexts.len()];

    // Helper function to check if a line number is in any primary range
    let line_in_primary = |line: u32| -> bool {
        location
            .primary
            .iter()
            .any(|primary| primary.start.line <= line && line <= primary.end.line)
    };

    let mut first_primary_rendered = false;
    let mut context_index = 0;
    for primary in &location.primary {
        while context_index < location.contexts.len()
            && location.contexts[context_index].start.line <= primary.start.line
        {
            let context = &location.contexts[context_index];
            // Skip rendering context lines that overlap with primary lines
            if !line_in_primary(context.start.line) {
                if first_rendered && context.start.line.to_usize() > last_line_index + 1 {
                    rendered.push_str(&divider(indent, enable_color))
                }
                rendered.push_str(&get_line(
                    &file_contents,
                    false,
                    context.start.line,
                    enable_color,
                ));
                rendered.push_str("\n");

                first_rendered = true;
                last_line_index = context.start.line.to_usize();
                indent += 1;
                rendered_contexts[context_index] = true;
            }
            context_index += 1;
        }
        if primary.start.line.to_usize() > last_line_index + 1
            && (first_rendered || first_primary_rendered)
        {
            rendered.push_str(&divider(indent, enable_color))
        }

        if primary.start.line == primary.end.line {
            rendered.push_str(&get_line(
                file_contents,
                true,
                primary.start.line,
                enable_color,
            ));
            rendered.push_str("\n");
            rendered.push_str(&highlight_line(&primary, enable_color));
            rendered.push_str("\n");
        } else {
            rendered.push_str(&get_lines(
                file_contents,
                true,
                primary.start.line,
                primary.end.line,
                enable_color,
            ));
            rendered.push_str("\n");
        }

        last_line_index = primary.end.line.to_usize();
        first_primary_rendered = true;
    }

    while context_index < location.contexts.len() {
        let context = &location.contexts[context_index];
        // Skip rendering context lines that overlap with primary lines
        if !line_in_primary(context.start.line) {
            if first_rendered && context.start.line.to_usize() > last_line_index + 1 {
                rendered.push_str(&divider(indent, enable_color))
            }
            rendered.push_str(&get_line(
                &file_contents,
                false,
                context.start.line,
                enable_color,
            ));
            rendered.push_str("\n");

            first_rendered = true;
            last_line_index = context.start.line.to_usize();
            indent += 1;
            rendered_contexts[context_index] = true;
        }
        context_index += 1;
    }

    for (context_index, context) in location.contexts.iter().enumerate().rev() {
        // Skip rendering context lines that overlap with primary lines
        if line_in_primary(context.end.line) {
            continue;
        }

        // Only decrement indent if the corresponding start line was actually rendered
        // (i.e., not skipped because it overlapped with a primary line)
        let start_line_was_rendered = rendered_contexts[context_index];

        // If this context is a single-line range and we already rendered its start line,
        // don't render it again as an end line.
        if start_line_was_rendered && context.start.line == context.end.line {
            continue;
        }

        if last_line_index == context.end.line.to_usize() {
            // Skip if we've already rendered this line - don't change indent
            continue;
        }
        if context.end.line.to_usize() > last_line_index + 1 {
            rendered.push_str(&divider(indent, enable_color))
        }

        rendered.push_str(&get_line(
            &file_contents,
            false,
            context.end.line,
            enable_color,
        ));
        rendered.push_str("\n");

        last_line_index = context.end.line.to_usize();
        // Only decrement indent if the corresponding start line was rendered
        if start_line_was_rendered && indent > 0 {
            indent -= 1;
        }
    }
}

fn line_number_prefix(show_line_number: bool, number: usize) -> String {
    if show_line_number {
        let num = number.to_string();
        if number < 10 {
            format!("   {}| ", num)
        } else if number < 100 {
            format!("  {}| ", num)
        } else if number < 1000 {
            format!(" {}| ", num)
        } else {
            format!("{}| ", num)
        }
    } else {
        "    | ".to_string()
    }
}

fn highlight_line(range: &Range, enable_color: bool) -> String {
    if range.start.column < range.end.column && range.start.line == range.end.line {
        // Columns are 1-based, so subtract 1 for 0-based string indexing
        // But position() captures BEFORE consuming, so we need to add 1 to get the actual character position
        let indent = " ".repeat(range.start.column.saturating_sub(1));
        let highlight = "^".repeat(range.end.column - range.start.column);
        format!(
            "    {} {}{}",
            color::gray(enable_color, "|"),
            indent,
            color::red(enable_color, &highlight)
        )
    } else if range.start.column == range.end.column && range.start.line == range.end.line {
        // Columns are 1-based, so subtract 1 for 0-based string indexing
        // But position() captures BEFORE consuming, so we need to add 1 to get the actual character position
        let indent = " ".repeat(range.start.column.saturating_sub(1));
        let highlight = "^";
        format!(
            "    {} {}{}",
            color::gray(enable_color, "|"),
            indent,
            color::red(enable_color, &highlight)
        )
    } else {
        println!("CROSSED RANGE {:#?}", range);
        color::red(enable_color, "    ^^")
    }
}

fn get_line(
    file_contents: &str,
    show_line_number: bool,
    line_index: u32,
    enable_color: bool,
) -> String {
    let line_number = line_index.to_usize().saturating_sub(1);

    let prefix = color::gray(
        enable_color,
        &line_number_prefix(show_line_number, line_index.to_usize()),
    );

    for (index, line) in file_contents.to_string().lines().enumerate() {
        if line_number == index {
            return format!("{}{}", prefix, line.to_string());
        }
    }
    prefix.to_string()
}

fn get_lines(
    file_contents: &str,
    show_line_number: bool,
    start: u32,
    end: u32,
    enable_color: bool,
) -> String {
    let start_line_number = start.to_usize() - 1;
    let end_line_number = end.to_usize() - 1;

    let mut result = "".to_string();

    for (index, line) in file_contents.to_string().lines().enumerate() {
        if start_line_number <= index && end_line_number >= index {
            let prefix = color::gray(
                enable_color,
                &line_number_prefix(show_line_number, index.to_usize() + 1),
            );
            result.push_str(&format!("{}{}", prefix, line.to_string()));
            result.push_str("\n");
        }
    }
    result
}

fn render_expecting(expecting: &Expecting, in_color: bool) -> String {
    match expecting {
        Expecting::PyreFile => "I ran into an issue parsing this that I didn't quite expect! I would love if you would file an issue on the repo showing the pyre file you're using.. ".to_string(),
        Expecting::ParamDefinition => return format!(
            "I was expecting a parameter, like:\n\n        {}\n\n    Hot tip: Running {} will automatically fix this for you.\n",
            yellow_if(in_color, "$id: Int"),
            cyan_if(in_color, "pyre format")
        ),
        Expecting::ParamDefType => return format!(
            "I was expecting a parameter type, like:\n\n        {}\n\n    Hot tip: Running {} will automatically fix this for you.\n",
            yellow_if(in_color, "$id: Int"),
            cyan_if(in_color, "pyre format")
        ),
        Expecting::AtDirective => return format!(
            "I don't recognize this, did you mean one of these:\n\n        {}\n        {}\n        {}",
            yellow_if(in_color, "@where"),
            yellow_if(in_color, "@sort"),
            yellow_if(in_color, "@limit")
        ),
        Expecting::SchemaAtDirective => return format!(
            "I don't recognize this, did you mean one of these:\n\n        {}\n        {}\n        {}\n        {}",
            yellow_if(in_color, "@watch"),
            yellow_if(in_color, "@tablename"),
            yellow_if(in_color, "@link"),
            yellow_if(in_color, "@allow")
        ),
        Expecting::SchemaFieldAtDirective => return format!(
            "I don't recognize this, did you mean one of these:\n\n        {}\n        {}\n        {}",
            yellow_if(in_color, "@id"),
            yellow_if(in_color, "@unique"),
            yellow_if(in_color, "@default")
        ),
        Expecting::SchemaColumn => return format!(
            "I was expecting a column, like:\n\n        {}",
            yellow_if(in_color, "id    Int")
        ),
        Expecting::LinkDirective => {
            let explicit_example = format!("{}(authorId, User.id)", yellow_if(in_color, "@link"));
            let reverse_example = format!("{}(Post.authorId)", yellow_if(in_color, "@link"));
            let example_breakdown = format!("       {}  {}",
                cyan_if(in_color, "^^^^^^^^"), 
                cyan_if(in_color, "^^^^^^^"));
            let example_breakdown_connector = format!("        {}         {}",
                cyan_if(in_color, "|"), 
                cyan_if(in_color, "|"));
            let example_breakdown_labels = format!("    {}   {}",
                cyan_if(in_color, "Local key"), 
                cyan_if(in_color, "Foreign table.key"));

            return format!(
                "This {} looks off. Expected one of:\n\n        {}\n        {}\n\n    Explicit form breakdown:\n        {}\n        {}\n        {}\n\n    Tip: if this looks correct, check for invisible whitespace characters around the comma/parentheses.",
                yellow_if(in_color, "@link"),
                explicit_example,
                reverse_example,
                example_breakdown,
                example_breakdown_connector,
                example_breakdown_labels
            )
        }


        // "I was expecting a link directive".to_string(),
    }
}

pub fn cyan_if(in_color: bool, text: &str) -> String {
    color::cyan_if(in_color, text)
}

pub fn yellow_if(in_color: bool, text: &str) -> String {
    color::yellow_if(in_color, text)
}

pub fn format_yellow_list(in_color: bool, items: Vec<String>) -> String {
    let mut result = "".to_string();
    for item in items {
        result.push_str(&format!("    {}\n", yellow_if(in_color, &item)));
    }
    result
}

fn format_yellow_or_list(items: &Vec<String>, in_color: bool) -> String {
    match items.len() {
        0 => String::new(),
        1 => yellow_if(in_color, &items[0]),
        2 => format!(
            "{} or {}",
            yellow_if(in_color, &items[0]),
            yellow_if(in_color, &items[1])
        ),
        _ => {
            if let Some((last, rest)) = items.split_last() {
                format!(
                    "{}, or {}",
                    rest.iter()
                        .map(|item| yellow_if(in_color, item))
                        .collect::<Vec<_>>()
                        .join(", "),
                    yellow_if(in_color, last)
                )
            } else {
                String::new()
            }
        }
    }
}

fn to_error_description(error: &Error, in_color: bool) -> String {
    match &error.error_type {
        ErrorType::ParsingError(parsing_details) => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{}",
                render_expecting(&parsing_details.expecting, in_color)
            ));

            result
        }

        ErrorType::MultipleSessionDefinitions => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "I found multiple {} definitions, but there should only be one!",
                cyan_if(in_color, "session"),
            ));

            result
        }

        ErrorType::UnknownFunction {
            found,
            known_functions,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "I don't recognize this function: {}\n\n",
                cyan_if(in_color, found),
            ));

            if known_functions.len() > 0 {
                result.push_str("\nHere are the functions I know:\n");
                for func in known_functions {
                    result.push_str(&format!("    {}\n", cyan_if(in_color, func)));
                }
            }

            result
        }

        ErrorType::MissingType => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "All parameters need a type, like {}\n\n    Hot tip: Running {} will automatically fix this automatically for you!\n",
                yellow_if(in_color, "Int"),
                cyan_if(in_color, "pyre format")
            ));

            result
        }

        ErrorType::DuplicateDefinition(name) => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "There are two definitions for {}\n",
                yellow_if(in_color, name)
            ));

            result
        }

        ErrorType::DefinitionIsBuiltIn(name) => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "The {} type is a built-in type, try using another name.\n",
                yellow_if(in_color, name)
            ));

            result
        }
        ErrorType::GeneratedCrudNameCollision {
            name,
            table,
            operation,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} is reserved for the generated {} mutation on {}. Pick a different name for your query or mutation.\n",
                yellow_if(in_color, name),
                cyan_if(
                    in_color,
                    match operation {
                        ast::QueryOperation::Insert => "create",
                        ast::QueryOperation::Update => "update",
                        ast::QueryOperation::Delete => "delete",
                        ast::QueryOperation::Query => "query",
                    }
                ),
                cyan_if(in_color, table)
            ));

            result
        }
        ErrorType::DuplicateField { record, field } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "There are multiple definitions for {} on {}.\n",
                yellow_if(in_color, field),
                cyan_if(in_color, record)
            ));

            result
        }
        ErrorType::DuplicateVariant { base_variant, .. } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} has more than one variant named {}.\n",
                yellow_if(in_color, &base_variant.typename),
                cyan_if(in_color, &base_variant.variant_name)
            ));

            result
        }
        ErrorType::DuplicateQueryField { field } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} is listed multiple times.\n",
                yellow_if(in_color, field)
            ));

            result
        }
        ErrorType::InvalidWildcardSelection { reason } => {
            let mut result = "".to_string();
            let message = match reason {
                InvalidWildcardReason::Aliased => "Wildcard fields cannot be aliased.",
                InvalidWildcardReason::Assigned => "Wildcard fields cannot be assigned.",
                InvalidWildcardReason::HasNestedSelections => {
                    "Wildcard fields cannot have nested selections."
                }
                InvalidWildcardReason::UsedInInsertOrUpdate => {
                    "Wildcard fields are not allowed in inserts or updates."
                }
            };
            result.push_str(&format!("{}\n", message));
            result
        }
        ErrorType::UnknownTable { found, existing } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "I don't recognize the '{}' table, is that a typo?\n",
                yellow_if(in_color, found)
            ));

            if existing.len() > 0 {
                result.push_str("\nThese tables might be similar\n");
                for table in existing {
                    result.push_str(&format!("    {}\n", cyan_if(in_color, table)));
                }
            }

            result
        }

        ErrorType::LiteralTypeMismatch {
            expecting_type,
            found,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "I was expecting {}, but found {}.\n",
                yellow_if(in_color, expecting_type),
                cyan_if(in_color, found)
            ));

            result
        }
        ErrorType::LiteralTypeMismatchVariant {
            found,
            expecting_type,
            variants,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "I was expecting a {}, but found {}.\n",
                yellow_if(in_color, expecting_type),
                cyan_if(in_color, found)
            ));

            result.push_str("    Here are the values I know would work:\n\n");
            for variant in variants {
                result.push_str(&format!("        {}\n", yellow_if(in_color, variant)));
            }

            result
        }

        ErrorType::TypeMismatch {
            column_defined_as,
            variable_name,
            variable_defined_as,
            ..
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} is defined as {}, but I'm expecting a {}.\n",
                yellow_if(in_color, &format!("${}", variable_name)),
                yellow_if(in_color, variable_defined_as),
                cyan_if(in_color, column_defined_as)
            ));

            result
        }

        ErrorType::LinkToUnknownForeignField {
            link_name,
            foreign_table,
            unknown_foreign_field,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} is trying to link to the {} column on the {} table, but that column doesn't exist.\n",
                yellow_if(in_color, link_name),
                yellow_if(in_color, unknown_foreign_field),
                yellow_if(in_color, foreign_table),
            ));

            result
        }

        ErrorType::LinkToUnknownField {
            link_name,
            unknown_local_field,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} is trying to link using the {} column, but that column doesn't exist.",
                yellow_if(in_color, link_name),
                yellow_if(in_color, unknown_local_field),
            ));

            result
        }
        ErrorType::LinkToUnknownTable {
            link_name,
            unknown_table,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} is trying to link to the {} table, but that table doesn't exist.",
                yellow_if(in_color, link_name),
                yellow_if(in_color, unknown_table),
            ));

            result
        }

        ErrorType::ForeignKeyToUnknownTable {
            field_name,
            referenced_table,
            existing_tables,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} references {}, but that table doesn't exist.",
                yellow_if(in_color, field_name),
                yellow_if(in_color, &format!("{}.id", referenced_table)),
            ));

            if !existing_tables.is_empty() {
                result.push_str("\n\nAvailable tables:\n");
                for table in existing_tables {
                    result.push_str(&format!("  - {}\n", cyan_if(in_color, table)));
                }
            }

            result
        }

        ErrorType::ForeignKeyToUnknownField {
            field_name,
            referenced_table,
            referenced_field,
            existing_fields,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} references {}, but the {} table doesn't have a field named {}.",
                yellow_if(in_color, field_name),
                yellow_if(
                    in_color,
                    &format!("{}.{}", referenced_table, referenced_field)
                ),
                cyan_if(in_color, referenced_table),
                yellow_if(in_color, referenced_field),
            ));

            if !existing_fields.is_empty() {
                result.push_str("\n\nAvailable fields:\n");
                for field in existing_fields {
                    result.push_str(&format!("  - {}\n", cyan_if(in_color, field)));
                }
            }

            result
        }

        ErrorType::ForeignKeyToNonIdField {
            field_name,
            referenced_table,
            referenced_field,
            referenced_field_type,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} references {}, but {} is a {} not an ID type.",
                yellow_if(in_color, field_name),
                yellow_if(
                    in_color,
                    &format!("{}.{}", referenced_table, referenced_field)
                ),
                yellow_if(in_color, referenced_field),
                cyan_if(in_color, referenced_field_type),
            ));

            result.push_str(&format!(
                "\n\nForeign key references must point to an {} or {} field.",
                cyan_if(in_color, "Id.Int"),
                cyan_if(in_color, "Id.Uuid"),
            ));

            result
        }

        ErrorType::NoPrimaryKey { record } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} doesn't have a primary key, let's add one!",
                cyan_if(in_color, record)
            ));

            result
        }

        ErrorType::MultiplePrimaryKeys { record, .. } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} has multiple primary keys, let's only have one.",
                cyan_if(in_color, record)
            ));

            result
        }

        ErrorType::MultipleTableNames { record } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} has has multiple @tablename definitions, let's only have one.",
                cyan_if(in_color, record)
            ));

            result
        }
        ErrorType::MultiplePermissions { record } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} has multiple {} definitions, let's only have one!",
                cyan_if(in_color, record),
                yellow_if(in_color, "@allow")
            ));

            result
        }
        ErrorType::MissingPermissions { record } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} must have exactly one permissions directive. Add either {} or {}.",
                cyan_if(in_color, record),
                yellow_if(in_color, "@allow"),
                yellow_if(in_color, "@public")
            ));

            result
        }
        ErrorType::InvalidRecordIndexField {
            record,
            directive,
            field,
            known_fields,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "{} references unknown field {}.",
                yellow_if(in_color, directive),
                yellow_if(in_color, field)
            ));

            result.push_str(&format!(
                "\n\n{} has: {}",
                cyan_if(in_color, record),
                known_fields
                    .iter()
                    .map(|f| yellow_if(in_color, f))
                    .collect::<Vec<String>>()
                    .join(", ")
            ));
            result
        }
        ErrorType::DuplicateRecordIndexField {
            record,
            directive,
            field,
        } => {
            format!(
                "{} has duplicate field {} in {}.",
                cyan_if(in_color, record),
                yellow_if(in_color, field),
                yellow_if(in_color, directive)
            )
        }

        ErrorType::MultipleLimits { query } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} has multiple {}, let's only have one!",
                cyan_if(in_color, query),
                yellow_if(in_color, "@limits")
            ));

            result
        }
        ErrorType::MultipleWheres { query } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} has multiple {}, let's only have one!",
                cyan_if(in_color, query),
                yellow_if(in_color, "@wheres")
            ));

            result
        }
        ErrorType::UndefinedParam { param, type_ } => {
            let mut result = "".to_string();
            let type_suggestion = match type_ {
                None => "".to_string(),
                Some(type_) => format!(
                    "\nAdd it to your declarations as {}: {}",
                    yellow_if(in_color, param),
                    cyan_if(in_color, type_)
                ),
            };
            result.push_str(&format!(
                "{} is used, but not declared.{}",
                yellow_if(in_color, param),
                type_suggestion
            ));

            result
        }
        ErrorType::LinkSelectionIsEmpty {
            link_name,
            foreign_table,
            ..
        } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is a link to the {} table, but doesn't select any fields.  Let's select some!",
                cyan_if(in_color, link_name),
                cyan_if(in_color, foreign_table)
            ));

            result
        }

        ErrorType::LinkToUnknownSchema {
            unknown_schema_name,
            known_schemas,
        } => {
            if known_schemas.len() == 1 {
                let mut result = "".to_string();

                result.push_str(&format!(
                    "I don't recognize {} as a schema. There's only one schema, {}, so nothing should be qualified.",
                    cyan_if(in_color, unknown_schema_name),
                    cyan_if(in_color, &join_hashset(known_schemas, "\n    "))
                ));

                result
            } else {
                let mut result = "".to_string();

                result.push_str(&format!(
                    "I don't recognize {} as a schema. Here are the schemas I know about:\n{}",
                    cyan_if(in_color, unknown_schema_name),
                    cyan_if(in_color, &join_hashset(known_schemas, "\n    "))
                ));

                result
            }
        }

        ErrorType::UnusedParam { param } => {
            let mut result = "".to_string();
            let colored_param = yellow_if(in_color, param);

            result.push_str(&format!(
                "{} isn't being used. Let's either use it or remove it.",
                colored_param
            ));

            result
        }
        ErrorType::InsertColumnIsNotSet { field } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is required but not set to anything.",
                yellow_if(in_color, field),
            ));

            result
        }
        ErrorType::LinksDisallowedInDeletes { field } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is a {}, which isn't allowed in a {}",
                yellow_if(in_color, field),
                yellow_if(in_color, "@link"),
                cyan_if(in_color, "delete")
            ));

            result
        }

        ErrorType::LinksDisallowedInUpdates { field } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is a {}, which isn't allowed in a {}",
                yellow_if(in_color, field),
                yellow_if(in_color, "@link"),
                cyan_if(in_color, "update")
            ));

            result
        }

        ErrorType::LinksDisallowedInInserts {
            field,
            table_name,
            local_ids,
        } => {
            let mut result = "".to_string();
            result.push_str(&format!(
                "Nested inserts are only allowed if you start with a primary key.\n\n{} links via {}, which isn't the primary key of the {} table.",
                yellow_if(in_color, field),
                yellow_if(in_color, &local_ids.clone().join(", ")),
                yellow_if(in_color, table_name),
            ));

            result
        }

        ErrorType::LimitOffsetOnlyInFlatRecord => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "This query has a limit, but also queries nested values.\n\n{} isn't able to handle this situation yet and can only handle @limit in a query with no nested fields.",
                yellow_if(in_color, "Pyre"),
            ));

            result
        }

        ErrorType::NoSetsInSelect { field } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is being set, which isn't allowed in a {}",
                yellow_if(in_color, &format!("${}", field)),
                cyan_if(in_color, "query")
            ));

            result
        }
        ErrorType::NoSetsInDelete { field } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is being set, which isn't allowed in a {}",
                yellow_if(in_color, &format!("${}", field)),
                cyan_if(in_color, "delete")
            ));

            result
        }

        ErrorType::InsertMissingColumn { table_name, fields } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is missing {}",
                yellow_if(in_color, &table_name),
                yellow_if(in_color, &fields.join(", "))
            ));

            result
        }
        ErrorType::InsertNestedValueAutomaticallySet { field } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "Pyre is setting {} automatically based on your {}, no need to set it manually.",
                yellow_if(in_color, field),
                yellow_if(in_color, "@link")
            ));

            result
        }
        ErrorType::ManagedColumnCannotBeSet { field, operation } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is managed by Pyre and can't be set in a {} mutation.",
                yellow_if(in_color, field),
                cyan_if(in_color, operation.as_str())
            ));

            result
        }
        ErrorType::MultipleSchemaWrites {
            field_table,
            field_schema,
            operation,
            other_schemas,
        } => {
            let mut result = "".to_string();

            let operation_words = match operation {
                ast::QueryOperation::Query => "querying from",
                ast::QueryOperation::Insert => "inserting a value to",
                ast::QueryOperation::Update => "updating a value on",
                ast::QueryOperation::Delete => "deleting from",
            };

            let schema_words: String = format_yellow_or_list(&other_schemas, in_color);

            result.push_str(&format!(
                "This value is on the {} table and is {} the {} schema, but you can only write to one schema in a query. Everything else is writing to {}",
                yellow_if(in_color, field_table),
                operation_words,
                yellow_if(in_color, field_schema),
                schema_words
            ));

            result
        }
        ErrorType::NoFieldsSelected => {
            let mut result = "".to_string();

            result.push_str("There are no fields selected for this table, let's add some!");

            result
        }
        ErrorType::WhereOnLinkIsntAllowed { link_name } => {
            let mut result = "".to_string();

            result.push_str(&format!(
                "{} is a {}, which can't be in a {}",
                cyan_if(in_color, link_name),
                cyan_if(in_color, "@link"),
                yellow_if(in_color, "@where"),
            ));

            result
        }
        ErrorType::InvalidRelationalPermission { message } => message.clone(),
        ErrorType::SyncedRelationalQueryPermission => "Linked query permissions require a query-only namespace until dependency invalidation and permission revocation are supported.".to_string(),
        ErrorType::UnknownType { found, known_types } => {
            let mut result = "".to_string();
            let colored_param = cyan_if(in_color, found);

            result.push_str(&format!(
                "I don't recognize the {} type, is that a typo?",
                colored_param
            ));

            let mut basic_types = Vec::new();
            let mut custom_types = Vec::new();
            let mut reference_types = Vec::new();

            let basic_type_names = [
                "Bool", "DateTime", "Float", "Json", "Id.Int", "Id.Uuid", "Int", "String",
            ];

            for typename in known_types {
                if basic_type_names.contains(&typename.as_str()) {
                    basic_types.push(typename);
                } else if typename.contains('.') && !typename.starts_with("Id.") {
                    reference_types.push(typename);
                } else {
                    custom_types.push(typename);
                }
            }

            let mut sorted_basic = Vec::new();
            for name in basic_type_names {
                if basic_types.iter().any(|t| t.as_str() == name) {
                    sorted_basic.push(name.to_string());
                }
            }

            custom_types.sort();
            reference_types.sort();

            result.push_str("\n\n    Basic types:\n\n");
            if sorted_basic.is_empty() {
                result.push_str("        (none)\n");
            } else {
                for typename in sorted_basic {
                    result.push_str(&format!("        {}\n", cyan_if(in_color, &typename)));
                }
            }

            result.push_str("\n    Custom types:\n\n");
            if custom_types.is_empty() {
                result.push_str("        (none)\n");
            } else {
                for typename in custom_types {
                    result.push_str(&format!("        {}\n", cyan_if(in_color, &typename)));
                }
            }

            result.push_str("\n    References:\n\n");
            if reference_types.is_empty() {
                result.push_str("        (none)\n");
            } else {
                for typename in reference_types {
                    result.push_str(&format!("        {}\n", cyan_if(in_color, &typename)));
                }
            }

            result
        }
        ErrorType::InvalidTypeUsage { message } => message.clone(),

        ErrorType::UnknownField {
            found,
            record_name,
            known_fields,
        } => {
            let mut result = "".to_string();
            let colored_param = yellow_if(in_color, found);
            let mut a_or_an = "a";

            if found.starts_with('a') {
                a_or_an = "an";
            }

            result.push_str(&format!(
                "{} doesn't have {} {} field, is that a typo?",
                cyan_if(in_color, record_name),
                a_or_an,
                colored_param
            ));

            result.push_str(&format!(
                "\n\n    Here are the fields on {}:\n\n",
                cyan_if(in_color, record_name)
            ));
            let mut largest_fieldname_size: usize = 0;
            for (field_name, _) in known_fields {
                let len = field_name.len();
                if len > largest_fieldname_size {
                    largest_fieldname_size = len
                }
            }

            for (field_name, field_type) in known_fields {
                let extra_spacing_amount = (largest_fieldname_size - field_name.len()) + 1;
                let spacing = " ".repeat(extra_spacing_amount);
                result.push_str(&format!(
                    "        {}:{}{}\n",
                    yellow_if(in_color, &field_name),
                    spacing,
                    cyan_if(in_color, &field_type)
                ));
            }

            result
        }
        ErrorType::VariantFieldTypeCollision { .. } => {
            format!("Fields with the same name across variants must have the same type.",)
        }
        ErrorType::MigrationTableDropped { table_name } => {
            format!(
                "The table {} has been dropped. This might be causing issues in your query.",
                yellow_if(in_color, table_name)
            )
        }
        ErrorType::MigrationColumnDropped {
            table_name,
            column_name,
            ..
        } => {
            format!("The column {} has been dropped from the table {}. This might be causing issues in your query. Consider adding the column back or updating your query to use the new column format.", yellow_if(in_color, column_name), yellow_if(in_color, table_name))
        }
        ErrorType::MigrationColumnModified {
            table_name,
            column_name,
            ..
        } => {
            format!("The column {} has been modified in the table {}. This might be causing issues in your query. Consider updating your query to use the new column format.", yellow_if(in_color, column_name), yellow_if(in_color, table_name))
        }
        ErrorType::MigrationVariantRemoved {
            tagged_name,
            variant_name,
        } => {
            format!("The variant {} has been removed from the tagged type {}. This might be causing issues in your query. Consider updating your query to use the new variant format.", yellow_if(in_color, variant_name), yellow_if(in_color, tagged_name))
        }
        ErrorType::InvalidColumnDefault {
            field_name,
            field_type,
            default_value,
            expected,
        } => {
            if field_type.starts_with("Id.Uuid") || field_type.starts_with("Id.Int") {
                return format!(
                    "{} is not allowed to have an @default.",
                    yellow_if(in_color, field_type)
                );
            }

            let expected_text = if expected.is_empty() {
                "none".to_string()
            } else {
                format_yellow_or_list(expected, in_color)
            };

            format!(
                "The field {} (type {}) has an unsupported default {}.\n\nAllowed defaults here: {}",
                yellow_if(in_color, field_name),
                cyan_if(in_color, field_type),
                yellow_if(in_color, default_value),
                expected_text
            )
        }
        ErrorType::MigrationSchemaNotFound { namespace } => match namespace {
            Some(name) => format!(
                "A migration was attempted for the schema named {}, but it was not found.",
                yellow_if(in_color, &name)
            ),
            None => {
                format!("A migration was attempted for the default schema, but it was not found.")
            }
        },
        ErrorType::MigrationMissingSchema => {
            format!("There is no schema recorded in the database.")
        }
    }
}

// JSON error format
fn to_error_title(error_type: &ErrorType) -> String {
    match error_type {
        ErrorType::ParsingError(_) => "Parsing Error",
        ErrorType::UnknownFunction { .. } => "Unknown Function",
        ErrorType::MultipleSessionDefinitions => "Multiple Session Definitions",
        ErrorType::MissingType => "Missing Type",
        ErrorType::DuplicateDefinition(_) => "Duplicate Definition",
        ErrorType::DefinitionIsBuiltIn(_) => "Definition Is Built-in",
        ErrorType::GeneratedCrudNameCollision { .. } => "Generated CRUD Name Collision",
        ErrorType::DuplicateField { .. } => "Duplicate Field",
        ErrorType::DuplicateVariant { .. } => "Duplicate Variant",
        ErrorType::UnknownType { .. } => "Unknown Type",
        ErrorType::InvalidTypeUsage { .. } => "Invalid Type Usage",
        ErrorType::NoPrimaryKey { .. } => "No Primary Key",
        ErrorType::MultiplePrimaryKeys { .. } => "Multiple Primary Keys",
        ErrorType::MultipleTableNames { .. } => "Multiple table names",
        ErrorType::MultiplePermissions { .. } => "Multiple Permissions",
        ErrorType::MissingPermissions { .. } => "Missing Permissions",
        ErrorType::InvalidRecordIndexField { .. } => "Invalid Record Index Field",
        ErrorType::DuplicateRecordIndexField { .. } => "Duplicate Record Index Field",
        ErrorType::LinkToUnknownTable { .. } => "Link to unknown table",
        ErrorType::LinkToUnknownField { .. } => "Link to unknown field",
        ErrorType::LinkToUnknownForeignField { .. } => "Link to Unknown Foreign Field",
        ErrorType::LinkSelectionIsEmpty { .. } => "Link Selection Is Empty",
        ErrorType::LinkToUnknownSchema { .. } => "Link to Unknown Schema",
        ErrorType::ForeignKeyToUnknownTable { .. } => "Foreign key to unknown table",
        ErrorType::ForeignKeyToUnknownField { .. } => "Foreign key to unknown field",
        ErrorType::ForeignKeyToNonIdField { .. } => "Foreign key to non-ID field",
        ErrorType::UnknownTable { .. } => "Unknown Table",
        ErrorType::DuplicateQueryField { .. } => "Duplicate Query Field",
        ErrorType::InvalidWildcardSelection { .. } => "Invalid Wildcard Selection",
        ErrorType::NoFieldsSelected => "No Fields Selected",
        ErrorType::UnknownField { .. } => "Unknown Field",
        ErrorType::MultipleLimits { .. } => "Multiple Limits",
        ErrorType::MultipleWheres { .. } => "Multiple Wheres",
        ErrorType::WhereOnLinkIsntAllowed { .. } => "Where On Link Not Allowed",
        ErrorType::InvalidRelationalPermission { .. } => "Invalid Relational Permission",
        ErrorType::SyncedRelationalQueryPermission => {
            "Linked Query Permission Requires Query-only Mode"
        }
        ErrorType::TypeMismatch { .. } => "Type Mismatch",
        ErrorType::LiteralTypeMismatch { .. } => "Incorrect type",
        ErrorType::LiteralTypeMismatchVariant { .. } => "Incorrect type",
        ErrorType::UnusedParam { .. } => "Unused Parameter",
        ErrorType::UndefinedParam { .. } => "Undefined Parameter",
        ErrorType::NoSetsInSelect { .. } => "No Sets In Select",
        ErrorType::NoSetsInDelete { .. } => "No Sets In Delete",
        ErrorType::LinksDisallowedInInserts { .. } => "Links Not Allowed In Inserts",
        ErrorType::LinksDisallowedInDeletes { .. } => "Links Not Allowed In Deletes",
        ErrorType::LinksDisallowedInUpdates { .. } => "Links Not Allowed In Updates",
        ErrorType::InsertColumnIsNotSet { .. } => "Insert Column Not Set",
        ErrorType::InsertMissingColumn { .. } => "Insert Missing Column",
        ErrorType::InsertNestedValueAutomaticallySet { .. } => "Can't set automatic field",
        ErrorType::ManagedColumnCannotBeSet { .. } => "Managed Column Cannot Be Set",
        ErrorType::MultipleSchemaWrites { .. } => "Multiple Schema Writes",
        ErrorType::LimitOffsetOnlyInFlatRecord => "Limit Only In Flat Record",
        ErrorType::VariantFieldTypeCollision { .. } => "Variant Field Type Collision",
        ErrorType::MigrationTableDropped { .. } => "Table Dropped",
        ErrorType::MigrationColumnDropped { .. } => "Column Dropped",
        ErrorType::MigrationColumnModified { .. } => "Column Modified",
        ErrorType::MigrationVariantRemoved { .. } => "Variant Removed",
        ErrorType::InvalidColumnDefault { .. } => "Invalid Column Default",
        ErrorType::MigrationSchemaNotFound { .. } => "Schema Not Found",
        ErrorType::MigrationMissingSchema => "Missing Schema",
    }
    .to_string()
}

#[cfg(feature = "json")]
pub fn format_json(error: &Error) -> serde_json::Value {
    let mut error_json = serde_json::Map::new();

    let title = to_error_title(&error.error_type);
    let description = to_error_description(&error, false);

    // Add filepath
    error_json.insert(
        "filepath".to_string(),
        serde_json::Value::String(error.filepath.clone()),
    );

    // Add locations
    let mut locations = Vec::new();
    for location in &error.locations {
        let mut location_json = serde_json::Map::new();

        // Add primary ranges
        let mut primary_ranges = Vec::new();
        for range in &location.primary {
            let mut range_json = serde_json::Map::new();

            // Create start object
            let mut start = serde_json::Map::new();
            start.insert(
                "line".to_string(),
                serde_json::Value::Number(range.start.line.into()),
            );
            start.insert(
                "column".to_string(),
                serde_json::Value::Number(range.start.column.into()),
            );
            range_json.insert("start".to_string(), serde_json::Value::Object(start));

            // Create end object
            let mut end = serde_json::Map::new();
            end.insert(
                "line".to_string(),
                serde_json::Value::Number(range.end.line.into()),
            );
            end.insert(
                "column".to_string(),
                serde_json::Value::Number(range.end.column.into()),
            );
            range_json.insert("end".to_string(), serde_json::Value::Object(end));

            primary_ranges.push(serde_json::Value::Object(range_json));
        }
        location_json.insert(
            "primary".to_string(),
            serde_json::Value::Array(primary_ranges),
        );

        // Add context ranges
        let mut context_ranges = Vec::new();
        for range in &location.contexts {
            let mut range_json = serde_json::Map::new();

            // Create start object
            let mut start = serde_json::Map::new();
            start.insert(
                "line".to_string(),
                serde_json::Value::Number(range.start.line.into()),
            );
            start.insert(
                "column".to_string(),
                serde_json::Value::Number(range.start.column.into()),
            );
            range_json.insert("start".to_string(), serde_json::Value::Object(start));

            // Create end object
            let mut end = serde_json::Map::new();
            end.insert(
                "line".to_string(),
                serde_json::Value::Number(range.end.line.into()),
            );
            end.insert(
                "column".to_string(),
                serde_json::Value::Number(range.end.column.into()),
            );
            range_json.insert("end".to_string(), serde_json::Value::Object(end));

            context_ranges.push(serde_json::Value::Object(range_json));
        }
        location_json.insert(
            "contexts".to_string(),
            serde_json::Value::Array(context_ranges),
        );

        locations.push(serde_json::Value::Object(location_json));
    }
    error_json.insert("locations".to_string(), serde_json::Value::Array(locations));

    error_json.insert("title".to_string(), serde_json::Value::String(title));
    error_json.insert(
        "description".to_string(),
        serde_json::Value::String(description),
    );

    serde_json::Value::Object(error_json)
}

pub fn report_and_exit(
    error_list: Vec<Error>,
    paths: &crate::filesystem::Found,
    enable_color: bool,
) -> ! {
    for err in error_list {
        let schema_source =
            crate::filesystem::get_schema_source(&err.filepath, paths).unwrap_or("");
        let formatted_error = format_error(&schema_source, &err, enable_color);
        eprintln!("{}", &formatted_error);
    }
    std::process::exit(1);
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColumnDiff {
    pub type_changed: Option<(String, String)>, // (old_type, new_type)
    pub nullable_changed: Option<(bool, bool)>, // (old_nullable, new_nullable)
    pub added_directives: Vec<ast::ColumnDirective>,
    pub removed_directives: Vec<ast::ColumnDirective>,
}
