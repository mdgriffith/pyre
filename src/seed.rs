use crate::ast;
use crate::ext::string;
use crate::typecheck;
use std::collections::HashMap;

/// Options for configuring seed data generation
#[derive(Debug, Clone)]
pub struct Options {
    /// Optional seed for deterministic generation. If None, uses a default seed.
    /// Same seed + schema + options will produce the same dataset.
    pub seed: Option<u64>,
    /// Default number of rows per table (if not specified per-table)
    pub default_rows_per_table: usize,
    /// Per-table row counts (overrides default_rows_per_table)
    pub table_rows: HashMap<String, usize>,
    /// Foreign key relationship ratios: (from_table, to_table) -> ratio
    /// e.g., ("users", "posts") -> 10.0 means ~10 posts per user
    pub foreign_key_ratios: HashMap<(String, String), f64>,
    /// Default foreign key ratio multiplier (default: 5.0)
    pub default_foreign_key_ratio: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            seed: None,
            default_rows_per_table: 1000,
            table_rows: HashMap::new(),
            foreign_key_ratios: HashMap::new(),
            default_foreign_key_ratio: 5.0,
        }
    }
}

/// Simple deterministic RNG using Linear Congruential Generator
/// This ensures the same seed produces the same sequence of values
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        DeterministicRng { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // LCG parameters (same as used in many standard libraries)
        self.state = self.state.wrapping_mul(1103515245).wrapping_add(12345);
        self.state
    }

    fn next_usize(&mut self) -> usize {
        self.next_u64() as usize
    }
}

/// A single SQL operation (INSERT statement)
#[derive(Debug, Clone)]
pub struct SqlOperation {
    pub sql: String,
}

/// Generate seed data for a database schema
///
/// Takes both `schema` and `context` - the context provides type information
/// and sync_layer ordering that's already computed.
pub fn seed_database(
    schema: &ast::Schema,
    context: &typecheck::Context,
    options: Option<Options>,
) -> Vec<SqlOperation> {
    let options = options.unwrap_or_default();
    let seed = options.seed.unwrap_or(12345); // Default seed if not provided

    if context.tables.is_empty() {
        return Vec::new();
    }

    // Initialize RNG with seed
    let mut rng = DeterministicRng::new(seed);

    // Calculate row counts based on foreign key ratios
    let row_counts = calculate_row_counts(context, &options, schema);

    // Sort tables by sync_layer (lower numbers first), then by table name for determinism
    let mut tables_by_layer: Vec<(&String, &typecheck::Table)> = context.tables.iter().collect();
    tables_by_layer.sort_by(|(name_a, table_a), (name_b, table_b)| {
        table_a
            .sync_layer
            .cmp(&table_b.sync_layer)
            .then_with(|| name_a.cmp(name_b))
    });

    // Track generated IDs for foreign key references and per-table ID counters
    let mut generated_ids: HashMap<String, Vec<i64>> = HashMap::new();
    let mut table_id_counters: HashMap<String, i64> = HashMap::new();

    // Generate SQL statements
    let mut sql_operations = Vec::new();

    for (_record_name, table) in tables_by_layer {
        let record = &table.record;
        let table_name = ast::get_tablename(&record.name, &record.fields);

        // Get the calculated number of rows
        // Try table_name first (e.g., "users"), then record.name (e.g., "User")
        let num_rows = row_counts
            .get(&table_name)
            .or_else(|| row_counts.get(&record.name))
            .copied()
            .unwrap_or(options.default_rows_per_table);

        // Get columns and links
        let columns = ast::collect_columns(&record.fields);
        let links = ast::collect_links(&record.fields);

        // Pre-compute column names and foreign key mappings (same for all rows)
        // Pre-compute which columns are foreign keys to avoid repeated checks
        let foreign_key_column_names: std::collections::HashSet<String> = links
            .iter()
            .flat_map(|link| link.local_ids.iter().cloned())
            .collect();

        let mut column_names = Vec::new();
        let mut foreign_key_mappings: Vec<(String, Vec<i64>, f64, bool)> = Vec::new(); // (local_id, foreign_ids, ratio, is_primary)

        // Collect non-foreign-key columns first
        for col in &columns {
            if !ast::is_sequence(col) && !foreign_key_column_names.contains(&col.name) {
                column_names.push(col.name.clone());
            }
        }

        // Pre-compute foreign key lookups
        for link in &links {
            for local_id in &link.local_ids {
                if !column_names.contains(local_id) {
                    column_names.push(local_id.clone());

                    // Check if this column is also a primary key
                    let col = columns.iter().find(|c| c.name == *local_id);
                    let is_also_primary = col.map(|c| ast::is_primary_key(c)).unwrap_or(false);

                    if is_also_primary {
                        // Primary key + foreign key - will use current_id
                        foreign_key_mappings.push((local_id.clone(), Vec::new(), 0.0, true));
                    } else {
                        // Regular foreign key - pre-compute the foreign IDs lookup
                        let foreign_table_name = ast::get_foreign_tablename(schema, link);
                        let foreign_ids = generated_ids
                            .get(&foreign_table_name)
                            .or_else(|| generated_ids.get(&link.foreign.table))
                            .or_else(|| {
                                context
                                    .tables
                                    .get(&crate::ext::string::decapitalize(&link.foreign.table))
                                    .and_then(|foreign_table| {
                                        let foreign_table_name_from_context = ast::get_tablename(
                                            &foreign_table.record.name,
                                            &foreign_table.record.fields,
                                        );
                                        generated_ids.get(&foreign_table_name_from_context)
                                    })
                            })
                            .cloned()
                            .unwrap_or_default();

                        let ratio =
                            get_foreign_key_ratio(&link.foreign.table, &record.name, &options);

                        foreign_key_mappings.push((local_id.clone(), foreign_ids, ratio, false));
                    }
                }
            }
        }

        // Initialize ID counter for this table (start at 1)
        let id_counter_ref = table_id_counters.entry(table_name.clone()).or_insert(1);
        if *id_counter_ref < 1 {
            *id_counter_ref = 1;
        }

        // Generate rows in batches for better performance
        // SQLite supports up to ~500-1000 values per INSERT, but we'll use 100 for safety
        const BATCH_SIZE: usize = 100;
        let mut row_ids = Vec::with_capacity(num_rows);
        let mut batch_values = Vec::new();

        for row_idx in 0..num_rows {
            // Get the current ID for this row and increment for next row
            let current_id = *id_counter_ref;
            *id_counter_ref += 1;

            let mut column_values = Vec::with_capacity(column_names.len());

            // Generate non-foreign-key column values
            for col in &columns {
                if !ast::is_sequence(col) && !foreign_key_column_names.contains(&col.name) {
                    let value = generate_column_value(
                        col,
                        row_idx,
                        context,
                        &mut rng,
                        &table_name,
                        &current_id,
                    );
                    column_values.push(value);
                }
            }

            // Handle foreign key columns using pre-computed mappings
            for (local_id, foreign_ids, ratio, is_primary) in &foreign_key_mappings {
                if *is_primary {
                    column_values.push(current_id.to_string());
                } else if foreign_ids.is_empty() {
                    // No foreign keys available - find the column to check nullable
                    let col = columns.iter().find(|c| c.name == *local_id);
                    if let Some(col) = col {
                        if col.nullable {
                            column_values.push("NULL".to_string());
                        } else {
                            column_values.push("1".to_string());
                        }
                    } else {
                        column_values.push("1".to_string());
                    }
                } else {
                    // Pick a foreign key based on ratio
                    let parent_idx = (row_idx as f64 / ratio) as usize % foreign_ids.len().max(1);
                    column_values.push(foreign_ids[parent_idx].to_string());
                }
            }

            // Add this row's values to the batch
            batch_values.push(column_values);
            row_ids.push(current_id);

            // Generate batched INSERT when batch is full or at the end
            if batch_values.len() >= BATCH_SIZE || row_idx == num_rows - 1 {
                if !batch_values.is_empty() && !column_names.is_empty() {
                    // Build batched INSERT: INSERT INTO table (cols) VALUES (row1), (row2), ...
                    let quoted_table = string::quote(&table_name);
                    let quoted_columns: Vec<String> =
                        column_names.iter().map(|n| string::quote(n)).collect();
                    let columns_str = quoted_columns.join(", ");

                    let values_strs: Vec<String> = batch_values
                        .iter()
                        .map(|values| format!("({})", values.join(", ")))
                        .collect();
                    let values_str = values_strs.join(", ");

                    let sql = format!(
                        "INSERT INTO {} ({}) VALUES {};",
                        quoted_table, columns_str, values_str
                    );
                    sql_operations.push(SqlOperation { sql });
                    batch_values.clear();
                }
            }
        }

        // Store generated IDs for this table
        generated_ids.insert(table_name.clone(), row_ids.clone());
        generated_ids.insert(record.name.clone(), row_ids);
    }

    sql_operations
}

/// Calculate row counts for each table based on foreign key ratios
fn calculate_row_counts(
    context: &typecheck::Context,
    options: &Options,
    schema: &ast::Schema,
) -> HashMap<String, usize> {
    let mut row_counts: HashMap<String, usize> = HashMap::new();

    // Sort tables by sync_layer to process parents before children, then by name for determinism
    let mut tables_by_layer: Vec<(&String, &typecheck::Table)> = context.tables.iter().collect();
    tables_by_layer.sort_by(|(name_a, table_a), (name_b, table_b)| {
        table_a
            .sync_layer
            .cmp(&table_b.sync_layer)
            .then_with(|| name_a.cmp(name_b))
    });

    // Pre-compute table names to avoid repeated calls to get_tablename
    let mut table_name_cache: HashMap<String, String> = HashMap::new();
    for (_record_name, table) in &tables_by_layer {
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        table_name_cache.insert(table.record.name.clone(), table_name.clone());
    }

    // First pass: set base row counts
    for (_record_name, table) in &tables_by_layer {
        let table_name = table_name_cache
            .get(&table.record.name)
            .cloned()
            .unwrap_or_else(|| ast::get_tablename(&table.record.name, &table.record.fields));
        let base_rows = options
            .table_rows
            .get(&table_name)
            .or_else(|| options.table_rows.get(&table.record.name))
            .copied()
            .unwrap_or(options.default_rows_per_table);
        row_counts.insert(table_name.clone(), base_rows);
        row_counts.insert(table.record.name.clone(), base_rows);
    }

    // Second pass: adjust based on foreign key ratios
    // But don't override explicitly set table_rows
    for (_record_name, table) in &tables_by_layer {
        let table_name = table_name_cache
            .get(&table.record.name)
            .cloned()
            .unwrap_or_else(|| ast::get_tablename(&table.record.name, &table.record.fields));

        // Skip if this table has an explicit row count set
        let has_explicit_count = options.table_rows.contains_key(&table_name)
            || options.table_rows.contains_key(&table.record.name);
        if has_explicit_count {
            continue;
        }

        let links = ast::collect_links(&table.record.fields);
        let columns = ast::collect_columns(&table.record.fields);
        let column_map: std::collections::HashMap<String, &ast::Column> =
            columns.iter().map(|c| (c.name.clone(), c)).collect();

        for link in links {
            // Check if local_ids contain a primary key column of the current table.
            // If they do, this is a reverse link (current table is the parent).
            // Otherwise, this is a forward link (current table is the child).
            let is_child = link.local_ids.iter().any(|id| {
                column_map
                    .get(id)
                    .map(|col| !ast::is_primary_key(col))
                    .unwrap_or(false)
            });

            if is_child {
                // This table is the child - update this table's count based on parent
                let foreign_table_name = ast::get_foreign_tablename(schema, &link);
                let ratio = get_foreign_key_ratio(&link.foreign.table, &table.record.name, options);

                // Calculate how many child rows we need based on parent rows and ratio
                let parent_rows = row_counts
                    .get(&foreign_table_name)
                    .or_else(|| row_counts.get(&link.foreign.table))
                    .copied()
                    .unwrap_or_else(|| {
                        // If parent table not found, use default_rows_per_table
                        // But also try to look it up by record name in context
                        if let Some(parent_table) = context
                            .tables
                            .get(&crate::ext::string::decapitalize(&link.foreign.table))
                        {
                            let parent_table_name = ast::get_tablename(
                                &parent_table.record.name,
                                &parent_table.record.fields,
                            );
                            row_counts
                                .get(&parent_table_name)
                                .or_else(|| row_counts.get(&parent_table.record.name))
                                .copied()
                                .unwrap_or(options.default_rows_per_table)
                        } else {
                            options.default_rows_per_table
                        }
                    });

                let child_rows = (parent_rows as f64 * ratio) as usize;

                // Update child table row count if calculated value is larger
                let current_child_rows = row_counts
                    .get(&table_name)
                    .or_else(|| row_counts.get(&table.record.name))
                    .copied()
                    .unwrap_or(0);

                if child_rows > current_child_rows {
                    row_counts.insert(table_name.clone(), child_rows);
                    row_counts.insert(table.record.name.clone(), child_rows);
                }
            } else {
                // This table is the parent - update child table's count based on this table
                let child_table_name = ast::get_foreign_tablename(schema, &link);
                let ratio = get_foreign_key_ratio(&table.record.name, &link.foreign.table, options);

                // Calculate how many child rows we need based on parent rows and ratio
                let parent_rows = row_counts
                    .get(&table_name)
                    .or_else(|| row_counts.get(&table.record.name))
                    .copied()
                    .unwrap_or(options.default_rows_per_table);

                let child_rows = (parent_rows as f64 * ratio) as usize;

                // Update child table row count if calculated value is larger
                // But skip if child table has an explicit count set
                let child_has_explicit_count = options.table_rows.contains_key(&child_table_name)
                    || options.table_rows.contains_key(&link.foreign.table);

                if !child_has_explicit_count {
                    let current_child_rows = row_counts
                        .get(&child_table_name)
                        .or_else(|| row_counts.get(&link.foreign.table))
                        .copied()
                        .unwrap_or(0);

                    if child_rows > current_child_rows {
                        if let Some(child_table) = context
                            .tables
                            .get(&crate::ext::string::decapitalize(&link.foreign.table))
                        {
                            let child_table_name_from_context = ast::get_tablename(
                                &child_table.record.name,
                                &child_table.record.fields,
                            );
                            row_counts.insert(child_table_name_from_context.clone(), child_rows);
                            row_counts.insert(child_table.record.name.clone(), child_rows);
                        } else {
                            row_counts.insert(child_table_name.clone(), child_rows);
                            row_counts.insert(link.foreign.table.clone(), child_rows);
                        }
                    }
                }
            }
        }
    }

    row_counts
}

/// Get the foreign key ratio for a relationship
/// Tries multiple name variations to handle capitalization differences
fn get_foreign_key_ratio(from_table: &str, to_table: &str, options: &Options) -> f64 {
    // Try exact match first
    if let Some(ratio) = options
        .foreign_key_ratios
        .get(&(from_table.to_string(), to_table.to_string()))
    {
        return *ratio;
    }

    // Try reversed order
    if let Some(ratio) = options
        .foreign_key_ratios
        .get(&(to_table.to_string(), from_table.to_string()))
    {
        return *ratio;
    }

    // Try with decapitalized names
    let from_decapped = crate::ext::string::decapitalize(from_table);
    let to_decapped = crate::ext::string::decapitalize(to_table);
    if let Some(ratio) = options
        .foreign_key_ratios
        .get(&(from_decapped.clone(), to_decapped.clone()))
    {
        return *ratio;
    }

    // Try reversed decapitalized
    if let Some(ratio) = options
        .foreign_key_ratios
        .get(&(to_decapped.clone(), from_decapped.clone()))
    {
        return *ratio;
    }

    // Try mixed case variations
    if let Some(ratio) = options
        .foreign_key_ratios
        .get(&(from_table.to_string(), to_decapped.clone()))
    {
        return *ratio;
    }
    if let Some(ratio) = options
        .foreign_key_ratios
        .get(&(from_decapped.clone(), to_table.to_string()))
    {
        return *ratio;
    }

    options.default_foreign_key_ratio
}

/// Generate a value for a column based on its type and constraints
fn generate_column_value(
    col: &ast::Column,
    row_idx: usize,
    context: &typecheck::Context,
    rng: &mut DeterministicRng,
    table_name: &str,
    id_counter: &i64,
) -> String {
    // Check if column has a default value
    if ast::has_default_value(col) {
        for directive in &col.directives {
            if let ast::ColumnDirective::Default { value, .. } = directive {
                match value {
                    ast::DefaultValue::Now => return "unixepoch()".to_string(),
                    ast::DefaultValue::Value(_) => {
                        // For now, we'll still generate a value
                        // In a full implementation, we might want to use the default
                    }
                }
            }
        }
    }

    // Check if this is a primary key
    if ast::is_primary_key(col) {
        // Use the ID counter to ensure unique IDs per table
        return id_counter.to_string();
    }

    // Generate value based on serialization type
    match col.type_.to_serialization_type() {
        ast::SerializationType::Concrete(concrete_type) => {
            generate_concrete_value(&concrete_type, col, row_idx, rng, table_name)
        }
        ast::SerializationType::FromType(type_name) => {
            // For named types (like tagged unions), generate a variant
            generate_named_type_value(&type_name, context, rng)
        }
    }
}

/// Generate a value for a concrete serialization type
fn generate_concrete_value(
    concrete_type: &ast::ConcreteSerializationType,
    col: &ast::Column,
    row_idx: usize,
    rng: &mut DeterministicRng,
    table_name: &str,
) -> String {
    match concrete_type {
        ast::ConcreteSerializationType::Integer => {
            // Generate a varied integer using RNG
            let base = rng.next_u64() % 10000;
            base.to_string()
        }
        ast::ConcreteSerializationType::Real => {
            // Generate a varied float using RNG
            let base = rng.next_u64() % 10000;
            let fractional = rng.next_u64() % 100;
            format!("{}.{}", base, fractional)
        }
        ast::ConcreteSerializationType::Text => {
            // Generate varied text (properly escaped)
            let text = generate_text_value(&col.name, row_idx, rng, table_name);
            // Escape single quotes for SQL
            let escaped = text.replace("'", "''");
            format!("'{}'", escaped)
        }
        ast::ConcreteSerializationType::Blob => {
            // Generate hex blob using RNG
            let mut hex_chars = String::new();
            for _ in 0..16 {
                hex_chars.push_str(&format!("{:02x}", rng.next_u64() % 256));
            }
            format!("X'{}'", hex_chars)
        }
        ast::ConcreteSerializationType::Date => {
            // Generate a varied date string (YYYY-MM-DD) using RNG
            // Spread dates over a wider range (e.g., 20 years)
            let days_range = 20 * 365; // 20 years
            let days_offset = rng.next_u64() % days_range;
            let base_year = 2000;
            let year = base_year + (days_offset / 365) as u32;
            let day_of_year = (days_offset % 365) as u32;

            // Convert day of year to month and day
            let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            let mut remaining_days = day_of_year;
            let mut month = 1;
            for &days_in_month in &month_days {
                if remaining_days < days_in_month {
                    break;
                }
                remaining_days -= days_in_month;
                month += 1;
            }
            let day = remaining_days + 1;
            let month = month.min(12);
            let max_day = month_days[(month - 1) as usize];
            let day = day.min(max_day);

            format!("'{}-{:02}-{:02}'", year, month, day)
        }
        ast::ConcreteSerializationType::DateTime => {
            // Generate varied unix timestamp using RNG
            // Spread over a wider range (e.g., 20 years)
            let base_timestamp = 946684800; // 2000-01-01 00:00:00 UTC
            let year_range_seconds = 20 * 365 * 24 * 3600;
            let offset = (rng.next_u64() % year_range_seconds as u64) as i64;
            (base_timestamp + offset).to_string()
        }
        ast::ConcreteSerializationType::VectorBlob { .. } => {
            // Generate hex blob for vector using RNG
            let mut hex_chars = String::new();
            for _ in 0..64 {
                hex_chars.push_str(&format!("{:02x}", rng.next_u64() % 256));
            }
            format!("X'{}'", hex_chars)
        }
        ast::ConcreteSerializationType::JsonB => {
            // Generate simple JSON using RNG
            let id = rng.next_u64() % 1000000;
            format!("'{{\"id\": {}}}'", id)
        }
        ast::ConcreteSerializationType::IdInt => {
            // Generate a unique integer ID using RNG
            let base = rng.next_u64() % 10000;
            base.to_string()
        }
        ast::ConcreteSerializationType::IdUuid => {
            // Generate a UUID string
            let uuid = format!(
                "'{:08x}-{:04x}-{:04x}-{:04x}-{:012x}'",
                rng.next_u64() as u32,
                (rng.next_u64() % 65536) as u16,
                (rng.next_u64() % 65536) as u16,
                (rng.next_u64() % 65536) as u16,
                rng.next_u64()
            );
            uuid
        }
    }
}

/// Generate a value for a named type (tagged union)
fn generate_named_type_value(
    type_name: &str,
    context: &typecheck::Context,
    rng: &mut DeterministicRng,
) -> String {
    // Look up the type in the context
    if let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(type_name) {
        if variants.is_empty() {
            return "'Unknown'".to_string();
        }

        // Pick a variant using RNG for more variation
        let variant_idx = rng.next_usize() % variants.len();
        let variant = &variants[variant_idx];

        // For simple variants (no fields), just return the variant name
        if variant.fields.is_none() {
            return format!("'{}'", variant.name);
        }

        // For variants with fields, we'd need to generate field values
        // For now, just return the variant name
        format!("'{}'", variant.name)
    } else {
        // Type not found, generate a placeholder using RNG
        let variant_num = (rng.next_usize() % 3) + 1;
        format!("'Variant{}'", variant_num)
    }
}

/// Generate a text value based on column name and row index
/// Returns the text without quotes (caller should add quotes and escape)
fn generate_text_value(
    column_name: &str,
    row_idx: usize,
    rng: &mut DeterministicRng,
    table_name: &str,
) -> String {
    let name_lower = column_name.to_lowercase();
    let random_suffix = rng.next_u64() % 1000000;

    // Generate contextually appropriate text based on column name
    if name_lower.contains("email") {
        format!("user{}_{}@example.com", row_idx + 1, random_suffix)
    } else if name_lower.contains("name") || name_lower.contains("title") {
        format!(
            "{} {} {}",
            capitalize_first(&column_name),
            row_idx + 1,
            random_suffix
        )
    } else if name_lower.contains("description") || name_lower.contains("content") {
        format!(
            "This is a sample {} for row {} with id {}",
            column_name,
            row_idx + 1,
            random_suffix
        )
    } else if name_lower.contains("url") || name_lower.contains("link") {
        format!("https://example.com/{}/{}", table_name, random_suffix)
    } else {
        // Generic text
        format!("{}_value_{}_{}", column_name, row_idx + 1, random_suffix)
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
