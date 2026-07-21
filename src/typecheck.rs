use crate::error::{DefInfo, Error, ErrorType, Location, Range, VariantDef};
use crate::{ast, error, platform};
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Debug)]
pub enum Type {
    Integer,
    Float,
    String,
    OneOf { variants: Vec<ast::Variant> },
    Record(ast::RecordDetails),
}

pub struct SqlColumnInfo {
    pub name: String,
    pub nullable: bool,
    pub type_: ast::ConcreteSerializationType,
    pub directives: Vec<ast::ColumnDirective>,
}

pub fn to_sql_column_info(context: &Context, fields: &Vec<ast::Field>) -> Vec<SqlColumnInfo> {
    let mut infos = vec![];

    for col in fields {
        field_to_sql_column(context, col, None, &mut infos);
    }
    infos
}

fn field_to_sql_column(
    context: &Context,
    field: &ast::Field,
    parent_name: Option<String>,
    gathered: &mut Vec<SqlColumnInfo>,
) {
    match field {
        ast::Field::Column(column) => match column.type_.to_serialization_type() {
            ast::SerializationType::Concrete(concrete) => gathered.push(SqlColumnInfo {
                name: match parent_name {
                    None => column.name.clone(),
                    Some(prefix) => {
                        format!("{}{}", prefix, column.name)
                    }
                },

                nullable: column.nullable,
                type_: concrete.clone(),
                directives: column.directives.clone(),
            }),
            ast::SerializationType::FromType(typename) => {
                match context.types.get(&typename) {
                    Some((_, type_)) => match type_ {
                        Type::OneOf { variants } => {
                            // We need a column for the discriminator
                            gathered.push(SqlColumnInfo {
                                name: match &parent_name {
                                    None => column.name.clone(),
                                    Some(prefix) => format!("{}{}", prefix, column.name),
                                },
                                nullable: column.nullable,
                                type_: ast::ConcreteSerializationType::Text,
                                directives: column.directives.clone(),
                            });

                            let base_name = match parent_name {
                                None => &format!("{}__", column.name),
                                Some(parent) => &format!("{}{}__", parent, column.name),
                            };
                            //
                            for var in variants {
                                // For each variant, we need to capture any additional fields
                                // Prefixed by the type name and variant name
                                // So, statusFieldName: Status.Active { activatedAt: DateTime }
                                // Turns into a colum  of statusFieldName__activatedAt
                                // Variants that have the same fields will share the same columns in the db
                                // If types are nested, then they'll be recursively namespaced by the fieldnames

                                match &var.fields {
                                    Some(var_fields) => {
                                        for var_f in var_fields {
                                            field_to_sql_column(
                                                context,
                                                &var_f,
                                                Some(base_name.to_string()),
                                                gathered,
                                            );
                                        }
                                    }
                                    None => continue,
                                }
                            }
                        }
                        _ => (),
                    },
                    None => {
                        // Should have been caught in typechecking
                    }
                }
            }
        },
        ast::Field::ColumnLines { .. } => (),
        ast::Field::ColumnComment { .. } => (),
        ast::Field::FieldDirective(_) => (),
    }
}

#[derive(Debug)]
pub struct Context {
    pub current_filepath: String,

    pub valid_namespaces: HashSet<String>,
    pub namespace_sync_modes: HashMap<String, ast::SyncMode>,
    pub session: Option<ast::SessionDetails>,
    pub funcs: HashMap<String, platform::FuncDefinition>,

    pub types: HashMap<String, (DefInfo, Type)>,
    pub tables: HashMap<String, Table>,

    // All variants by type name + variant name.
    // Used to check if there are multiple variants with the same name in a type.
    // Tracks the location of type definitions so they can be reported in the error.
    pub variants: HashMap<String, (Option<Range>, Vec<VariantDef>)>,
}

#[derive(Debug)]
pub struct Table {
    pub schema: String,
    pub record: ast::RecordDetails,
    /// Topological layer for sync ordering. Lower numbers sync first.
    /// Tables in cycles get the same layer number.
    pub sync_layer: usize,
    /// Filepath of the schema file where this record is defined
    pub filepath: String,
}

fn convert_range(range: &ast::Range) -> Range {
    Range {
        start: range.start.clone(),
        end: range.end.clone(),
    }
}

pub fn get_linked_table<'a>(context: &'a Context, link: &'a ast::LinkDetails) -> Option<&'a Table> {
    context.tables.values().find(|table| {
        table.schema == link.foreign.schema && table.record.name == link.foreign.table
    })
}

pub fn empty_context() -> Context {
    let mut fns = HashMap::new();
    platform::add_builtin(&mut fns);

    let mut context = Context {
        current_filepath: "".to_string(),
        valid_namespaces: HashSet::new(),
        namespace_sync_modes: HashMap::new(),
        session: None,
        funcs: fns,
        types: HashMap::new(),
        tables: HashMap::new(),
        variants: HashMap::new(),
    };
    context
        .types
        .insert("String".to_string(), (DefInfo::Builtin, Type::String));
    context
        .types
        .insert("Int".to_string(), (DefInfo::Builtin, Type::Integer));
    context
        .types
        .insert("Float".to_string(), (DefInfo::Builtin, Type::Float));
    context.types.insert(
        "Bool".to_string(),
        (
            DefInfo::Builtin,
            Type::OneOf {
                variants: vec![ast::to_variant("True"), ast::to_variant("False")],
            },
        ),
    );
    context
        .types
        .insert("DateTime".to_string(), (DefInfo::Builtin, Type::String));
    context
        .types
        .insert("Json".to_string(), (DefInfo::Builtin, Type::String));
    context
        .types
        .insert("Id.Int".to_string(), (DefInfo::Builtin, Type::String));
    context
        .types
        .insert("Id.Uuid".to_string(), (DefInfo::Builtin, Type::String));

    context
}

fn known_types_for_error(context: &Context) -> Vec<String> {
    context
        .types
        .iter()
        .filter_map(|(name, (_, type_))| match type_ {
            Type::Record(_) => None,
            _ => Some(name.clone()),
        })
        .collect()
}

fn is_capitalized(s: &str) -> bool {
    if let Some(first_char) = s.chars().next() {
        first_char.is_uppercase()
    } else {
        false
    }
}

fn default_value_display(value: &ast::DefaultValue) -> String {
    match value {
        ast::DefaultValue::Now => "now".to_string(),
        ast::DefaultValue::Value(query_value) => match query_value {
            ast::QueryValue::String((_, s)) => format!("\"{}\"", s),
            ast::QueryValue::Int((_, i)) => i.to_string(),
            ast::QueryValue::Float((_, f)) => f.to_string(),
            ast::QueryValue::Bool((_, b)) => b.to_string(),
            ast::QueryValue::Null(_) => "null".to_string(),
            ast::QueryValue::LiteralTypeValue((_, details)) => details.name.clone(),
            ast::QueryValue::Fn(func) => format!("{}()", func.name),
            ast::QueryValue::Variable((_, details)) => format!("${}", details.name),
        },
    }
}

fn allowed_defaults_for_column_type(column: &ast::Column) -> Vec<String> {
    let mut allowed = match &column.type_ {
        ast::ColumnType::String => vec!["a string literal".to_string()],
        ast::ColumnType::Int => vec!["an integer literal".to_string()],
        ast::ColumnType::Float => vec!["a number literal".to_string()],
        ast::ColumnType::Bool => vec!["true".to_string(), "false".to_string()],
        ast::ColumnType::DateTime => vec!["now".to_string(), "an integer literal".to_string()],
        ast::ColumnType::Date => vec!["a string literal".to_string()],
        ast::ColumnType::Json | ast::ColumnType::JsonTyped(_) => vec![],
        ast::ColumnType::List(_) | ast::ColumnType::Dict(_) => vec![],
        ast::ColumnType::Nullable(inner) => {
            let nested = ast::Column {
                name: column.name.clone(),
                type_: (*inner.clone()),
                nullable: true,
                directives: vec![],
                start: None,
                end: None,
                start_name: None,
                end_name: None,
                start_typename: None,
                end_typename: None,
                inline_comment: None,
            };
            allowed_defaults_for_column_type(&nested)
        }
        ast::ColumnType::IdInt { .. } | ast::ColumnType::IdUuid { .. } => vec![],
        ast::ColumnType::ForeignKey { .. } => vec!["an integer or string literal".to_string()],
        ast::ColumnType::Custom(_) => vec!["a literal value matching the field type".to_string()],
    };

    if column.nullable && !column.type_.is_json_like() {
        allowed.push("null".to_string());
    }

    allowed
}

fn is_supported_default_for_column(column: &ast::Column, value: &ast::DefaultValue) -> bool {
    match value {
        ast::DefaultValue::Now => matches!(column.type_, ast::ColumnType::DateTime),
        ast::DefaultValue::Value(query_value) => match query_value {
            ast::QueryValue::String(_) => matches!(
                column.type_,
                ast::ColumnType::String
                    | ast::ColumnType::Date
                    | ast::ColumnType::IdUuid { .. }
                    | ast::ColumnType::ForeignKey { .. }
            ),
            ast::QueryValue::Int(_) => matches!(
                column.type_,
                ast::ColumnType::Int
                    | ast::ColumnType::Float
                    | ast::ColumnType::DateTime
                    | ast::ColumnType::ForeignKey { .. }
            ),
            ast::QueryValue::Float(_) => {
                matches!(column.type_, ast::ColumnType::Float)
            }
            ast::QueryValue::Bool(_) => matches!(column.type_, ast::ColumnType::Bool),
            ast::QueryValue::Null(_) => column.nullable && !column.type_.is_json_like(),
            ast::QueryValue::Fn(_) => false,
            ast::QueryValue::Variable(_) => false,
            ast::QueryValue::LiteralTypeValue(_) => false,
        },
    }
}

fn invalid_type_usage_error(
    filepath: &str,
    message: String,
    contexts: Vec<Range>,
    primary: Vec<Range>,
) -> Error {
    Error {
        filepath: filepath.to_string(),
        error_type: ErrorType::InvalidTypeUsage { message },
        locations: vec![Location { contexts, primary }],
    }
}

fn validate_type_fields(
    context: &Context,
    filepath: &str,
    contexts: Vec<Range>,
    primary: Vec<Range>,
    fields: &Vec<ast::Field>,
    inside_json: bool,
    seen_types: &mut HashSet<String>,
    errors: &mut Vec<Error>,
) {
    for field in fields {
        match field {
            ast::Field::Column(column) => validate_type_expr(
                context,
                filepath,
                contexts.clone(),
                primary.clone(),
                &column.type_,
                inside_json,
                seen_types,
                errors,
            ),
            ast::Field::FieldDirective(ast::FieldDirective::Link(_)) if inside_json => {
                errors.push(invalid_type_usage_error(
                    filepath,
                    "Relationships are not allowed inside Json<...>.".to_string(),
                    contexts.clone(),
                    primary.clone(),
                ));
            }
            _ => {}
        }
    }
}

fn validate_type_expr(
    context: &Context,
    filepath: &str,
    contexts: Vec<Range>,
    primary: Vec<Range>,
    type_: &ast::ColumnType,
    inside_json: bool,
    seen_types: &mut HashSet<String>,
    errors: &mut Vec<Error>,
) {
    match type_ {
        ast::ColumnType::String
        | ast::ColumnType::Int
        | ast::ColumnType::Float
        | ast::ColumnType::Bool
        | ast::ColumnType::DateTime
        | ast::ColumnType::Date
        | ast::ColumnType::Json
        | ast::ColumnType::IdInt { .. }
        | ast::ColumnType::IdUuid { .. }
        | ast::ColumnType::ForeignKey { .. } => {}
        ast::ColumnType::JsonTyped(inner) => validate_type_expr(
            context, filepath, contexts, primary, inner, true, seen_types, errors,
        ),
        ast::ColumnType::List(inner) | ast::ColumnType::Dict(inner) => {
            if !inside_json {
                errors.push(invalid_type_usage_error(
                    filepath,
                    format!("{} may only appear inside Json<...>.", type_),
                    contexts.clone(),
                    primary.clone(),
                ));
            }

            validate_type_expr(
                context, filepath, contexts, primary, inner, true, seen_types, errors,
            );
        }
        ast::ColumnType::Nullable(inner) => validate_type_expr(
            context,
            filepath,
            contexts,
            primary,
            inner,
            inside_json,
            seen_types,
            errors,
        ),
        ast::ColumnType::Custom(name) => {
            let Some((_def_info, type_definition)) = context.types.get(name) else {
                errors.push(Error {
                    filepath: filepath.to_string(),
                    error_type: ErrorType::UnknownType {
                        found: type_.to_string(),
                        known_types: known_types_for_error(context),
                    },
                    locations: vec![Location { contexts, primary }],
                });
                return;
            };

            if !seen_types.insert(name.clone()) {
                return;
            }

            match type_definition {
                Type::Record(record) => validate_type_fields(
                    context,
                    filepath,
                    contexts,
                    primary,
                    &record.fields,
                    inside_json,
                    seen_types,
                    errors,
                ),
                Type::OneOf { variants } => {
                    if !inside_json {
                        validate_variant_field_type_collisions(filepath, variants, errors);
                    }

                    for variant in variants {
                        if let Some(fields) = &variant.fields {
                            validate_type_fields(
                                context,
                                filepath,
                                contexts.clone(),
                                primary.clone(),
                                fields,
                                inside_json,
                                seen_types,
                                errors,
                            );
                        }
                    }
                }
                Type::Integer | Type::Float | Type::String => {}
            }

            seen_types.remove(name);
        }
    }
}

fn validate_variant_field_type_collisions(
    filepath: &str,
    variants: &Vec<ast::Variant>,
    errors: &mut Vec<Error>,
) {
    let mut field_types: HashMap<String, (String, String, Option<Range>, Option<Range>)> =
        HashMap::new();

    for variant in variants {
        if let Some(fields) = &variant.fields {
            for field in ast::collect_columns(fields) {
                let field_type_str = field.type_.to_string();
                let field_range = to_single_range(&field.start_typename, &field.end_typename);

                match field_types.get(&field.name) {
                    Some((
                        existing_type,
                        existing_variant_name,
                        existing_field_range,
                        existing_variant_range,
                    )) => {
                        if existing_type != &field_type_str {
                            let mut contexts: Vec<Range> = Vec::new();
                            if let Some(existing_range) = existing_variant_range.clone() {
                                contexts.push(Range {
                                    start: existing_range.start.clone(),
                                    end: existing_range.start.clone(),
                                });
                            }
                            if let Some(current_range) =
                                to_single_range(&variant.start, &variant.end)
                            {
                                contexts.push(Range {
                                    start: current_range.start.clone(),
                                    end: current_range.start.clone(),
                                });
                            }

                            let mut primary_ranges: Vec<Range> = Vec::new();
                            if let Some(existing_range) = existing_field_range.clone() {
                                primary_ranges.push(existing_range);
                            }
                            if let Some(current_range) = field_range.clone() {
                                primary_ranges.push(current_range);
                            }

                            errors.push(Error {
                                filepath: filepath.to_string(),
                                error_type: ErrorType::VariantFieldTypeCollision {
                                    field: field.name.clone(),
                                    type_one: existing_type.clone(),
                                    type_two: field_type_str.clone(),
                                    variant_one: existing_variant_name.clone(),
                                    variant_two: variant.name.clone(),
                                },
                                locations: vec![Location {
                                    contexts: contexts.clone(),
                                    primary: primary_ranges,
                                }],
                            });
                        }
                    }
                    None => {
                        field_types.insert(
                            field.name.clone(),
                            (
                                field_type_str,
                                variant.name.clone(),
                                field_range,
                                to_single_range(&variant.start, &variant.end),
                            ),
                        );
                    }
                }
            }
        }
    }
}

pub fn check_schema(db: &ast::Database) -> Result<Context, Vec<Error>> {
    let mut context = populate_context(db)?;

    let mut errors: Vec<Error> = Vec::new();

    // Namespaces must be capitalized
    for schem in &db.schemas {
        if schem.namespace == ast::DEFAULT_SCHEMANAME {
            continue;
        } else if !is_capitalized(&schem.namespace) {
            let body = format!(
                "This schema name must be capitalized: {}",
                &error::yellow_if(true, &schem.namespace)
            );
            let error = error::format_custom_error("Schema name formatting", &body);
            eprintln!("{}", error);
            std::process::exit(1);
        }
    }

    check_schema_definitions(&context, db, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    // Compute topological layers for sync ordering
    compute_sync_layers(&mut context);

    Ok(context)
}

/// Compute topological layers for sync ordering.
/// Tables with lower layer numbers should be synced first.
/// Tables in cycles get the same layer number.
fn compute_sync_layers(context: &mut Context) {
    use crate::ast;

    // Build dependency graph: child -> [parent1, parent2, ...]
    // A table depends on (must sync after) tables it references via @link
    let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_tables = HashSet::new();

    // Collect all table names and their dependencies
    for (_record_name, table) in &context.tables {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        all_tables.insert(table_name.clone());

        // Get all links from this table
        let links = ast::collect_links(&table.record.fields);
        let mut parents = Vec::new();

        for link in links {
            // Get the foreign table name
            if let Some(foreign_table) = get_linked_table(context, &link) {
                let foreign_table_name =
                    ast::get_tablename(&foreign_table.record.name, &foreign_table.record.fields);
                parents.push(foreign_table_name);
            }
        }

        if !parents.is_empty() {
            dependencies.insert(table_name, parents);
        }
    }

    // Find strongly connected components (cycles) using Tarjan's algorithm
    let sccs = find_strongly_connected_components(&all_tables, &dependencies);

    // Build a map from table name to its SCC ID
    let mut table_to_scc: HashMap<String, usize> = HashMap::new();
    for (scc_id, scc) in sccs.iter().enumerate() {
        for table in scc {
            table_to_scc.insert(table.clone(), scc_id);
        }
    }

    // Build dependency graph of SCCs (DAG)
    let mut scc_dependencies: HashMap<usize, Vec<usize>> = HashMap::new();
    for (child_table, parent_tables) in &dependencies {
        let child_scc = table_to_scc.get(child_table).copied();
        if let Some(child_scc_id) = child_scc {
            for parent_table in parent_tables {
                if let Some(parent_scc_id) = table_to_scc.get(parent_table).copied() {
                    if child_scc_id != parent_scc_id {
                        // Different SCCs, add dependency
                        scc_dependencies
                            .entry(child_scc_id)
                            .or_insert_with(Vec::new)
                            .push(parent_scc_id);
                    }
                }
            }
        }
    }

    // Topological sort of SCCs (Kahn's algorithm)
    let scc_layers = topological_sort_sccs(&sccs, &scc_dependencies);

    // Assign layer numbers to tables based on their SCC's layer
    for (_record_name, table) in context.tables.iter_mut() {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        if let Some(scc_id) = table_to_scc.get(&table_name) {
            if let Some(layer) = scc_layers.get(scc_id) {
                table.sync_layer = *layer;
            }
        }
    }
}

/// Find strongly connected components using Tarjan's algorithm
fn find_strongly_connected_components(
    all_tables: &HashSet<String>,
    dependencies: &HashMap<String, Vec<String>>,
) -> Vec<Vec<String>> {
    let mut index = 0;
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut lowlinks: HashMap<String, usize> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut sccs: Vec<Vec<String>> = Vec::new();

    fn strong_connect(
        table: &String,
        dependencies: &HashMap<String, Vec<String>>,
        index: &mut usize,
        indices: &mut HashMap<String, usize>,
        lowlinks: &mut HashMap<String, usize>,
        stack: &mut Vec<String>,
        on_stack: &mut HashSet<String>,
        sccs: &mut Vec<Vec<String>>,
    ) {
        indices.insert(table.clone(), *index);
        lowlinks.insert(table.clone(), *index);
        *index += 1;
        stack.push(table.clone());
        on_stack.insert(table.clone());

        // Consider successors
        if let Some(parents) = dependencies.get(table) {
            for parent in parents {
                if !indices.contains_key(parent) {
                    strong_connect(
                        parent,
                        dependencies,
                        index,
                        indices,
                        lowlinks,
                        stack,
                        on_stack,
                        sccs,
                    );
                    let lowlink = *lowlinks.get(table).unwrap();
                    let parent_lowlink = *lowlinks.get(parent).unwrap();
                    lowlinks.insert(table.clone(), lowlink.min(parent_lowlink));
                } else if on_stack.contains(parent) {
                    let lowlink = *lowlinks.get(table).unwrap();
                    let parent_index = *indices.get(parent).unwrap();
                    lowlinks.insert(table.clone(), lowlink.min(parent_index));
                }
            }
        }

        // If table is a root node, pop the stack and form an SCC
        if lowlinks.get(table) == indices.get(table) {
            let mut scc = Vec::new();
            loop {
                let w = stack.pop().unwrap();
                on_stack.remove(&w);
                scc.push(w.clone());
                if w == *table {
                    break;
                }
            }
            sccs.push(scc);
        }
    }

    for table in all_tables {
        if !indices.contains_key(table) {
            strong_connect(
                table,
                dependencies,
                &mut index,
                &mut indices,
                &mut lowlinks,
                &mut stack,
                &mut on_stack,
                &mut sccs,
            );
        }
    }

    sccs
}

/// Topological sort of SCCs using Kahn's algorithm
/// Returns a map from SCC ID to layer number
fn topological_sort_sccs(
    sccs: &[Vec<String>],
    scc_dependencies: &HashMap<usize, Vec<usize>>,
) -> HashMap<usize, usize> {
    let mut layers: HashMap<usize, usize> = HashMap::new();
    let mut in_degree: HashMap<usize, usize> = HashMap::new();

    // Initialize in-degrees for all SCCs
    for scc_id in 0..sccs.len() {
        in_degree.insert(scc_id, 0);
    }

    // Calculate in-degrees (count how many dependencies each SCC has)
    // If child_scc depends on parent_sccs, increment in-degree of child_scc
    for (child_scc_id, parent_sccs) in scc_dependencies {
        // Each parent dependency increases the child's in-degree
        *in_degree.get_mut(child_scc_id).unwrap() += parent_sccs.len();
    }

    // Find all SCCs with no incoming edges (layer 0)
    let mut queue: Vec<usize> = Vec::new();
    for scc_id in 0..sccs.len() {
        let degree = in_degree.get(&scc_id).copied().unwrap_or(0);
        if degree == 0 {
            queue.push(scc_id);
            layers.insert(scc_id, 0);
        }
    }

    // Process queue
    let mut current_layer = 0;
    while !queue.is_empty() {
        let mut next_queue = Vec::new();
        current_layer += 1;

        for scc_id in queue.drain(..) {
            // Process all SCCs that depend on this one (scc_id)
            // Find all child SCCs that have scc_id as a parent
            for (child_scc_id, parent_sccs) in scc_dependencies {
                if parent_sccs.contains(&scc_id) {
                    let degree = in_degree.get_mut(child_scc_id).unwrap();
                    *degree -= 1;
                    if *degree == 0 {
                        next_queue.push(*child_scc_id);
                        layers.insert(*child_scc_id, current_layer);
                    }
                }
            }
        }

        queue = next_queue;
    }

    // Handle any remaining SCCs (shouldn't happen in a DAG, but handle gracefully)
    for scc_id in 0..sccs.len() {
        if !layers.contains_key(&scc_id) {
            layers.insert(scc_id, current_layer);
        }
    }

    layers
}

fn to_range(start: &Option<ast::Location>, end: &Option<ast::Location>) -> Vec<Range> {
    match start {
        None => vec![],
        Some(s) => match end {
            None => vec![],
            Some(e) => vec![
                (Range {
                    start: s.clone(),
                    end: e.clone(),
                }),
            ],
        },
    }
}

fn to_single_range(start: &Option<ast::Location>, end: &Option<ast::Location>) -> Option<Range> {
    match start {
        None => None,
        Some(s) => match end {
            None => None,
            Some(e) => Some(Range {
                start: s.clone(),
                end: e.clone(),
            }),
        },
    }
}

pub fn query_param_type_for_column(table: &ast::RecordDetails, column: &ast::Column) -> String {
    if column.type_.is_id_type() {
        let table_name = column.type_.table_name().unwrap_or(table.name.as_str());
        format!("{}.{}", table_name, column.name)
    } else {
        column.type_.query_type_string()
    }
}

/// Resolves record field references used in query parameter declarations to the
/// field's concrete type. Parsed references otherwise retain an integer fallback.
pub fn resolve_query_param_type(context: &Context, type_: &str) -> String {
    let ast::ColumnType::ForeignKey { table, field, .. } = ast::ColumnType::from_str(type_) else {
        return type_.to_string();
    };

    context
        .tables
        .values()
        .find(|candidate| candidate.record.name == table)
        .and_then(|candidate| {
            candidate
                .record
                .fields
                .iter()
                .find_map(|field_| match field_ {
                    ast::Field::Column(column) if column.name == field => {
                        Some(column.type_.query_type_string())
                    }
                    _ => None,
                })
        })
        .unwrap_or_else(|| type_.to_string())
}

/// Gathers information for a context.
/// Also checks for a number of errors
pub fn populate_context(database: &ast::Database) -> Result<Context, Vec<Error>> {
    let mut context = empty_context();
    let mut errors = Vec::new();
    context.valid_namespaces = database
        .schemas
        .iter()
        .map(|schema| schema.namespace.clone())
        .collect();
    context.namespace_sync_modes = database
        .schemas
        .iter()
        .map(|schema| (schema.namespace.clone(), schema.sync_mode))
        .collect();

    // Check for duplicate records
    // Check for duplicate types
    // Gather table names
    // Gather type names
    for schema in &database.schemas {
        if context.session.is_none() && schema.session.is_some() {
            context.session = schema.session.clone();
        }

        for file in &schema.files {
            for definition in &file.definitions {
                match definition {
                    ast::Definition::Record {
                        name,
                        fields,
                        start,
                        end,
                        start_name,
                        end_name,
                    } => {
                        match context.types.get(name) {
                            None => (),
                            Some((DefInfo::Def(loc), _)) => {
                                let mut locations: Vec<Location> = vec![];
                                locations.push(Location {
                                    contexts: vec![],
                                    primary: loc.clone().into_iter().collect(),
                                });
                                locations.push(Location {
                                    contexts: vec![],
                                    primary: to_range(start_name, end_name),
                                });
                                errors.push(Error {
                                    filepath: file.path.clone(),
                                    error_type: ErrorType::DuplicateDefinition(name.clone()),
                                    locations,
                                });
                            }
                            Some((DefInfo::Builtin, _)) => {
                                let mut locations: Vec<Location> = vec![];
                                locations.push(Location {
                                    contexts: vec![],
                                    primary: to_range(start_name, end_name),
                                });
                                errors.push(Error {
                                    filepath: file.path.clone(),
                                    error_type: ErrorType::DefinitionIsBuiltIn(name.clone()),
                                    locations,
                                });
                            }
                        }
                        let mut fields_with_updated_at = ast::with_timestamps_fields(fields);
                        // Ensure updatedAt field exists
                        ast::ensure_updated_at_field(&mut fields_with_updated_at);

                        let record_details = ast::RecordDetails {
                            name: name.clone(),
                            fields: fields_with_updated_at.clone(),
                            start: start.clone(),
                            end: end.clone(),
                            start_name: start_name.clone(),
                            end_name: end_name.clone(),
                        };

                        context.types.insert(
                            name.clone(),
                            (
                                DefInfo::Def(to_single_range(start_name, end_name)),
                                Type::Record(record_details.clone()),
                            ),
                        );

                        for field in &record_details.fields {
                            if let ast::Field::Column(column) = field {
                                if column.type_.is_id_type() {
                                    let type_name =
                                        query_param_type_for_column(&record_details, column);
                                    context
                                        .types
                                        .entry(type_name)
                                        .or_insert((DefInfo::Builtin, Type::String));
                                }
                            }
                        }

                        context.tables.insert(
                            crate::ext::string::decapitalize(&name),
                            Table {
                                schema: schema.namespace.clone(),
                                record: record_details,
                                sync_layer: 0, // Will be computed later
                                filepath: file.path.clone(),
                            },
                        );
                    }
                    ast::Definition::Tagged {
                        name,
                        variants,
                        start,
                        end,
                    } => {
                        match context.types.get(name) {
                            None => (),
                            Some((DefInfo::Def(loc), _)) => {
                                let mut locations: Vec<Location> = vec![];
                                locations.push(Location {
                                    contexts: vec![],
                                    primary: loc.clone().into_iter().collect(),
                                });
                                locations.push(Location {
                                    contexts: vec![],
                                    primary: to_range(start, end),
                                });
                                errors.push(Error {
                                    filepath: file.path.clone(),
                                    error_type: ErrorType::DuplicateDefinition(name.clone()),
                                    locations,
                                });
                            }
                            Some((DefInfo::Builtin, _)) => {
                                let mut locations: Vec<Location> = vec![];
                                locations.push(Location {
                                    contexts: vec![],
                                    primary: to_range(start, end),
                                });
                                errors.push(Error {
                                    filepath: file.path.clone(),
                                    error_type: ErrorType::DefinitionIsBuiltIn(name.clone()),
                                    locations,
                                });
                            }
                        }
                        context.types.insert(
                            name.clone(),
                            (
                                DefInfo::Def(to_single_range(start, end)),
                                Type::OneOf {
                                    variants: variants.clone(),
                                },
                            ),
                        );

                        for variant in variants {
                            let variant_def = VariantDef {
                                typename: name.clone(),
                                variant_name: variant.name.clone(),
                                filepath: file.path.clone(),
                                range: to_single_range(&variant.start_name, &variant.end_name),
                            };

                            let type_range = to_single_range(&start, &end);

                            // Add variant to the map, creating a new entry with type range if it doesn't exist
                            let variant_key = format!("{}::{}", name, variant.name);
                            context
                                .variants
                                .entry(variant_key)
                                .or_insert_with(|| (type_range, Vec::new()))
                                .1
                                .push(variant_def);
                        }
                    }
                    ast::Definition::SyncMode(_) => {}
                    _ => {}
                }
            }
        }
    }

    // Check for duplicate fields
    // Check for duplicate tablenames
    //
    for schema in &database.schemas {
        for file in &schema.files {
            for definition in &file.definitions {
                match definition {
                    ast::Definition::Record {
                        name,
                        fields,
                        start,
                        end,
                        start_name,
                        end_name,
                    } => {
                        let mut tablenames: Vec<Range> = Vec::new();
                        let mut has_primary_id = false;
                        let mut field_names = HashSet::new();
                        let mut permission_directives: Vec<(ast::PermissionDetails, Range)> =
                            Vec::new();

                        let fields = ast::with_timestamps_fields(fields);
                        for field in &fields {
                            match field {
                                ast::Field::Column(column) => {
                                    if field_names.contains(&column.name) {
                                        errors.push(Error {
                                            filepath: file.path.clone(),
                                            error_type: ErrorType::DuplicateField {
                                                record: name.clone(),
                                                field: column.name.clone(),
                                            },
                                            locations: vec![Location {
                                                contexts: to_range(&start, &end),
                                                primary: to_range(&column.start, &column.end),
                                            }],
                                        });
                                    }
                                    if column
                                        .directives
                                        .iter()
                                        .any(|item| *item == ast::ColumnDirective::PrimaryKey)
                                    {
                                        if has_primary_id {
                                            errors.push(Error {
                                                filepath: file.path.clone(),
                                                error_type: ErrorType::MultiplePrimaryKeys {
                                                    record: name.clone(),
                                                    field: name.clone(),
                                                },
                                                locations: vec![Location {
                                                    contexts: to_range(&start, &end),
                                                    primary: to_range(
                                                        &column.start_name,
                                                        &column.end_name,
                                                    ),
                                                }],
                                            });
                                        }
                                        has_primary_id = true;
                                    }

                                    // Validate foreign key references
                                    if let ast::ColumnType::ForeignKey {
                                        table: ref_table,
                                        field: ref_field,
                                        ..
                                    } = &column.type_
                                    {
                                        // Check if the referenced table exists
                                        let ref_table_lower =
                                            crate::ext::string::decapitalize(ref_table);
                                        match context.tables.get(&ref_table_lower) {
                                            Some(foreign_table) => {
                                                // Check if the referenced field exists
                                                let ref_column =
                                                    foreign_table.record.fields.iter().find_map(
                                                        |f| match f {
                                                            ast::Field::Column(col)
                                                                if col.name == *ref_field =>
                                                            {
                                                                Some(col)
                                                            }
                                                            _ => None,
                                                        },
                                                    );

                                                match ref_column {
                                                    Some(ref_col) => {
                                                        // Check if the referenced field is an ID type
                                                        if !ref_col.type_.is_id_type() {
                                                            errors.push(Error {
                                                                filepath: file.path.clone(),
                                                                error_type:
                                                                    ErrorType::ForeignKeyToNonIdField {
                                                                        field_name: column
                                                                            .name
                                                                            .clone(),
                                                                        referenced_table: ref_table
                                                                            .clone(),
                                                                        referenced_field: ref_field
                                                                            .clone(),
                                                                        referenced_field_type: ref_col
                                                                            .type_
                                                                            .to_string(),
                                                                    },
                                                                locations: vec![Location {
                                                                    contexts: to_range(&start, &end),
                                                                    primary: to_range(
                                                                        &column.start_typename,
                                                                        &column.end_typename,
                                                                    ),
                                                                }],
                                                            });
                                                        }
                                                    }
                                                    None => {
                                                        // Field doesn't exist
                                                        let existing_fields: Vec<String> =
                                                            foreign_table
                                                                .record
                                                                .fields
                                                                .iter()
                                                                .filter_map(|f| match f {
                                                                    ast::Field::Column(col) => {
                                                                        Some(col.name.clone())
                                                                    }
                                                                    _ => None,
                                                                })
                                                                .collect();
                                                        errors.push(Error {
                                                            filepath: file.path.clone(),
                                                            error_type:
                                                                ErrorType::ForeignKeyToUnknownField {
                                                                    field_name: column.name.clone(),
                                                                    referenced_table: ref_table
                                                                        .clone(),
                                                                    referenced_field: ref_field
                                                                        .clone(),
                                                                    existing_fields,
                                                                },
                                                            locations: vec![Location {
                                                                contexts: to_range(&start, &end),
                                                                primary: to_range(
                                                                    &column.start_typename,
                                                                    &column.end_typename,
                                                                ),
                                                            }],
                                                        });
                                                    }
                                                }
                                            }
                                            None => {
                                                // Table doesn't exist
                                                let existing_tables: Vec<String> =
                                                    context.tables.keys().cloned().collect();
                                                errors.push(Error {
                                                    filepath: file.path.clone(),
                                                    error_type:
                                                        ErrorType::ForeignKeyToUnknownTable {
                                                            field_name: column.name.clone(),
                                                            referenced_table: ref_table.clone(),
                                                            existing_tables,
                                                        },
                                                    locations: vec![Location {
                                                        contexts: to_range(&start, &end),
                                                        primary: to_range(
                                                            &column.start_typename,
                                                            &column.end_typename,
                                                        ),
                                                    }],
                                                });
                                            }
                                        }
                                    }

                                    field_names.insert(name.clone());
                                }

                                ast::Field::FieldDirective(ast::FieldDirective::TableName((
                                    tablename_range,
                                    tablename,
                                ))) => {
                                    tablenames.push(convert_range(tablename_range));
                                    if !crate::ext::string::is_safe_sql_identifier(tablename) {
                                        errors.push(Error {
                                            filepath: file.path.clone(),
                                            error_type: ErrorType::InvalidTypeUsage {
                                                message: format!(
                                                    "Invalid @tablename value '{}'. Table names must contain only letters, numbers, and underscores, and cannot start with a number.",
                                                    tablename
                                                ),
                                            },
                                            locations: vec![Location {
                                                contexts: to_range(&start, &end),
                                                primary: vec![convert_range(tablename_range)],
                                            }],
                                        });
                                    }
                                }

                                ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                                    perm,
                                )) => {
                                    // Use record range for permission directives since Field doesn't store individual ranges
                                    // Extract first range from Vec<Range> returned by to_range
                                    let perm_ranges = to_range(&start, &end);
                                    let perm_range =
                                        perm_ranges.first().cloned().unwrap_or_else(|| {
                                            let default_loc = ast::Location {
                                                line: 0,
                                                column: 0,
                                                offset: 0,
                                            };
                                            Range {
                                                start: start.clone().unwrap_or(default_loc.clone()),
                                                end: end.clone().unwrap_or(default_loc),
                                            }
                                        });
                                    permission_directives.push((perm.clone(), perm_range));
                                }

                                ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                                    if !context.valid_namespaces.contains(&link.foreign.schema) {
                                        errors.push(Error {
                                            filepath: file.path.clone(),
                                            error_type: ErrorType::LinkToUnknownSchema {
                                                unknown_schema_name: link.foreign.schema.clone(),
                                                known_schemas: context.valid_namespaces.clone(),
                                            },
                                            locations: vec![Location {
                                                contexts: vec![],
                                                primary: to_range(&start, &end),
                                            }],
                                        });
                                    }

                                    let maybe_foreign_table = get_linked_table(&context, link);

                                    if field_names.contains(&link.link_name) {
                                        errors.push(Error {
                                            filepath: file.path.clone(),
                                            error_type: ErrorType::DuplicateField {
                                                record: name.clone(),
                                                field: link.link_name.clone(),
                                            },
                                            locations: vec![Location {
                                                contexts: vec![],
                                                primary: to_range(&start, &end),
                                            }],
                                        });
                                    }

                                    match maybe_foreign_table {
                                        Some(foreign_table) => {
                                            for foreign_id in &link.foreign.fields {
                                                if !foreign_table
                                                    .record
                                                    .fields
                                                    .iter()
                                                    .any(|f| ast::has_fieldname(f, foreign_id))
                                                {
                                                    errors.push(Error {
                                                        filepath: file.path.clone(),
                                                        error_type:
                                                            ErrorType::LinkToUnknownForeignField {
                                                                link_name: link.link_name.clone(),
                                                                foreign_table: link
                                                                    .foreign
                                                                    .table
                                                                    .clone(),
                                                                unknown_foreign_field: foreign_id
                                                                    .clone(),
                                                            },
                                                        locations: vec![Location {
                                                            contexts: to_range(&start, &end),
                                                            primary: to_range(
                                                                &link.start_name,
                                                                &link.end_name,
                                                            ),
                                                        }],
                                                    });
                                                }
                                            }
                                        }
                                        None => {
                                            errors.push(Error {
                                                filepath: file.path.clone(),
                                                error_type: ErrorType::LinkToUnknownTable {
                                                    link_name: link.link_name.clone(),
                                                    unknown_table: link.foreign.table.clone(),
                                                },
                                                locations: vec![Location {
                                                    contexts: to_range(&start, &end),
                                                    primary: to_range(
                                                        &link.start_name,
                                                        &link.end_name,
                                                    ),
                                                }],
                                            });
                                        }
                                    }

                                    // Check that the local ids exist
                                    for local_id in &link.local_ids {
                                        if !fields.iter().any(|f| ast::has_fieldname(f, local_id)) {
                                            errors.push(Error {
                                                filepath: file.path.clone(),
                                                error_type: ErrorType::LinkToUnknownField {
                                                    link_name: link.link_name.clone(),
                                                    unknown_local_field: local_id.clone(),
                                                },
                                                locations: vec![Location {
                                                    contexts: to_range(&start, &end),
                                                    primary: to_range(
                                                        &link.start_name,
                                                        &link.end_name,
                                                    ),
                                                }],
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }

                        if tablenames.len() > 1 {
                            errors.push(Error {
                                filepath: file.path.clone(),
                                error_type: ErrorType::MultipleTableNames {
                                    record: name.clone(),
                                },
                                locations: vec![Location {
                                    contexts: to_range(&start, &end),
                                    primary: tablenames,
                                }],
                            });
                        }

                        // Validate permissions according to new rules
                        if permission_directives.is_empty() {
                            errors.push(Error {
                                filepath: file.path.clone(),
                                error_type: ErrorType::MissingPermissions {
                                    record: name.clone(),
                                },
                                locations: vec![Location {
                                    contexts: to_range(&start, &end),
                                    primary: to_range(&start_name, &end_name),
                                }],
                            });
                        } else {
                            // Check for star permission or @public
                            let mut has_star = false;
                            let mut has_public = false;
                            let mut fine_grained_permissions: Vec<(
                                Vec<ast::QueryOperation>,
                                Range,
                            )> = Vec::new();

                            for (perm, perm_range) in &permission_directives {
                                match perm {
                                    ast::PermissionDetails::Star(_) => {
                                        has_star = true;
                                    }
                                    ast::PermissionDetails::Public => {
                                        has_public = true;
                                    }
                                    ast::PermissionDetails::OnOperation(ops) => {
                                        for op in ops {
                                            fine_grained_permissions
                                                .push((op.operations.clone(), perm_range.clone()));
                                        }
                                    }
                                }
                            }

                            // Rule 1: Star permission can't coexist with other permissions
                            if has_star && permission_directives.len() > 1 {
                                errors.push(Error {
                                    filepath: file.path.clone(),
                                    error_type: ErrorType::MultiplePermissions {
                                        record: name.clone(),
                                    },
                                    locations: vec![Location {
                                        contexts: to_range(&start, &end),
                                        primary: permission_directives
                                            .iter()
                                            .map(|(_, r)| r.clone())
                                            .collect(),
                                    }],
                                });
                            }

                            // Rule 2: @public can't coexist with other permissions
                            if has_public && permission_directives.len() > 1 {
                                errors.push(Error {
                                    filepath: file.path.clone(),
                                    error_type: ErrorType::MultiplePermissions {
                                        record: name.clone(),
                                    },
                                    locations: vec![Location {
                                        contexts: to_range(&start, &end),
                                        primary: permission_directives
                                            .iter()
                                            .map(|(_, r)| r.clone())
                                            .collect(),
                                    }],
                                });
                            }

                            // Rule 3: Multiple fine-grained permissions can't overlap operations
                            if !has_star && !has_public && fine_grained_permissions.len() > 1 {
                                let mut operation_coverage: std::collections::HashMap<
                                    ast::QueryOperation,
                                    Vec<Range>,
                                > = std::collections::HashMap::new();

                                for (ops, perm_range) in &fine_grained_permissions {
                                    for op in ops {
                                        operation_coverage
                                            .entry(op.clone())
                                            .or_insert_with(Vec::new)
                                            .push(perm_range.clone());
                                    }
                                }

                                // Check for overlaps
                                for (_op, ranges) in operation_coverage {
                                    if ranges.len() > 1 {
                                        errors.push(Error {
                                            filepath: file.path.clone(),
                                            error_type: ErrorType::MultiplePermissions {
                                                record: name.clone(),
                                            },
                                            locations: vec![Location {
                                                contexts: to_range(&start, &end),
                                                primary: ranges,
                                            }],
                                        });
                                        break; // Only report one error per record
                                    }
                                }
                            }
                        }

                        if !has_primary_id {
                            errors.push(Error {
                                filepath: file.path.clone(),
                                error_type: ErrorType::NoPrimaryKey {
                                    record: name.clone(),
                                },
                                locations: vec![Location {
                                    contexts: vec![],
                                    primary: to_range(&start_name, &end_name),
                                }],
                            });
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    if errors.len() > 0 {
        return Err(errors);
    } else {
        resolve_foreign_key_serialization_types(&mut context);
        return Ok(context);
    }
}

fn resolve_foreign_key_serialization_types(context: &mut Context) {
    let foreign_key_types: HashMap<String, HashMap<String, ast::ConcreteSerializationType>> =
        context
            .tables
            .iter()
            .map(|(table_name, table)| {
                let columns = table
                    .record
                    .fields
                    .iter()
                    .filter_map(|field| match field {
                        ast::Field::Column(column) => column
                            .type_
                            .to_serialization_type()
                            .into_concrete()
                            .map(|type_| (column.name.clone(), type_)),
                        _ => None,
                    })
                    .collect();
                (table_name.clone(), columns)
            })
            .collect();

    fn resolve_type(
        type_: &mut ast::ColumnType,
        foreign_key_types: &HashMap<String, HashMap<String, ast::ConcreteSerializationType>>,
    ) {
        match type_ {
            ast::ColumnType::ForeignKey {
                table,
                field,
                serialization_type,
            } => {
                *serialization_type = foreign_key_types
                    .get(&crate::ext::string::decapitalize(table))
                    .and_then(|columns| columns.get(field))
                    .cloned();
            }
            ast::ColumnType::JsonTyped(inner)
            | ast::ColumnType::List(inner)
            | ast::ColumnType::Dict(inner)
            | ast::ColumnType::Nullable(inner) => resolve_type(inner, foreign_key_types),
            _ => {}
        }
    }

    for table in context.tables.values_mut() {
        for field in &mut table.record.fields {
            if let ast::Field::Column(column) = field {
                resolve_type(&mut column.type_, &foreign_key_types);
            }
        }
    }
}

// Check for duplicate variants
fn check_schema_definitions(context: &Context, database: &ast::Database, errors: &mut Vec<Error>) {
    let vars = context.variants.clone();
    for (_variant_name, (maybe_type_range, mut instances)) in vars {
        if instances.len() > 1 {
            let base_variant = instances.remove(0); // remove and use the first variant as the base
            let duplicates = instances; // the rest are duplicates
            let maybe_location = base_variant.range.clone();
            let mut primary_ranges: Vec<Range> = vec![];
            match maybe_location {
                None => (),
                Some(loc) => primary_ranges.push(loc),
            }

            for dup in &duplicates {
                match &dup.range {
                    None => (),
                    Some(range) => primary_ranges.push(range.clone()),
                }
            }

            let mut contexts = vec![];
            match maybe_type_range {
                None => (),
                Some(range) => contexts.push(range),
            }

            errors.push(Error {
                filepath: base_variant.filepath.clone(),
                error_type: ErrorType::DuplicateVariant {
                    base_variant,
                    duplicates,
                },
                locations: vec![Location {
                    contexts,
                    primary: primary_ranges,
                }],
            });
        }
    }

    let mut session_found = false;

    // Check definitions
    for schema in database.schemas.iter() {
        for file in schema.files.iter() {
            for definition in &file.definitions {
                match definition {
                    ast::Definition::Session(session) => {
                        if session_found {
                            errors.push(Error {
                                filepath: file.path.clone(),
                                error_type: ErrorType::MultipleSessionDefinitions,
                                locations: vec![Location {
                                    contexts: to_range(&session.start, &session.end),
                                    primary: vec![],
                                }],
                            });
                        } else {
                            session_found = true;
                        }
                    }
                    ast::Definition::Record {
                        name,
                        fields,
                        start,
                        end,
                        start_name: _start_name,
                        end_name: _end_name,
                    } => {
                        let mut field_names: HashMap<String, Option<Range>> = HashMap::new();
                        let fields = ast::with_timestamps_fields(fields);
                        for column in ast::collect_columns(&fields) {
                            validate_type_expr(
                                context,
                                &file.path,
                                to_range(start, end),
                                to_range(&column.start_typename, &column.end_typename),
                                &column.type_,
                                false,
                                &mut HashSet::new(),
                                errors,
                            );

                            for directive in &column.directives {
                                if matches!(
                                    directive,
                                    ast::ColumnDirective::CreatedAt
                                        | ast::ColumnDirective::UpdatedAt
                                ) && !matches!(column.type_, ast::ColumnType::DateTime)
                                {
                                    errors.push(Error {
                                        filepath: file.path.clone(),
                                        error_type: ErrorType::InvalidTypeUsage {
                                            message: format!(
                                                "{} must be a DateTime field",
                                                match directive {
                                                    ast::ColumnDirective::CreatedAt => "@createdAt",
                                                    ast::ColumnDirective::UpdatedAt => "@updatedAt",
                                                    _ => unreachable!(),
                                                }
                                            ),
                                        },
                                        locations: vec![Location {
                                            contexts: to_range(start, end),
                                            primary: to_range(&column.start, &column.end),
                                        }],
                                    });
                                }

                                if let ast::ColumnDirective::Default {
                                    value,
                                    start: default_start,
                                    end: default_end,
                                    ..
                                } = directive
                                {
                                    let primary =
                                        if default_start.is_some() && default_end.is_some() {
                                            to_range(default_start, default_end)
                                        } else {
                                            to_range(&column.start, &column.end)
                                        };

                                    if !is_supported_default_for_column(&column, value) {
                                        errors.push(Error {
                                            filepath: file.path.clone(),
                                            error_type: ErrorType::InvalidColumnDefault {
                                                field_name: column.name.clone(),
                                                field_type: column.type_.to_string(),
                                                default_value: default_value_display(value),
                                                expected: allowed_defaults_for_column_type(&column),
                                            },
                                            locations: vec![Location {
                                                contexts: to_range(start, end),
                                                primary,
                                            }],
                                        });
                                    }
                                }
                            }

                            // Duplicate field check
                            match field_names.get(&column.name) {
                                None => (),
                                Some(duplicate_maybe_range) => {
                                    let mut ranges: Vec<Range> = vec![];

                                    match duplicate_maybe_range {
                                        None => (),
                                        Some(new_range) => {
                                            ranges.push(new_range.clone());
                                        }
                                    }

                                    match to_single_range(&column.start_name, &column.end_name) {
                                        None => (),
                                        Some(new_range) => {
                                            ranges.push(new_range);
                                        }
                                    }

                                    errors.push(Error {
                                        filepath: file.path.clone(),
                                        error_type: ErrorType::DuplicateField {
                                            record: name.clone(),
                                            field: column.name.clone(),
                                        },
                                        locations: vec![Location {
                                            contexts: to_range(start, end),
                                            primary: ranges,
                                        }],
                                    });
                                }
                            }
                            field_names.insert(
                                column.name.clone(),
                                to_single_range(&column.start_name, &column.end_name),
                            );
                        }

                        // Validate permissions for this record
                        if let Some(record_details) =
                            context.tables.get(&crate::ext::string::decapitalize(name))
                        {
                            check_record_indexes(&record_details.record, &file.path, errors);
                            check_record_permissions(
                                context,
                                &record_details.record,
                                &file.path,
                                errors,
                            );
                        }
                    }
                    ast::Definition::Tagged {
                        name: _name,
                        variants,
                        start,
                        end,
                    } => {
                        for variant in variants {
                            if let Some(fields) = &variant.fields {
                                for field in ast::collect_columns(&fields) {
                                    let mut contexts: Vec<Range> = vec![];

                                    if let Some(new_range) = to_single_range(&start, &end) {
                                        contexts.push(new_range);
                                    }

                                    if let Some(new_range) =
                                        to_single_range(&variant.start, &variant.end)
                                    {
                                        contexts.push(new_range);
                                    }

                                    validate_type_expr(
                                        context,
                                        &file.path,
                                        contexts,
                                        to_range(&field.start_typename, &field.end_typename),
                                        &field.type_,
                                        true,
                                        &mut HashSet::new(),
                                        errors,
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

pub fn check_queries<'a>(
    query_list: &ast::QueryList,
    context: &Context,
) -> Result<HashMap<String, QueryInfo>, Vec<Error>> {
    let mut errors: Vec<Error> = Vec::new();
    let mut all_params: HashMap<String, QueryInfo> = HashMap::new();

    for query in &query_list.queries {
        match query {
            ast::QueryDef::Query(q) => {
                let query_info = check_query(context, &mut errors, &q);
                all_params.insert(q.name.clone(), query_info);
                continue;
            }
            _ => continue,
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(all_params)
}

#[derive(Debug)]
pub enum ParamInfo {
    Defined {
        // This is the variable name without the $ prefix
        raw_variable_name: String,
        defined_at: Option<Range>,
        type_: Option<String>,
        nullable: bool,
        used_by_top_level_field_alias: HashSet<String>,
        used: bool,
        type_inferred: bool,
        from_session: bool,
        session_name: Option<String>,
    },
    NotDefinedButUsed {
        used_at: Option<Range>,
        type_: Option<String>,
        nullable: bool,
    },
}

pub struct QueryInfo {
    pub variables: HashMap<String, ParamInfo>,
    pub primary_db: String,
    pub attached_dbs: HashSet<String>,
}

struct UsedNamespaces {
    primary: HashSet<String>,
    secondary: HashSet<String>,
}

struct QueryContext {
    // Used to track which params are used in which
    // toplevel query field.
    top_level_field_alias: String,
}

pub fn check_query(context: &Context, errors: &mut Vec<Error>, query: &ast::Query) -> QueryInfo {
    // We need to check
    // 1. The field exists on the record in the schema
    //    What type is the field (add to `QueryField`)
    //
    // 2. If params are defined
    //     2.a All defined params are used
    //     2.b Every used param is defined
    //
    let mut param_names: HashMap<String, ParamInfo> = HashMap::new();
    let mut used_namespaces: UsedNamespaces = UsedNamespaces {
        primary: HashSet::new(),
        secondary: HashSet::new(),
    };

    let mut query_context = QueryContext {
        top_level_field_alias: "".to_string(),
    };

    // Check that all param types are known
    // Gather them in param_names so we can calculate usage.
    for param_def in &query.args {
        // formatted name
        let param_name = format!("${}", param_def.name);
        match &param_def.type_ {
            None => {
                param_names.insert(
                    param_name,
                    ParamInfo::Defined {
                        raw_variable_name: param_def.name.clone(),
                        defined_at: to_single_range(&param_def.start_name, &param_def.end_name),
                        type_: None,
                        nullable: param_def.nullable,
                        used: false,
                        used_by_top_level_field_alias: HashSet::new(),
                        type_inferred: false,
                        from_session: false,
                        session_name: None,
                    },
                );
            }

            Some(type_) => {
                if !is_known_query_param_type(context, type_) {
                    errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::UnknownType {
                            found: type_.clone(),
                            known_types: known_types_for_error(context),
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: to_range(&param_def.start_type, &param_def.end_type),
                        }],
                    })
                }

                param_names.insert(
                    param_name,
                    ParamInfo::Defined {
                        raw_variable_name: param_def.name.clone(),
                        defined_at: to_single_range(&param_def.start_name, &param_def.end_name),
                        type_: Some(type_.clone()),
                        nullable: param_def.nullable,
                        used: false,
                        used_by_top_level_field_alias: HashSet::new(),
                        type_inferred: false,
                        from_session: false,
                        session_name: None,
                    },
                );
            }
        }
    }

    // Add session fields to the param_names collection
    match &context.session {
        None => (),
        Some(session) => {
            for field in &session.fields {
                match field {
                    ast::Field::Column(col) => {
                        param_names.insert(
                            ast::session_field_name(col),
                            ParamInfo::Defined {
                                raw_variable_name: format!("session_{}", col.name),
                                defined_at: None,
                                type_: Some(col.type_.to_string()),
                                nullable: col.nullable,
                                used: false,
                                used_by_top_level_field_alias: HashSet::new(),
                                type_inferred: false,
                                from_session: true,
                                session_name: Some(col.name.clone()),
                            },
                        );
                    }
                    _ => (),
                }
            }
        }
    }
    // Track aliased field names to detect duplicates
    let mut seen_fields = HashSet::new();

    // Verify that all fields exist in the schema
    for field in &query.fields {
        match field {
            ast::TopLevelQueryField::Field(query_field) => {
                let aliased_name = ast::get_aliased_name(query_field);

                if seen_fields.contains(&aliased_name) {
                    errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::DuplicateQueryField {
                            field: aliased_name.clone(),
                        },
                        locations: vec![Location {
                            contexts: to_range(&query.start, &query.end),
                            primary: to_range(
                                &query_field.start_fieldname,
                                &query_field.end_fieldname,
                            ),
                        }],
                    });
                } else {
                    seen_fields.insert(aliased_name.clone());
                }

                query_context.top_level_field_alias = aliased_name;
                match context.tables.get(&query_field.name) {
                    None => errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::UnknownTable {
                            found: query_field.name.clone(),
                            existing: context.tables.keys().cloned().collect(),
                        },
                        locations: vec![Location {
                            contexts: to_range(&query.start, &query.end),
                            primary: to_range(
                                &query_field.start_fieldname,
                                &query_field.end_fieldname,
                            ),
                        }],
                    }),
                    Some(table) => check_table_query(
                        context,
                        &query_context,
                        errors,
                        &query.operation,
                        None,
                        table,
                        query_field,
                        &mut param_names,
                        &mut used_namespaces,
                    ),
                }
            }
            ast::TopLevelQueryField::Lines { .. } => {}
            ast::TopLevelQueryField::Comment { .. } => {}
        }
    }

    // Check for unused or undefined parameters and add errors accordingly
    for (param_name, param_info) in param_names.iter() {
        match &param_info {
            ParamInfo::Defined {
                defined_at,
                type_: _type_,
                used,
                type_inferred,
                from_session,
                ..
            } => {
                if !from_session && *used == false {
                    errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::UnusedParam {
                            param: param_name.clone(),
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: defined_at.clone().into_iter().collect(),
                        }],
                    })
                } else if !from_session && *type_inferred {
                    errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::MissingType,
                        locations: vec![Location {
                            contexts: vec![],
                            primary: defined_at.clone().into_iter().collect(),
                        }],
                    })
                }
            }
            ParamInfo::NotDefinedButUsed { used_at, type_, .. } => errors.push(Error {
                filepath: context.current_filepath.clone(),
                error_type: ErrorType::UndefinedParam {
                    param: param_name.clone(),
                    type_: type_.clone(),
                },
                locations: vec![Location {
                    contexts: vec![],
                    primary: used_at.clone().into_iter().collect(),
                }],
            }),
        }
    }

    let primary_db = get_primary_db(&used_namespaces).to_string();
    let secondary_dbs = get_secondary_dbs(&used_namespaces, &primary_db);

    QueryInfo {
        variables: param_names,
        primary_db,
        attached_dbs: secondary_dbs,
    }
}

fn is_known_query_param_type(context: &Context, type_: &str) -> bool {
    if context.types.contains_key(type_) {
        return true;
    }

    match ast::ColumnType::from_str(type_) {
        ast::ColumnType::String
        | ast::ColumnType::Int
        | ast::ColumnType::Float
        | ast::ColumnType::Bool
        | ast::ColumnType::DateTime
        | ast::ColumnType::Date
        | ast::ColumnType::Json
        | ast::ColumnType::JsonTyped(_)
        | ast::ColumnType::List(_)
        | ast::ColumnType::Dict(_)
        | ast::ColumnType::Nullable(_)
        | ast::ColumnType::IdInt { .. }
        | ast::ColumnType::IdUuid { .. }
        | ast::ColumnType::ForeignKey { .. } => true,
        ast::ColumnType::Custom(name) => context.types.contains_key(&name),
    }
}

fn get_primary_db(namespaces: &UsedNamespaces) -> &str {
    if namespaces.primary.is_empty() && namespaces.secondary.is_empty() {
        ast::DEFAULT_SCHEMANAME
    } else if !namespaces.primary.is_empty() {
        // This gets an arbitrary value from primary if it has any elements
        namespaces
            .primary
            .iter()
            .min()
            .map(|s| s.as_str())
            .unwrap_or(ast::DEFAULT_SCHEMANAME)
    } else {
        // If primary is empty but secondary isn't, get an arbitrary value from secondary
        namespaces
            .secondary
            .iter()
            .min()
            .map(|s| s.as_str())
            .unwrap_or(ast::DEFAULT_SCHEMANAME)
    }
}

fn get_secondary_dbs(namespaces: &UsedNamespaces, primary_db: &str) -> HashSet<String> {
    let mut secondary = namespaces.secondary.clone();
    secondary.remove(primary_db);
    secondary
}

fn check_where_args(
    context: &Context,
    query_context: &QueryContext,
    start: &Option<ast::Location>,
    end: &Option<ast::Location>,
    table: &ast::RecordDetails,
    errors: &mut Vec<Error>,
    params: &mut HashMap<String, ParamInfo>,
    where_args: &ast::WhereArg,
) {
    let error_filepath = context.current_filepath.clone();
    match where_args {
        ast::WhereArg::Exists(path, _) => {
            errors.push(Error {
                filepath: error_filepath,
                error_type: ErrorType::InvalidRelationalPermission {
                    message: "exists expressions are only allowed in permissions".to_string(),
                },
                locations: vec![Location {
                    contexts: vec![],
                    primary: path
                        .first()
                        .map(|(_, range)| convert_range(range))
                        .into_iter()
                        .collect(),
                }],
            });
        }
        ast::WhereArg::And(ands) => {
            for and in ands {
                check_where_args(
                    context,
                    query_context,
                    start,
                    end,
                    table,
                    errors,
                    params,
                    and,
                );
            }
        }
        ast::WhereArg::Or(ors) => {
            for or in ors {
                check_where_args(
                    context,
                    query_context,
                    start,
                    end,
                    table,
                    errors,
                    params,
                    or,
                );
            }
        }
        ast::WhereArg::Column(
            is_session_var,
            field_name,
            _operator,
            query_val,
            field_name_range,
        ) => {
            // Check if this is a Session variable (e.g., Session.userId, Session.role)
            let mut is_known_field = false;
            let mut column_type: Option<String> = None;
            let mut is_nullable = false;

            if *is_session_var {
                // Validate against session fields
                if let Some(session) = &context.session {
                    for field in &session.fields {
                        match field {
                            ast::Field::Column(column) => {
                                if &column.name == field_name {
                                    is_known_field = true;
                                    column_type = Some(query_param_type_for_column(table, column));
                                    is_nullable = column.nullable;
                                    // Mark the session variable as used
                                    let session_param_name = ast::session_field_name(column);
                                    if let Some(param_info) = params.get_mut(&session_param_name) {
                                        match param_info {
                                            ParamInfo::Defined {
                                                ref mut used,
                                                ref mut used_by_top_level_field_alias,
                                                ..
                                            } => {
                                                *used = true;
                                                used_by_top_level_field_alias.insert(
                                                    query_context.top_level_field_alias.clone(),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                    break;
                                }
                            }
                            _ => (),
                        }
                    }
                }
                if !is_known_field {
                    let known_fields = match &context.session {
                        Some(session) => get_column_reference(&session.fields),
                        None => vec![],
                    };
                    errors.push(Error {
                        filepath: error_filepath.clone(),
                        error_type: ErrorType::UnknownField {
                            found: format!("Session.{}", field_name),
                            record_name: "Session".to_string(),
                            known_fields,
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(field_name_range)],
                        }],
                    })
                }
            } else {
                // Validate against table fields
                for col in &table.fields {
                    if is_known_field {
                        continue;
                    }
                    match col {
                        ast::Field::Column(column) => {
                            if &column.name == field_name {
                                is_known_field = true;
                                column_type = Some(query_param_type_for_column(table, column));
                                is_nullable = column.nullable;
                            }
                        }
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            if &link.link_name == field_name {
                                is_known_field = true;
                                errors.push(Error {
                                    filepath: error_filepath.clone(),
                                    error_type: ErrorType::WhereOnLinkIsntAllowed {
                                        link_name: field_name.clone(),
                                    },
                                    locations: vec![Location {
                                        contexts: vec![],
                                        primary: to_range(&link.start_name, &link.end_name),
                                    }],
                                })
                            }
                        }
                        _ => (),
                    }
                }
                if !is_known_field {
                    let known_fields = get_column_reference(&table.fields);
                    let error_found = field_name.clone();

                    errors.push(Error {
                        filepath: error_filepath.clone(),
                        error_type: ErrorType::UnknownField {
                            found: error_found,
                            record_name: table.name.clone(),
                            known_fields,
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(field_name_range)],
                        }],
                    })
                }
            }

            match column_type {
                None => mark_as_used(&query_context, query_val, params),
                Some(column_type_string) => {
                    check_value(
                        context,
                        &query_context,
                        query_val,
                        start,
                        end,
                        errors,
                        params,
                        &table.name,
                        &column_type_string,
                        is_nullable,
                    );
                }
            }

            // Check if the field exists
            // Get field type
        }
    }
}

/// Validate permissions WHERE clauses during schema checking.
/// This validates that fields exist and session variables are valid,
/// but doesn't track params (that's done during query checking).
fn check_permissions_where_args(
    context: &Context,
    table: &ast::RecordDetails,
    where_args: &ast::WhereArg,
    filepath: &String,
    errors: &mut Vec<Error>,
) {
    match where_args {
        ast::WhereArg::Exists(path, body) => {
            let Some((_, first_range)) = path.first() else {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::InvalidRelationalPermission {
                        message: "An exists path must contain at least one link.".to_string(),
                    },
                    locations: vec![],
                });
                return;
            };

            if where_contains_exists(body) {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::InvalidRelationalPermission {
                        message: "Nested exists expressions are not supported yet.".to_string(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: vec![convert_range(first_range)],
                    }],
                });
                return;
            }

            let mut current = table;
            let root_schema = context
                .tables
                .values()
                .find(|candidate| candidate.record.name == table.name)
                .map(|candidate| candidate.schema.as_str());
            for (segment, range) in path {
                let link = current.fields.iter().find_map(|field| match field {
                    ast::Field::FieldDirective(ast::FieldDirective::Link(link))
                        if link.link_name == *segment =>
                    {
                        Some(link)
                    }
                    _ => None,
                });
                let Some(link) = link else {
                    let kind = if current
                        .fields
                        .iter()
                        .any(|field| ast::has_fieldname(field, segment))
                    {
                        "is a scalar field, not a materialized link"
                    } else {
                        "is not a declared materialized link"
                    };
                    errors.push(Error {
                        filepath: filepath.clone(),
                        error_type: ErrorType::InvalidRelationalPermission {
                            message: format!("{}.{} {}.", current.name, segment, kind),
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(range)],
                        }],
                    });
                    return;
                };
                if link.local_ids.is_empty()
                    || link.foreign.fields.is_empty()
                    || link.local_ids.len() != link.foreign.fields.len()
                {
                    errors.push(Error {
                        filepath: filepath.clone(),
                        error_type: ErrorType::InvalidRelationalPermission {
                            message: format!("Link {} has an invalid column mapping.", segment),
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(range)],
                        }],
                    });
                    return;
                }
                let Some(linked) = get_linked_table(context, link) else {
                    return;
                };
                if root_schema.map_or(false, |schema| schema != linked.schema) {
                    errors.push(Error {
                        filepath: filepath.clone(),
                        error_type: ErrorType::InvalidRelationalPermission {
                            message: "Cross-namespace exists paths are not supported.".to_string(),
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(range)],
                        }],
                    });
                    return;
                }
                current = &linked.record;
            }
            check_permissions_where_args(context, current, body, filepath, errors);
        }
        ast::WhereArg::And(ands) => {
            for and in ands {
                check_permissions_where_args(context, table, and, filepath, errors);
            }
        }
        ast::WhereArg::Or(ors) => {
            for or in ors {
                check_permissions_where_args(context, table, or, filepath, errors);
            }
        }
        ast::WhereArg::Column(
            is_session_var,
            field_name,
            _operator,
            _query_val,
            field_name_range,
        ) => {
            if *is_session_var {
                // Validate session variable exists
                if let Some(session) = &context.session {
                    let mut is_known_field = false;
                    for field in &session.fields {
                        match field {
                            ast::Field::Column(column) => {
                                if &column.name == field_name {
                                    is_known_field = true;
                                    break;
                                }
                            }
                            _ => (),
                        }
                    }
                    if !is_known_field {
                        let known_fields = get_column_reference(&session.fields);
                        errors.push(Error {
                            filepath: filepath.clone(),
                            error_type: ErrorType::UnknownField {
                                found: format!("Session.{}", field_name),
                                record_name: "Session".to_string(),
                                known_fields,
                            },
                            locations: vec![Location {
                                contexts: vec![],
                                primary: vec![convert_range(field_name_range)],
                            }],
                        });
                    }
                } else {
                    errors.push(Error {
                        filepath: filepath.clone(),
                        error_type: ErrorType::UnknownField {
                            found: format!("Session.{}", field_name),
                            record_name: "Session".to_string(),
                            known_fields: vec![],
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(field_name_range)],
                        }],
                    });
                }
            } else {
                // Validate table field exists
                let mut is_known_field = false;
                for col in &table.fields {
                    match col {
                        ast::Field::Column(column) => {
                            if &column.name == field_name {
                                is_known_field = true;
                                break;
                            }
                        }
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            if &link.link_name == field_name {
                                is_known_field = true;
                                errors.push(Error {
                                    filepath: filepath.clone(),
                                    error_type: ErrorType::WhereOnLinkIsntAllowed {
                                        link_name: field_name.clone(),
                                    },
                                    locations: vec![Location {
                                        contexts: vec![],
                                        primary: to_range(&link.start_name, &link.end_name),
                                    }],
                                });
                                break;
                            }
                        }
                        _ => (),
                    }
                }
                if !is_known_field {
                    let known_fields = get_column_reference(&table.fields);
                    errors.push(Error {
                        filepath: filepath.clone(),
                        error_type: ErrorType::UnknownField {
                            found: field_name.clone(),
                            record_name: table.name.clone(),
                            known_fields,
                        },
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(field_name_range)],
                        }],
                    });
                }
            }
        }
    }
}

fn check_record_indexes(record: &ast::RecordDetails, filepath: &String, errors: &mut Vec<Error>) {
    let known_fields: Vec<String> = ast::collect_columns(&record.fields)
        .iter()
        .map(|c| c.name.clone())
        .collect();

    for field in &record.fields {
        let (directive_name, details) = match field {
            ast::Field::FieldDirective(ast::FieldDirective::Index(details)) => ("@index", details),
            ast::Field::FieldDirective(ast::FieldDirective::Unique(details)) => {
                ("@unique", details)
            }
            _ => continue,
        };

        let mut seen = HashSet::new();
        for indexed in &details.columns {
            if !known_fields.contains(&indexed.name) {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::InvalidRecordIndexField {
                        record: record.name.clone(),
                        directive: directive_name.to_string(),
                        field: indexed.name.clone(),
                        known_fields: known_fields.clone(),
                    },
                    locations: vec![Location {
                        contexts: to_range(&record.start, &record.end),
                        primary: vec![],
                    }],
                });
            }

            if !seen.insert(indexed.name.clone()) {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::DuplicateRecordIndexField {
                        record: record.name.clone(),
                        directive: directive_name.to_string(),
                        field: indexed.name.clone(),
                    },
                    locations: vec![Location {
                        contexts: to_range(&record.start, &record.end),
                        primary: vec![],
                    }],
                });
            }
        }

        if let Some(where_) = &details.where_ {
            check_record_index_where(
                where_,
                &record.name,
                directive_name,
                &known_fields,
                filepath,
                errors,
            );
        }
    }
}

fn check_record_index_where(
    where_arg: &ast::WhereArg,
    record_name: &str,
    directive_name: &str,
    known_fields: &Vec<String>,
    filepath: &String,
    errors: &mut Vec<Error>,
) {
    match where_arg {
        ast::WhereArg::Exists(path, _) => errors.push(Error {
            filepath: filepath.clone(),
            error_type: ErrorType::InvalidRelationalPermission {
                message: "exists expressions are not allowed in partial-index predicates"
                    .to_string(),
            },
            locations: vec![Location {
                contexts: vec![],
                primary: path
                    .first()
                    .map(|(_, range)| convert_range(range))
                    .into_iter()
                    .collect(),
            }],
        }),
        ast::WhereArg::And(items) | ast::WhereArg::Or(items) => {
            for item in items {
                check_record_index_where(
                    item,
                    record_name,
                    directive_name,
                    known_fields,
                    filepath,
                    errors,
                );
            }
        }
        ast::WhereArg::Column(is_session_var, field_name, _, value, _) => {
            if *is_session_var || !known_fields.contains(field_name) {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::InvalidRecordIndexField {
                        record: record_name.to_string(),
                        directive: directive_name.to_string(),
                        field: field_name.clone(),
                        known_fields: known_fields.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: vec![],
                    }],
                });
            }

            if matches!(value, ast::QueryValue::Variable(_) | ast::QueryValue::Fn(_)) {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::InvalidRecordIndexField {
                        record: record_name.to_string(),
                        directive: directive_name.to_string(),
                        field: field_name.clone(),
                        known_fields: known_fields.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: vec![],
                    }],
                });
            }
        }
    }
}

fn where_contains_exists(where_arg: &ast::WhereArg) -> bool {
    match where_arg {
        ast::WhereArg::Exists(..) => true,
        ast::WhereArg::And(items) | ast::WhereArg::Or(items) => {
            items.iter().any(where_contains_exists)
        }
        ast::WhereArg::Column(..) => false,
    }
}

fn first_exists_range(where_arg: &ast::WhereArg) -> Option<&ast::Range> {
    match where_arg {
        ast::WhereArg::Exists(path, _) => path.first().map(|(_, range)| range),
        ast::WhereArg::And(items) | ast::WhereArg::Or(items) => {
            items.iter().find_map(first_exists_range)
        }
        ast::WhereArg::Column(..) => None,
    }
}

fn collect_exists_first_links<'a>(where_arg: &'a ast::WhereArg, links: &mut Vec<&'a str>) {
    match where_arg {
        ast::WhereArg::Exists(path, _) => {
            if let Some((link, _)) = path.first() {
                links.push(link);
            }
        }
        ast::WhereArg::And(items) | ast::WhereArg::Or(items) => {
            for item in items {
                collect_exists_first_links(item, links);
            }
        }
        ast::WhereArg::Column(..) => {}
    }
}

/// Validate all permissions for a record during schema checking.
fn check_record_permissions(
    context: &Context,
    record: &ast::RecordDetails,
    filepath: &String,
    errors: &mut Vec<Error>,
) {
    let synced = context
        .tables
        .values()
        .find(|table| table.record.name == record.name && table.record == *record)
        .and_then(|table| context.namespace_sync_modes.get(&table.schema))
        .copied()
        .unwrap_or(ast::SyncMode::Synced)
        == ast::SyncMode::Synced;

    if synced {
        for field in &record.fields {
            let expressions: Vec<&ast::WhereArg> = match field {
                ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                    ast::PermissionDetails::Star(where_),
                )) => vec![where_],
                ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                    ast::PermissionDetails::OnOperation(operations),
                )) => operations
                    .iter()
                    .filter(|permission| {
                        permission.operations.contains(&ast::QueryOperation::Query)
                    })
                    .map(|permission| &permission.where_)
                    .collect(),
                _ => vec![],
            };
            for expression in expressions {
                if let Some(range) = first_exists_range(expression) {
                    errors.push(Error {
                        filepath: filepath.clone(),
                        error_type: ErrorType::SyncedRelationalQueryPermission,
                        locations: vec![Location {
                            contexts: vec![],
                            primary: vec![convert_range(range)],
                        }],
                    });
                }
            }
        }
    }

    for field in &record.fields {
        let expressions: Vec<&ast::WhereArg> = match field {
            ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                ast::PermissionDetails::Star(where_),
            )) => vec![where_],
            ast::Field::FieldDirective(ast::FieldDirective::Permissions(
                ast::PermissionDetails::OnOperation(operations),
            )) => operations
                .iter()
                .filter(|permission| permission.operations.contains(&ast::QueryOperation::Insert))
                .map(|permission| &permission.where_)
                .collect(),
            _ => vec![],
        };
        for expression in expressions {
            if let Some(range) = first_exists_range(expression) {
                errors.push(Error {
                    filepath: filepath.clone(),
                    error_type: ErrorType::InvalidRelationalPermission {
                        message: "Relational insert permissions are not supported until insert policies are enforced against the proposed row.".to_string(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: vec![convert_range(range)],
                    }],
                });
            }
        }
    }

    // Collect unique permissions to avoid checking the same permission expression multiple times.
    // When a single @allow(*) directive is used (PermissionDetails::Star), it applies to all
    // operations, so we'd otherwise check and report the same errors 4 times (once per operation).
    let mut checked_permissions: Vec<ast::WhereArg> = Vec::new();

    // Check permissions for all operations
    for operation in &[
        ast::QueryOperation::Query,
        ast::QueryOperation::Insert,
        ast::QueryOperation::Update,
        ast::QueryOperation::Delete,
    ] {
        if let Some(perms) = ast::get_permissions(record, operation) {
            // Only check this permission if we haven't already checked an equivalent one
            if !checked_permissions.contains(&perms) {
                check_permissions_where_args(context, record, &perms, filepath, errors);
                checked_permissions.push(perms);
            }
        }
    }
}

/// Mark session variables found in WHERE clauses as used.
///
/// This is necessary because session variables that appear in permissions WHERE clauses
/// (e.g., `delete { authorId = Session.userId }`) are automatically included in the
/// generated SQL, but they aren't explicitly referenced in the query itself.
///
/// If session variables aren't marked as used:
/// 1. They won't be added to `session_param_names` during query execution
/// 2. They won't be replaced in the SQL (e.g., `$session_userId` remains as-is instead of becoming `?`)
/// 3. The SQL will fail to execute because the parameter placeholder isn't bound
///
/// This function handles both cases:
/// - When the column is a session variable: `Session.userId = ...`
/// - When the value is a session variable: `authorId = Session.userId`
fn mark_session_vars_in_where_as_used(
    query_context: &QueryContext,
    context: &Context,
    where_args: &ast::WhereArg,
    params: &mut HashMap<String, ParamInfo>,
) {
    match where_args {
        ast::WhereArg::Exists(_, body) => {
            mark_session_vars_in_where_as_used(query_context, context, body, params)
        }
        ast::WhereArg::And(ands) => {
            for and in ands {
                mark_session_vars_in_where_as_used(query_context, context, and, params);
            }
        }
        ast::WhereArg::Or(ors) => {
            for or in ors {
                mark_session_vars_in_where_as_used(query_context, context, or, params);
            }
        }
        ast::WhereArg::Column(
            is_session_var,
            field_name,
            _operator,
            query_val,
            _field_name_range,
        ) => {
            // Check if the column itself is a session variable (e.g., Session.userId = ...)
            if *is_session_var {
                if let Some(session) = &context.session {
                    for field in &session.fields {
                        match field {
                            ast::Field::Column(column) => {
                                if &column.name == field_name {
                                    let session_param_name = ast::session_field_name(column);
                                    if let Some(param_info) = params.get_mut(&session_param_name) {
                                        match param_info {
                                            ParamInfo::Defined {
                                                ref mut used,
                                                ref mut used_by_top_level_field_alias,
                                                ..
                                            } => {
                                                *used = true;
                                                used_by_top_level_field_alias.insert(
                                                    query_context.top_level_field_alias.clone(),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                    break;
                                }
                            }
                            _ => (),
                        }
                    }
                }
            }
            // Also check if the value is a session variable (e.g., authorId = Session.userId)
            if let ast::QueryValue::Variable((_, var_details)) = query_val {
                if let Some(session_field) = &var_details.session_field {
                    if let Some(session) = &context.session {
                        for field in &session.fields {
                            match field {
                                ast::Field::Column(column) => {
                                    if &column.name == session_field {
                                        let session_param_name = ast::session_field_name(column);
                                        if let Some(param_info) =
                                            params.get_mut(&session_param_name)
                                        {
                                            match param_info {
                                                ParamInfo::Defined {
                                                    ref mut used,
                                                    ref mut used_by_top_level_field_alias,
                                                    ..
                                                } => {
                                                    *used = true;
                                                    used_by_top_level_field_alias.insert(
                                                        query_context.top_level_field_alias.clone(),
                                                    );
                                                }
                                                _ => {}
                                            }
                                        }
                                        break;
                                    }
                                }
                                _ => (),
                            }
                        }
                    }
                }
            }
        }
    }
}

fn mark_as_used(
    query_context: &QueryContext,
    value: &ast::QueryValue,
    params: &mut HashMap<String, ParamInfo>,
) {
    match value {
        ast::QueryValue::Variable((var_range, var)) => {
            let var_name = ast::to_pyre_variable_name(var);
            match params.get_mut(&var_name) {
                None => {
                    params.insert(
                        var_name,
                        ParamInfo::NotDefinedButUsed {
                            used_at: Some(convert_range(var_range)),
                            type_: None,
                            nullable: false, // mark_as_used doesn't have nullable context
                        },
                    );
                }
                Some(param_info) => {
                    match param_info {
                        ParamInfo::Defined {
                            ref mut used,
                            ref mut used_by_top_level_field_alias,
                            ..
                        } => {
                            // mark as used
                            *used = true;
                            used_by_top_level_field_alias
                                .insert(query_context.top_level_field_alias.clone());
                        }
                        ParamInfo::NotDefinedButUsed { .. } => (),
                    };
                }
            }
        }
        _ => {}
    }
}

fn check_value(
    context: &Context,
    query_context: &QueryContext,
    value: &ast::QueryValue,
    start: &Option<ast::Location>,
    end: &Option<ast::Location>,
    errors: &mut Vec<Error>,
    params: &mut HashMap<String, ParamInfo>,
    table_name: &str,
    table_type_string: &str,
    is_nullable: bool,
) {
    if let ast::ColumnType::JsonTyped(inner) = ast::ColumnType::from_str(table_type_string) {
        match value {
            ast::QueryValue::Variable(_) | ast::QueryValue::Null(_) => {}
            _ => {
                check_value(
                    context,
                    query_context,
                    value,
                    start,
                    end,
                    errors,
                    params,
                    table_name,
                    &inner.to_string(),
                    false,
                );
                return;
            }
        }
    }

    if table_type_string == "Json" {
        if let ast::QueryValue::Variable(_) = value {
            mark_as_used(query_context, value, params);
            return;
        }

        let found = match value {
            ast::QueryValue::String(_) => "String",
            ast::QueryValue::Int(_) => "Int",
            ast::QueryValue::Float(_) => "Float",
            ast::QueryValue::Bool(_) => "Bool",
            ast::QueryValue::Null(_) => "Null",
            ast::QueryValue::Fn(_) => "Function",
            ast::QueryValue::LiteralTypeValue(_) => "Literal",
            ast::QueryValue::Variable(_) => "Variable",
        };

        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::LiteralTypeMismatch {
                expecting_type: table_type_string.to_string(),
                found: found.to_string(),
            },
            locations: vec![Location {
                contexts: vec![],
                primary: to_range(start, end),
            }],
        });
        return;
    }

    match value {
        ast::QueryValue::String((range, _value)) => {
            if table_type_string != "String" {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::LiteralTypeMismatch {
                        expecting_type: table_type_string.to_string(),
                        found: "String".to_string(),
                    },
                    locations: vec![Location {
                        contexts: vec![], // to_range(&start, &end),
                        primary: vec![convert_range(range)],
                    }],
                })
            }
        }
        ast::QueryValue::Int((range, _)) => {
            if table_type_string != "Int" {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::LiteralTypeMismatch {
                        expecting_type: table_type_string.to_string(),
                        found: "Int".to_string(),
                    },
                    locations: vec![Location {
                        contexts: vec![], // to_range(&start, &end),
                        primary: vec![convert_range(range)],
                    }],
                })
            }
        }
        ast::QueryValue::Float((range, _)) => {
            if table_type_string != "Float" {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::LiteralTypeMismatch {
                        expecting_type: table_type_string.to_string(),
                        found: "Float".to_string(),
                    },
                    locations: vec![Location {
                        contexts: vec![], // to_range(&start, &end),
                        primary: vec![convert_range(range)],
                    }],
                })
            }
        }
        ast::QueryValue::Bool((range, _)) => {
            if table_type_string != "Bool" {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::LiteralTypeMismatch {
                        expecting_type: table_type_string.to_string(),
                        found: "Bool".to_string(),
                    },
                    locations: vec![Location {
                        contexts: vec![], // to_range(&start, &end),
                        primary: vec![convert_range(range)],
                    }],
                })
            }
        }
        ast::QueryValue::Null(_) => {}
        ast::QueryValue::Fn(func) => {
            let found = context.funcs.get(&func.name);
            match found {
                None => errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::UnknownFunction {
                        found: func.name.clone(),
                        known_functions: context.funcs.keys().cloned().collect(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: vec![convert_range(&func.location_fn_name)],
                    }],
                }),
                Some(func_definition) => {
                    if func_definition.arg_types.len() != func.args.len() {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::UnknownFunction {
                                found: func.name.clone(),
                                known_functions: context.funcs.keys().cloned().collect(),
                            },
                            locations: vec![Location {
                                contexts: vec![],
                                primary: vec![convert_range(&func.location_fn_name)],
                            }],
                        });
                    } else {
                        for (arg, arg_type) in
                            func.args.iter().zip(func_definition.arg_types.iter())
                        {
                            check_value(
                                context,
                                &query_context,
                                arg,
                                start,
                                end,
                                errors,
                                params,
                                table_name,
                                arg_type,
                                false,
                            );
                        }
                    }
                }
            }
        }
        ast::QueryValue::Variable((var_range, var)) => {
            let var_name = ast::to_pyre_variable_name(var);

            match params.get_mut(&var_name) {
                None => {
                    params.insert(
                        var_name,
                        ParamInfo::NotDefinedButUsed {
                            used_at: Some(convert_range(var_range)),
                            type_: Some(table_type_string.to_string()),
                            nullable: is_nullable,
                        },
                    );
                }
                Some(param_info) => {
                    match param_info {
                        ParamInfo::Defined {
                            defined_at,
                            ref mut type_,
                            nullable: param_nullable,
                            ref mut used,
                            ref mut type_inferred,
                            ref mut used_by_top_level_field_alias,
                            ..
                        } => {
                            // mark as used
                            *used = true;
                            used_by_top_level_field_alias
                                .insert(query_context.top_level_field_alias.clone());

                            match &type_ {
                                None => {
                                    // We can set the type, but also mark it as inferred
                                    // If it's inferred, it will error if exec'ed, but succeed if formatted
                                    *type_ = Some(table_type_string.to_string());
                                    *type_inferred = true;
                                }
                                Some(type_name) => {
                                    // Check type compatibility:
                                    // 1. Base types must match
                                    if !are_query_types_compatible(type_name, table_type_string) {
                                        errors.push(Error {
                                            filepath: context.current_filepath.clone(),
                                            error_type: ErrorType::TypeMismatch {
                                                table: table_name.to_string(),
                                                column_defined_as: table_type_string.to_string(),
                                                variable_name: var.name.clone(),
                                                variable_defined_as: type_name.clone(),
                                            },
                                            locations: vec![
                                                Location {
                                                    contexts: vec![],
                                                    primary: defined_at
                                                        .as_ref()
                                                        .map_or_else(Vec::new, |range| {
                                                            vec![range.clone()]
                                                        }),
                                                },
                                                Location {
                                                    contexts: vec![], // to_range(&start, &end),
                                                    primary: vec![convert_range(var_range)],
                                                },
                                            ],
                                        })
                                    }
                                    // 2. Check nullability: nullable params cannot be used with non-nullable columns
                                    //    Null is not a valid value for non-nullable types, regardless of context
                                    //    (WHERE clauses, SET operations, etc.)
                                    if !is_nullable && *param_nullable {
                                        errors.push(Error {
                                            filepath: context.current_filepath.clone(),
                                            error_type: ErrorType::TypeMismatch {
                                                table: table_name.to_string(),
                                                column_defined_as: format!(
                                                    "{} (non-nullable)",
                                                    table_type_string
                                                ),
                                                variable_name: var.name.clone(),
                                                variable_defined_as: format!(
                                                    "{} (nullable)",
                                                    type_name
                                                ),
                                            },
                                            locations: vec![
                                                Location {
                                                    contexts: vec![],
                                                    primary: defined_at
                                                        .as_ref()
                                                        .map_or_else(Vec::new, |range| {
                                                            vec![range.clone()]
                                                        }),
                                                },
                                                Location {
                                                    contexts: vec![],
                                                    primary: vec![convert_range(var_range)],
                                                },
                                            ],
                                        })
                                    }
                                }
                            }
                        }
                        ParamInfo::NotDefinedButUsed { .. } => (),
                    };
                }
            }
        }
        ast::QueryValue::LiteralTypeValue((range, details)) => {
            match context.types.get(table_type_string) {
                Some((_type_info, type_)) => {
                    match type_ {
                        Type::OneOf { variants } => {
                            let mut found_variant = false;
                            for variant in variants {
                                if variant.name == details.name {
                                    found_variant = true;
                                    break;
                                }
                            }
                            if !found_variant {
                                errors.push(Error {
                                    filepath: context.current_filepath.clone(),
                                    error_type: ErrorType::LiteralTypeMismatchVariant {
                                        expecting_type: table_type_string.to_string(),
                                        found: details.name.to_string(),
                                        variants: variants.iter().map(|v| v.name.clone()).collect(),
                                    },
                                    locations: vec![Location {
                                        contexts: vec![], // to_range(&start, &end),
                                        primary: vec![convert_range(range)],
                                    }],
                                })
                            }
                        }
                        _ => {
                            errors.push(Error {
                                filepath: context.current_filepath.clone(),
                                error_type: ErrorType::LiteralTypeMismatch {
                                    expecting_type: table_type_string.to_string(),
                                    found: details.name.to_string(),
                                },
                                locations: vec![Location {
                                    contexts: vec![], // to_range(&start, &end),
                                    primary: vec![convert_range(range)],
                                }],
                            })
                        }
                    }
                }
                None => {
                    errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::LiteralTypeMismatch {
                            expecting_type: table_type_string.to_string(),
                            found: details.name.to_string(),
                        },
                        locations: vec![Location {
                            contexts: vec![], // to_range(&start, &end),
                            primary: vec![convert_range(range)],
                        }],
                    })
                }
            }
        }
    }
}

fn are_query_types_compatible(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    match (
        ast::ColumnType::from_str(left),
        ast::ColumnType::from_str(right),
    ) {
        (
            ast::ColumnType::IdInt { table: left_table },
            ast::ColumnType::ForeignKey {
                table: right_table,
                field,
                ..
            },
        ) if field == "id" => left_table == right_table,
        (
            ast::ColumnType::ForeignKey {
                table: left_table,
                field,
                ..
            },
            ast::ColumnType::IdInt { table: right_table },
        ) if field == "id" => left_table == right_table,
        (
            ast::ColumnType::IdUuid { table: left_table },
            ast::ColumnType::ForeignKey {
                table: right_table,
                field,
                ..
            },
        ) if field == "id" => left_table == right_table,
        (
            ast::ColumnType::ForeignKey {
                table: left_table,
                field,
                ..
            },
            ast::ColumnType::IdUuid { table: right_table },
        ) if field == "id" => left_table == right_table,
        _ => false,
    }
}

/// Primary schemas/namespaces are the only ones that can accept writes.
/// So, if any field is set, it needs to be on a primary schema.
/// If the operation is a delete, the namespace is always primary
/// Otherwise, it's secondary
fn add_schema(
    context: &Context,
    table: &Table,
    operation: &ast::QueryOperation,
    query: &ast::QueryField,
    field: &ast::QueryField,
    errors: &mut Vec<Error>,
    used_schemas: &mut UsedNamespaces,
) {
    match operation {
        ast::QueryOperation::Delete => {
            insert_primary_schema(
                context,
                table,
                operation,
                query,
                field,
                errors,
                used_schemas,
            );
        }
        ast::QueryOperation::Update | ast::QueryOperation::Insert => {
            if field.set == None {
                used_schemas.secondary.insert(table.schema.to_string());
            } else {
                insert_primary_schema(
                    context,
                    table,
                    operation,
                    query,
                    field,
                    errors,
                    used_schemas,
                );
            }
        }
        ast::QueryOperation::Query => {
            used_schemas.secondary.insert(table.schema.to_string());
        }
    }
}

fn insert_primary_schema(
    context: &Context,
    table: &Table,
    operation: &ast::QueryOperation,
    query: &ast::QueryField,
    field: &ast::QueryField,
    errors: &mut Vec<Error>,
    used_schemas: &mut UsedNamespaces,
) {
    let schema_name = table.schema.to_string();
    // If there's already a primary schema and it's not the current schema
    // Then it's a problem
    if !used_schemas.primary.is_empty() && !used_schemas.primary.contains(&schema_name) {
        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::MultipleSchemaWrites {
                field_table: table.record.name.clone(),
                field_schema: table.schema.to_string(),
                operation: operation.clone(),
                other_schemas: used_schemas.primary.clone().into_iter().collect(),
            },
            locations: vec![Location {
                contexts: to_range(&query.start, &query.end),
                primary: to_range(&field.start_fieldname, &field.end_fieldname),
            }],
        });
    }
    used_schemas.primary.insert(table.schema.to_string());
}

fn check_table_query(
    context: &Context,
    query_context: &QueryContext,
    errors: &mut Vec<Error>,
    operation: &ast::QueryOperation,
    through_link: Option<&ast::LinkDetails>,
    table: &Table,
    query: &ast::QueryField,
    params: &mut HashMap<String, ParamInfo>,
    used_namespaces: &mut UsedNamespaces,
) {
    // Mark session variables in permissions as used
    if let Some(perms) = ast::get_permissions(&table.record, operation) {
        mark_session_vars_in_where_as_used(query_context, context, &perms, params);

        if *operation == ast::QueryOperation::Update {
            let mut first_links = Vec::new();
            collect_exists_first_links(&perms, &mut first_links);
            let protected_columns: HashSet<&str> = first_links
                .iter()
                .filter_map(|link_name| {
                    table.record.fields.iter().find_map(|field| match field {
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link))
                            if link.link_name == *link_name =>
                        {
                            Some(&link.local_ids)
                        }
                        _ => None,
                    })
                })
                .flatten()
                .map(String::as_str)
                .collect();

            for field in &query.fields {
                if let ast::ArgField::Field(field) = field {
                    if field.set.is_some() && protected_columns.contains(field.name.as_str()) {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::InvalidRelationalPermission {
                                message: format!(
                                    "Cannot update relationship key {} while authorization depends on that relationship.",
                                    field.name
                                ),
                            },
                            locations: vec![Location {
                                contexts: vec![],
                                primary: to_range(&field.start_fieldname, &field.end_fieldname),
                            }],
                        });
                    }
                }
            }
        }
    }

    if query.fields.is_empty() {
        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::NoFieldsSelected,
            locations: vec![Location {
                contexts: vec![],
                primary: to_range(&query.start_fieldname, &query.end_fieldname),
            }],
        })
    }

    let mut queried_fields: HashMap<String, bool> = HashMap::new();

    let mut limits: Vec<Range> = vec![];
    let mut wheres: Vec<Range> = vec![];
    let mut has_nested_selected = false;

    // We've already checked that the top-level query field name is valid
    // we want to make sure that every field queried exists in `table` as a column
    for arg_field in &query.fields {
        match arg_field {
            ast::ArgField::Lines { .. } => (),
            ast::ArgField::QueryComment { .. } => (),
            ast::ArgField::Arg(arg) => {
                let arg_data = &arg.arg;
                match arg_data {
                    ast::Arg::Limit(limit_val) => {
                        match to_single_range(&arg.start, &arg.end) {
                            Some(range) => limits.push(range),
                            None => (),
                        }

                        check_value(
                            context,
                            &query_context,
                            &limit_val,
                            &arg.start,
                            &arg.end,
                            errors,
                            params,
                            &table.record.name,
                            "Int",
                            false,
                        );
                    }
                    ast::Arg::Where(where_args) => {
                        match to_single_range(&arg.start, &arg.end) {
                            Some(range) => wheres.push(range),
                            None => (),
                        }

                        check_where_args(
                            context,
                            &query_context,
                            &arg.start,
                            &arg.end,
                            &table.record,
                            errors,
                            params,
                            &where_args,
                        );
                    }
                    _ => (),
                }
            }
            ast::ArgField::Field(field) => {
                add_schema(
                    context,
                    &table,
                    operation,
                    query,
                    field,
                    errors,
                    used_namespaces,
                );

                let aliased_name = ast::get_aliased_name(field);

                if field.name != "*" {
                    if queried_fields.get(&aliased_name).is_some() {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::DuplicateQueryField {
                                field: aliased_name.clone(),
                            },
                            locations: vec![Location {
                                contexts: to_range(&query.start, &query.end),
                                primary: to_range(&field.start_fieldname, &field.end_fieldname),
                            }],
                        });
                    } else {
                        queried_fields.insert(aliased_name.clone(), field.set.is_some());
                    }
                }

                if field.name == "*" {
                    if field.alias.is_some() {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::InvalidWildcardSelection {
                                reason: error::InvalidWildcardReason::Aliased,
                            },
                            locations: vec![Location {
                                contexts: to_range(&query.start, &query.end),
                                primary: to_range(&field.start_fieldname, &field.end_fieldname),
                            }],
                        });
                    }

                    if field.set.is_some() {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::InvalidWildcardSelection {
                                reason: error::InvalidWildcardReason::Assigned,
                            },
                            locations: vec![Location {
                                contexts: to_range(&query.start, &query.end),
                                primary: to_range(&field.start_fieldname, &field.end_fieldname),
                            }],
                        });
                    }

                    if !field.fields.is_empty() {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::InvalidWildcardSelection {
                                reason: error::InvalidWildcardReason::HasNestedSelections,
                            },
                            locations: vec![Location {
                                contexts: to_range(&query.start, &query.end),
                                primary: to_range(&field.start_fieldname, &field.end_fieldname),
                            }],
                        });
                    }

                    if matches!(
                        operation,
                        ast::QueryOperation::Insert | ast::QueryOperation::Update
                    ) {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::InvalidWildcardSelection {
                                reason: error::InvalidWildcardReason::UsedInInsertOrUpdate,
                            },
                            locations: vec![Location {
                                contexts: to_range(&query.start, &query.end),
                                primary: to_range(&field.start_fieldname, &field.end_fieldname),
                            }],
                        });
                    }

                    continue;
                }

                let mut is_known_field = false;
                for col in &table.record.fields {
                    if is_known_field {
                        continue;
                    }
                    match col {
                        ast::Field::Column(column) => {
                            if column.name == field.name {
                                is_known_field = true;
                                check_field(
                                    context,
                                    &query_context,
                                    params,
                                    operation,
                                    errors,
                                    &table.record,
                                    column,
                                    field,
                                )
                            }
                        }
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            if link.link_name == field.name {
                                is_known_field = true;
                                has_nested_selected = true;
                                check_link(
                                    context,
                                    &query_context,
                                    operation,
                                    errors,
                                    &table.record,
                                    link,
                                    field,
                                    params,
                                    used_namespaces,
                                )
                            }
                            ()
                        }
                        _ => (),
                    }
                }
                if !is_known_field {
                    let known_fields = get_column_reference(&table.record.fields);
                    errors.push(Error {
                        filepath: context.current_filepath.clone(),
                        error_type: ErrorType::UnknownField {
                            found: field.name.clone(),
                            record_name: table.record.name.clone(),
                            known_fields,
                        },
                        locations: vec![Location {
                            contexts: to_range(&query.start, &query.end),
                            primary: to_range(&field.start_fieldname, &field.end_fieldname),
                        }],
                    })
                }
            }
        }
    }

    // Only one @limit allowed
    let limit_len = limits.len();
    if limit_len > 1 {
        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::MultipleLimits {
                query: query.name.clone(),
            },
            locations: vec![Location {
                contexts: to_range(&query.start, &query.end),
                primary: limits.clone(),
            }],
        });
    }

    if limit_len > 0 && has_nested_selected {
        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::LimitOffsetOnlyInFlatRecord,
            locations: vec![Location {
                contexts: to_range(&query.start, &query.end),
                primary: limits,
            }],
        });
    }

    if wheres.len() > 1 {
        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::MultipleWheres {
                query: query.name.clone(),
            },
            locations: vec![Location {
                contexts: to_range(&query.start, &query.end),
                primary: wheres,
            }],
        });
    }

    match operation {
        ast::QueryOperation::Insert => {
            let mut missing_fields = vec![];

            if let Some(link) = through_link {
                for field in &link.foreign.fields {
                    if queried_fields.contains_key(field) {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::InsertNestedValueAutomaticallySet {
                                field: field.clone(),
                            },
                            locations: vec![Location {
                                contexts: vec![],
                                primary: to_range(&query.start, &query.end),
                            }],
                        });
                        return;
                    }
                }
            }

            for col in ast::collect_columns(&table.record.fields) {
                if ast::is_integer_primary_key(&col)
                    || ast::has_default_value(&col)
                    || ast::is_managed_timestamp(&col)
                    || through_link.map_or(false, |link| link.foreign.fields.contains(&col.name))
                {
                    // Integer primary keys, fields with defaults, managed timestamps, and nested
                    // foreign keys are set automatically.
                    continue;
                }

                match queried_fields.get(&col.name) {
                    Some(is_set) => {
                        if !is_set {
                            errors.push(Error {
                                filepath: context.current_filepath.clone(),
                                error_type: ErrorType::InsertColumnIsNotSet {
                                    field: col.name.clone(),
                                },
                                locations: vec![Location {
                                    contexts: vec![],
                                    primary: to_range(&query.start, &query.end),
                                }],
                            })
                        }
                    }
                    None => {
                        missing_fields.push(col.name.clone());
                    }
                }
            }

            if !missing_fields.is_empty() {
                let query_field_name = ast::get_aliased_name(query);
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::InsertMissingColumn {
                        table_name: query_field_name,
                        fields: missing_fields.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: to_range(&query.start, &query.end),
                    }],
                });
            }
        }
        _ => {}
    }
}

fn check_field(
    context: &Context,
    query_context: &QueryContext,
    params: &mut HashMap<String, ParamInfo>,
    operation: &ast::QueryOperation,
    mut errors: &mut Vec<Error>,
    table: &ast::RecordDetails,
    column: &ast::Column,
    field: &ast::QueryField,
) {
    let column_type_str = query_param_type_for_column(table, column);
    match &field.set {
        Some(set) => {
            if matches!(
                operation,
                ast::QueryOperation::Insert | ast::QueryOperation::Update
            ) && ast::is_managed_timestamp(column)
            {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::ManagedColumnCannotBeSet {
                        field: column.name.clone(),
                        operation: operation.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: to_range(&field.start_fieldname, &field.end_fieldname),
                    }],
                });
                return;
            }

            if matches!(operation, ast::QueryOperation::Insert)
                && ast::is_integer_primary_key(column)
            {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::ManagedColumnCannotBeSet {
                        field: column.name.clone(),
                        operation: operation.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: to_range(&field.start_fieldname, &field.end_fieldname),
                    }],
                });
                return;
            }

            check_value(
                context,
                &query_context,
                &set,
                &field.start,
                &field.end,
                &mut errors,
                params,
                &column.name,
                &column_type_str,
                column.nullable,
            );

            // If this is a union variant with nested fields (e.g., Success { message = $message }),
            // we need to process the nested fields to mark variables as used
            if let ast::QueryValue::LiteralTypeValue((_, details)) = set {
                let type_lookup = column
                    .type_
                    .get_custom_type_name()
                    .and_then(|name| context.types.get(name));
                if let Some((_, type_)) = type_lookup {
                    if let Type::OneOf { variants } = type_ {
                        // Find the variant that matches
                        if let Some(variant) = variants.iter().find(|v| v.name == details.name) {
                            // Process fields stored in the LiteralTypeValueDetails (from inline syntax like Create { name = $name })
                            if let Some(variant_fields) = &variant.fields {
                                if let Some(fields) = &details.fields {
                                    // Collect the names of fields that were provided
                                    let provided_field_names: std::collections::HashSet<String> =
                                        fields.iter().map(|(name, _)| name.clone()).collect();

                                    // Process each field assignment to mark variables as used
                                    for (field_name, field_value) in fields {
                                        // Find the matching variant field
                                        if let Some(variant_col) =
                                            variant_fields.iter().find(|f| match f {
                                                ast::Field::Column(col) => col.name == *field_name,
                                                _ => false,
                                            })
                                        {
                                            if let ast::Field::Column(variant_col) = variant_col {
                                                // Check the field value to mark variables as used
                                                let variant_col_type_str =
                                                    variant_col.type_.to_string();
                                                check_value(
                                                    context,
                                                    &query_context,
                                                    field_value,
                                                    &field.start,
                                                    &field.end,
                                                    &mut errors,
                                                    params,
                                                    &variant_col.name,
                                                    &variant_col_type_str,
                                                    variant_col.nullable,
                                                );
                                            }
                                        }
                                    }

                                    // Check that all required (non-nullable) fields are present
                                    let missing_fields: Vec<String> = variant_fields
                                        .iter()
                                        .filter_map(|f| match f {
                                            ast::Field::Column(col) if !col.nullable => {
                                                if !provided_field_names.contains(&col.name) {
                                                    Some(col.name.clone())
                                                } else {
                                                    None
                                                }
                                            }
                                            _ => None,
                                        })
                                        .collect();

                                    if !missing_fields.is_empty() {
                                        let variant_name = format!("{} variant", details.name);
                                        errors.push(Error {
                                            filepath: context.current_filepath.clone(),
                                            error_type: ErrorType::InsertMissingColumn {
                                                table_name: variant_name,
                                                fields: missing_fields,
                                            },
                                            locations: vec![Location {
                                                contexts: vec![],
                                                primary: to_range(&field.start, &field.end),
                                            }],
                                        });
                                    }
                                } else {
                                    // No fields provided, check if variant requires any fields
                                    let required_fields: Vec<String> = variant_fields
                                        .iter()
                                        .filter_map(|f| match f {
                                            ast::Field::Column(col) if !col.nullable => {
                                                Some(col.name.clone())
                                            }
                                            _ => None,
                                        })
                                        .collect();

                                    if !required_fields.is_empty() {
                                        let variant_name = format!("{} variant", details.name);
                                        errors.push(Error {
                                            filepath: context.current_filepath.clone(),
                                            error_type: ErrorType::InsertMissingColumn {
                                                table_name: variant_name,
                                                fields: required_fields,
                                            },
                                            locations: vec![Location {
                                                contexts: vec![],
                                                primary: to_range(&field.start, &field.end),
                                            }],
                                        });
                                    }
                                }
                            }
                            // Also handle nested fields in field.fields (for alternative syntax)
                            if let Some(variant_fields) = &variant.fields {
                                if !field.fields.is_empty() {
                                    // Process nested fields to mark variables as used
                                    for arg_field in &field.fields {
                                        if let ast::ArgField::Field(nested_field) = arg_field {
                                            // Find the matching variant field
                                            if let Some(variant_col) =
                                                variant_fields.iter().find(|f| match f {
                                                    ast::Field::Column(col) => {
                                                        col.name == nested_field.name
                                                    }
                                                    _ => false,
                                                })
                                            {
                                                if let ast::Field::Column(variant_col) = variant_col
                                                {
                                                    // Check the nested field's set value to mark variables as used
                                                    if let Some(nested_set) = &nested_field.set {
                                                        let variant_col_type_str =
                                                            variant_col.type_.to_string();
                                                        check_value(
                                                            context,
                                                            &query_context,
                                                            nested_set,
                                                            &nested_field.start,
                                                            &nested_field.end,
                                                            &mut errors,
                                                            params,
                                                            &variant_col.name,
                                                            &variant_col_type_str,
                                                            variant_col.nullable,
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
                }
            }
        }
        None => {}
    }

    match operation {
        ast::QueryOperation::Query => {
            if field.set.is_some() {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::NoSetsInSelect {
                        field: column.name.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: to_range(&field.start_fieldname, &field.end_fieldname),
                    }],
                })
            }
        }
        ast::QueryOperation::Insert => {
            // Set is required
        }
        ast::QueryOperation::Update => {
            // Set is optional
        }
        ast::QueryOperation::Delete => {
            // Setting is disallowed
            if field.set.is_some() {
                errors.push(Error {
                    filepath: context.current_filepath.clone(),
                    error_type: ErrorType::NoSetsInDelete {
                        field: column.name.clone(),
                    },
                    locations: vec![Location {
                        contexts: vec![],
                        primary: to_range(&field.start_fieldname, &field.end_fieldname),
                    }],
                })
            }
        }
    }
}

fn get_column_reference(fields: &Vec<ast::Field>) -> Vec<(String, String)> {
    let mut known_fields: Vec<(String, String)> = vec![];
    for col in fields {
        match col {
            ast::Field::Column(column) => {
                known_fields.push((column.name.clone(), column.type_.to_string()))
            }
            ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                known_fields.push((link.link_name.clone(), link.foreign.table.clone()))
            }
            _ => (),
        }
    }
    known_fields
}

fn check_link(
    context: &Context,
    query_context: &QueryContext,
    operation: &ast::QueryOperation,
    errors: &mut Vec<Error>,
    local_table: &ast::RecordDetails,
    link: &ast::LinkDetails,
    field: &ast::QueryField,
    params: &mut HashMap<String, ParamInfo>,
    used_namespaces: &mut UsedNamespaces,
) {
    match operation {
        ast::QueryOperation::Insert => {
            // Nested inserts are only allowed if the local_id is a primary key
            let primary_key_field_name = ast::get_primary_id_field_name(&local_table.fields);

            match primary_key_field_name {
                None => (),
                Some(primary_key_name) => {
                    let are_primary = link
                        .local_ids
                        .iter()
                        .all(|s: &String| s == &primary_key_name);
                    if !are_primary {
                        errors.push(Error {
                            filepath: context.current_filepath.clone(),
                            error_type: ErrorType::LinksDisallowedInInserts {
                                field: link.link_name.clone(),
                                table_name: local_table.name.clone(),
                                local_ids: link.local_ids.clone(),
                            },
                            locations: vec![Location {
                                contexts: vec![],
                                primary: to_range(&field.start, &field.end),
                            }],
                        });
                    }
                }
            };
        }
        ast::QueryOperation::Update => {
            errors.push(Error {
                filepath: context.current_filepath.clone(),
                error_type: ErrorType::LinksDisallowedInUpdates {
                    field: link.link_name.clone(),
                },
                locations: vec![Location {
                    contexts: vec![],
                    primary: to_range(&field.start, &field.end),
                }],
            });
        }
        ast::QueryOperation::Delete => {
            errors.push(Error {
                filepath: context.current_filepath.clone(),
                error_type: ErrorType::LinksDisallowedInDeletes {
                    field: link.link_name.clone(),
                },
                locations: vec![Location {
                    contexts: vec![],
                    primary: to_range(&field.start, &field.end),
                }],
            });
            return ();
        }
        _ => (),
    }

    if field.fields.is_empty() {
        let known_fields: Vec<(String, String)> = vec![];

        errors.push(Error {
            filepath: context.current_filepath.clone(),
            error_type: ErrorType::LinkSelectionIsEmpty {
                link_name: link.link_name.clone(),
                foreign_table: link.foreign.table.clone(),
                foreign_table_fields: known_fields,
            },
            locations: vec![Location {
                // contexts: to_range(&field.start, &field.end),
                contexts: vec![],
                primary: to_range(&field.start_fieldname, &field.end_fieldname),
            }],
        })
    } else {
        let table = get_linked_table(context, link);
        match table {
            None => errors.push(Error {
                filepath: context.current_filepath.clone(),
                error_type: ErrorType::UnknownTable {
                    found: link.foreign.table.clone(),
                    existing: context
                        .tables
                        .values()
                        .map(|table| format!("{}.{}", table.schema, table.record.name))
                        .collect(),
                },
                locations: vec![Location {
                    contexts: vec![],
                    primary: to_range(&field.start, &field.end),
                }],
            }),
            Some(table) => check_table_query(
                context,
                &query_context,
                errors,
                operation,
                Some(link),
                table,
                field,
                params,
                used_namespaces,
            ),
        }
    }
}
