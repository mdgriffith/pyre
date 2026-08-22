use crate::ast;
use crate::error;
use crate::typecheck;
use std::collections::HashMap;
use std::collections::HashSet;

pub fn database(database: &mut ast::Database) {
    for schem in database.schemas.iter_mut() {
        schema(schem);
    }
}

pub fn schema(schem: &mut ast::Schema) {
    for file in schem.files.iter_mut() {
        format_top_level_spacing(&mut file.definitions);
    }

    let mut links: HashMap<String, Vec<(bool, ast::LinkDetails)>> = HashMap::new();
    // Get all links and calculate reciprocals
    for file in schem.files.iter() {
        for def in &file.definitions {
            if let ast::Definition::Record { name, fields, .. } = def {
                for field in fields {
                    match field {
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            add_link(&mut links, &name, &link, true);
                            let reciprocal = ast::to_reciprocal(&schem.namespace, &name, link);
                            add_link(&mut links, &link.foreign.table, &reciprocal, false);
                        }
                        _ => (),
                    }
                }
            }
        }
    }

    // Standard formatting
    for file in schem.files.iter_mut() {
        for definition in &mut file.definitions {
            format_definition(definition, &links);
        }
    }
}

fn format_top_level_spacing(definitions: &mut Vec<ast::Definition>) {
    let mut formatted = Vec::new();
    let mut pending_lines = 0;

    for definition in std::mem::take(definitions) {
        match definition {
            ast::Definition::Lines { count } => {
                pending_lines += count;
            }
            definition => {
                if let Some(previous) = formatted.last() {
                    let lines = if matches!(previous, ast::Definition::Comment { .. }) {
                        pending_lines.min(1)
                    } else {
                        std::cmp::max(1, pending_lines).min(2)
                    };

                    if lines > 0 {
                        formatted.push(ast::Definition::Lines { count: lines });
                    }
                }

                formatted.push(definition);
                pending_lines = 0;
            }
        }
    }

    *definitions = formatted;
}

fn add_link(
    links: &mut HashMap<String, Vec<(bool, ast::LinkDetails)>>,
    tablename: &str,
    link: &ast::LinkDetails,
    is_calculated: bool,
) {
    match links.get(tablename) {
        None => {
            links.insert(tablename.to_string(), vec![(is_calculated, link.clone())]);
        }
        Some(existing_links) => {
            enum LinkOp {
                Append,
                Skip,
                Replace(usize),
            }
            let mut op = LinkOp::Append;
            for (i, (existing_calculated, existing_link)) in existing_links.iter().enumerate() {
                if ast::link_equivalent(link, existing_link) {
                    if is_calculated {
                        // The new link is calculated
                        // So we should skip this because it's already been added
                        op = LinkOp::Skip
                    } else if *existing_calculated {
                        op = LinkOp::Replace(i);
                    }
                }
            }
            match op {
                LinkOp::Append => {
                    let mut new_links = existing_links.clone();
                    new_links.push((is_calculated, link.clone()));
                    links.insert(tablename.to_string(), new_links);
                }
                LinkOp::Skip => (),
                LinkOp::Replace(i) => {
                    let mut new_links = existing_links.clone();
                    new_links[i] = (is_calculated, link.clone());
                    links.insert(tablename.to_string(), new_links);
                }
            }
        }
    }
}

fn reorder_record_fields(fields: &mut Vec<ast::Field>) {
    // Separate fields into categories
    let mut tablename: Option<ast::Field> = None;
    let mut watch: Option<ast::Field> = None;
    let mut unique_directives: Vec<ast::Field> = Vec::new();
    let mut index_directives: Vec<ast::Field> = Vec::new();
    let mut permissions: Vec<ast::Field> = Vec::new();
    let mut singletons: Vec<ast::Field> = Vec::new();
    let mut timestamps: Vec<ast::Field> = Vec::new();
    let mut non_directive_fields: Vec<ast::Field> = Vec::new();
    let mut links: Vec<ast::Field> = Vec::new();

    // First pass: extract directives and separate links from other fields
    for field in fields.drain(..) {
        match &field {
            ast::Field::FieldDirective(ast::FieldDirective::TableName(_)) => {
                tablename = Some(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Watched(_)) => {
                watch = Some(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Unique(_)) => {
                unique_directives.push(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Index(_)) => {
                index_directives.push(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Permissions(_)) => {
                permissions.push(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Singleton) => {
                singletons.push(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Timestamps) => {
                timestamps.push(field);
            }
            ast::Field::FieldDirective(ast::FieldDirective::Link(_)) => {
                links.push(field);
            }
            _ => {
                non_directive_fields.push(field);
            }
        }
    }

    // Sort permissions by operation order: select, update, insert, delete
    permissions.sort_by(|a, b| {
        let order_a = get_permission_order(a);
        let order_b = get_permission_order(b);
        order_a.cmp(&order_b)
    });

    // Reassemble fields in the correct order
    fields.clear();

    // Check if we have directives before moving them
    let has_directives = tablename.is_some()
        || watch.is_some()
        || !permissions.is_empty()
        || !singletons.is_empty()
        || !unique_directives.is_empty()
        || !index_directives.is_empty()
        || !timestamps.is_empty();
    let has_content = !non_directive_fields.is_empty() || !links.is_empty();

    // 1. @tablename
    if let Some(tn) = tablename {
        fields.push(tn);
    }

    // 2. @watch
    if let Some(w) = watch {
        fields.push(w);
    }

    // 3. @allowed (or @public)
    fields.extend(permissions);

    // 4. @singleton
    fields.extend(singletons);

    // 5. @timestamps
    fields.extend(timestamps);

    // 6. @unique
    fields.extend(unique_directives);

    // 7. @index
    fields.extend(index_directives);

    // 7. Empty line (if we have directives and non-directive fields/links)
    if has_directives && has_content {
        // Check if there's already a ColumnLines at the start of non_directive_fields
        let needs_separator = match non_directive_fields.first() {
            Some(ast::Field::ColumnLines { .. }) => false,
            _ => true,
        };
        if needs_separator {
            fields.push(ast::Field::ColumnLines { count: 1 });
        }
    }

    // 8. Non-directive fields (columns, comments, column_lines) - preserve order
    fields.extend(non_directive_fields);

    // 9. Empty line (if links exist and we have other fields)
    if !links.is_empty() && !fields.is_empty() {
        // Check if the last field is already a ColumnLines
        let needs_separator = match fields.last() {
            Some(ast::Field::ColumnLines { .. }) => false,
            _ => true,
        };
        if needs_separator {
            fields.push(ast::Field::ColumnLines { count: 1 });
        }
    }

    // 10. Links
    fields.extend(links);
}

fn get_permission_order(field: &ast::Field) -> usize {
    match field {
        ast::Field::FieldDirective(ast::FieldDirective::Permissions(perm)) => match perm {
            ast::PermissionDetails::Public => 0,  // Public comes first
            ast::PermissionDetails::Star(_) => 1, // Star comes after public
            ast::PermissionDetails::OnOperation(ops) => {
                // Get the minimum operation order from the operations
                let mut min_order = usize::MAX;
                for op in ops {
                    for operation in &op.operations {
                        let order = match operation {
                            ast::QueryOperation::Query => 2,
                            ast::QueryOperation::Update => 3,
                            ast::QueryOperation::Insert => 4,
                            ast::QueryOperation::Delete => 5,
                            ast::QueryOperation::Transaction => 6,
                        };
                        min_order = min_order.min(order);
                    }
                }
                if min_order == usize::MAX {
                    6 // Fallback
                } else {
                    min_order
                }
            }
        },
        _ => 6, // Fallback
    }
}

fn format_definition(
    def: &mut ast::Definition,
    links: &HashMap<String, Vec<(bool, ast::LinkDetails)>>,
) {
    match def {
        ast::Definition::Lines { count } => {
            *count = std::cmp::max(1, std::cmp::min(*count, 2));
        }
        ast::Definition::Session(_) => (),
        ast::Definition::Comment { .. } => (),
        ast::Definition::SyncMode(_) => (),
        ast::Definition::Tagged { .. } => (),
        ast::Definition::Record {
            name,
            ref mut fields,
            ..
        } => {
            let empty_links = &vec![];
            let links_on_this_table = links.get(name).unwrap_or(empty_links);
            let mut links_missing: Vec<ast::LinkDetails> = vec![];
            let mut links_to_remove: Vec<ast::LinkDetails> = vec![];

            // See if any calculated links are missing
            for (_is_calculated, link) in links_on_this_table {
                let mut found = false;
                for field in fields.iter() {
                    match field {
                        ast::Field::FieldDirective(ast::FieldDirective::Link(existing_link)) => {
                            if ast::link_equivalent(existing_link, link) {
                                found = true;
                            }
                        }
                        _ => (),
                    }
                }
                if !found {
                    links_missing.push(link.clone());
                }
            }

            // See if there are links that should be removed
            for field in fields.iter() {
                match field {
                    ast::Field::FieldDirective(ast::FieldDirective::Link(existing_link)) => {
                        let mut found = false;
                        for (_is_calculated, link) in links_on_this_table {
                            if ast::link_equivalent(existing_link, link) {
                                found = true;
                            }
                        }
                        if !found {
                            links_to_remove.push(existing_link.clone());
                        }
                    }
                    _ => (),
                }
            }

            let mut previous_count: Option<usize> = None;
            let mut removed_fields = HashSet::new();
            let mut i = 0;

            // Merge adjacent ColumnLines items and have a max count of 2
            for field in fields.iter_mut() {
                match field {
                    ast::Field::ColumnLines { count } => {
                        if *count == 0 {
                            removed_fields.insert(i);
                        } else {
                            match previous_count {
                                Some(prev_count) => {
                                    *count += prev_count;
                                    *count = (*count).min(2);
                                    removed_fields.insert(i - 1);
                                }
                                None => {
                                    previous_count = Some(*count);
                                }
                            }
                        }
                    }

                    _ => {
                        previous_count = None;
                    }
                }
                i += 1;
            }

            // Remove the fields that were marked for removal
            let mut i = 0;
            fields.retain(|_| {
                let should_keep = !removed_fields.contains(&i);
                i += 1;
                should_keep
            });

            // Remove unnecessary links
            fields.retain(|field| {
                if let ast::Field::FieldDirective(ast::FieldDirective::Link(link)) = field {
                    !links_to_remove.contains(link)
                } else {
                    true
                }
            });

            // Add missing links
            for link in links_missing.drain(..) {
                fields.push(ast::Field::FieldDirective(ast::FieldDirective::Link(link)));
            }

            // Reorder fields according to standard format:
            // 1. @tablename
            // 2. @watch
            // 3. @allowed (or @public) - ordered: query, update, insert, delete
            // 4. @singleton
            // 5. @timestamps
            // 6. @unique
            // 7. @index
            // 8. Empty line
            // 9. Columns (in order)
            // 10. Empty line (if links exist)
            // 11. Links
            reorder_record_fields(fields);
        }
    }
}
/* Queries

The main thing that query_list does is calculate what the inferred param types are for each query.

Which is why it needs the full schema

*/
pub fn query_list(db_schema: &ast::Database, queries: &mut ast::QueryList) {
    match typecheck::populate_context(db_schema) {
        Ok(mut context) => {
            let mut all_query_info = HashMap::new();

            for query in queries.queries.iter() {
                match query {
                    ast::QueryDef::Query(q) => {
                        let mut errors: Vec<error::Error> = Vec::new();
                        let query_info = typecheck::check_query(&mut context, &mut errors, q);
                        all_query_info.insert(q.name.clone(), query_info);
                    }
                    _ => (),
                }
            }

            for query in queries.queries.iter_mut() {
                match query {
                    ast::QueryDef::Query(ref mut q) => match all_query_info.get_mut(&q.name) {
                        Some(query_info) => {
                            for arg in q.args.iter_mut() {
                                // Add $ to the front of the arg name
                                let param_name = format!("${}", arg.name);

                                //
                                match query_info.variables.get(&param_name) {
                                    Some(param) => {
                                        match param {
                                            typecheck::ParamInfo::Defined { type_, .. } => {
                                                arg.type_ = type_.clone();
                                            }
                                            typecheck::ParamInfo::NotDefinedButUsed {
                                                type_,
                                                ..
                                            } => {
                                                arg.type_ = type_.clone();
                                            }
                                        }

                                        query_info.variables.remove(&param_name);
                                    }
                                    None => (),
                                }
                            }

                            for (name, param) in query_info.variables.iter() {
                                match param {
                                    typecheck::ParamInfo::Defined { .. } => (),
                                    typecheck::ParamInfo::NotDefinedButUsed {
                                        type_,
                                        nullable,
                                        ..
                                    } => {
                                        q.args.push(ast::QueryParamDefinition {
                                            name: name.clone(),
                                            type_: type_.clone(),
                                            nullable: *nullable,
                                            omittable: false,
                                            start_name: None,
                                            end_name: None,
                                            start_type: None,
                                            end_type: None,
                                        });
                                    }
                                }
                            }

                            // Reorder query fields
                            for field in &mut q.fields {
                                match field {
                                    ast::TopLevelQueryField::Field(query_field) => {
                                        reorder_query_field(query_field);
                                    }
                                    _ => (),
                                }
                            }
                        }
                        None => (),
                    },
                    _ => (),
                }
            }

            // Format leading and trailing newlines
            format_query_list_newlines(&mut queries.queries);
        }
        Err(_) => (),
    }
}

fn format_query_list_newlines(queries: &mut Vec<ast::QueryDef>) {
    // Handle leading newlines: remove all leading QueryLines
    // (No leading newline should be present)
    while !queries.is_empty() {
        match queries[0] {
            ast::QueryDef::QueryLines { .. } => {
                queries.remove(0);
            }
            _ => break,
        }
    }

    // Handle trailing newlines: remove all trailing QueryLines
    // We'll handle trailing newlines in to_string to ensure exactly 2
    while !queries.is_empty() {
        let last_idx = queries.len() - 1;
        match queries[last_idx] {
            ast::QueryDef::QueryLines { .. } => {
                queries.remove(last_idx);
            }
            _ => break,
        }
    }

    // Don't add QueryLines here - let to_string handle the trailing newlines
    // (to_string already handles empty files by returning "\n\n" directly)
}

fn reorder_query_field(query_field: &mut ast::QueryField) {
    // Reorder args in this field
    reorder_query_field_args(&mut query_field.fields);

    // Recursively reorder nested fields
    for arg_field in &mut query_field.fields {
        match arg_field {
            ast::ArgField::Field(nested_field) => {
                reorder_query_field(nested_field);
            }
            _ => (),
        }
    }
}

fn reorder_query_field_args(arg_fields: &mut Vec<ast::ArgField>) {
    // Separate fields into categories
    let mut limits: Vec<ast::ArgField> = Vec::new();
    let mut sorts: Vec<ast::ArgField> = Vec::new();
    let mut wheres: Vec<ast::ArgField> = Vec::new();
    let mut fields: Vec<ast::ArgField> = Vec::new();
    let mut comments: Vec<ast::ArgField> = Vec::new();
    let mut lines: Vec<ast::ArgField> = Vec::new();

    for arg_field in arg_fields.drain(..) {
        match &arg_field {
            ast::ArgField::Arg(located_arg) => match &located_arg.arg {
                ast::Arg::Limit(_) => limits.push(arg_field),
                ast::Arg::OrderBy(_, _) => sorts.push(arg_field),
                ast::Arg::Where(_) => wheres.push(arg_field),
            },
            ast::ArgField::Field(_) => fields.push(arg_field),
            ast::ArgField::QueryComment { .. } => comments.push(arg_field),
            ast::ArgField::Lines { .. } => lines.push(arg_field),
        }
    }

    // Check if we have args and fields before moving
    let has_args = !limits.is_empty() || !sorts.is_empty() || !wheres.is_empty();
    let has_fields = !fields.is_empty();

    // Reassemble in the correct order
    arg_fields.clear();

    // 1. @limit
    arg_fields.extend(limits);

    // 2. @sort
    arg_fields.extend(sorts);

    // 3. @where
    arg_fields.extend(wheres);
    if has_args && has_fields {
        // Merge all lines into one if needed
        let mut total_lines = 0;
        for line in &lines {
            if let ast::ArgField::Lines { count } = line {
                total_lines += count;
            }
        }
        if total_lines == 0 {
            // Add a newline if there isn't one already
            arg_fields.push(ast::ArgField::Lines { count: 1 });
        } else {
            // Use existing lines but merge them
            arg_fields.push(ast::ArgField::Lines {
                count: total_lines.min(2),
            });
        }
    } else if !lines.is_empty() {
        // If we have lines but no args/fields separation needed, preserve them
        let mut total_lines = 0;
        for line in &lines {
            if let ast::ArgField::Lines { count } = line {
                total_lines += count;
            }
        }
        arg_fields.push(ast::ArgField::Lines {
            count: total_lines.min(2),
        });
    }

    // 5. Fields
    arg_fields.extend(fields);

    // Comments are preserved with their relative positions (after fields for simplicity)
    arg_fields.extend(comments);
}
