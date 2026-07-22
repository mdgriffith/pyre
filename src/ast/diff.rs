use crate::ast;
use crate::error::{ColumnDiff, Error, ErrorType};

// Define a type to represent the diff of two schemas
#[derive(Debug)]
pub struct SchemaDiff {
    pub added: Vec<crate::ast::Definition>,
    pub removed: Vec<crate::ast::Definition>,
    pub modified_records: Vec<DetailedRecordDiff>,
    pub modified_taggeds: Vec<DetailedTaggedDiff>,
}

pub fn to_errors(diff: SchemaDiff) -> Vec<Error> {
    let mut errors = Vec::new();

    // Check for dropped tables
    for table in &diff.removed {
        match table {
            crate::ast::Definition::Record { name, .. } => {
                errors.push(Error {
                    error_type: ErrorType::MigrationTableDropped {
                        table_name: name.clone(),
                    },
                    filepath: "".to_string(), // We don't have filepath information in diffs
                    locations: vec![],
                });
            }
            _ => {}
        }
    }

    // Check for modified records
    for record_diff in &diff.modified_records {
        for change in &record_diff.changes {
            match change {
                RecordChange::RemovedField(field) => {
                    errors.push(Error {
                        error_type: ErrorType::MigrationColumnDropped {
                            table_name: record_diff.name.clone(),
                            column_name: field.name.clone(),
                            added_columns: vec![],
                        },
                        filepath: "".to_string(),
                        locations: vec![],
                    });
                }
                RecordChange::ModifiedField { name, changes } => {
                    errors.push(Error {
                        error_type: ErrorType::MigrationColumnModified {
                            table_name: record_diff.name.clone(),
                            column_name: name.clone(),
                            changes: changes.clone(),
                        },
                        filepath: "".to_string(),
                        locations: vec![],
                    });
                }
                _ => {}
            }
        }
    }

    // Check for modified tagged types
    for tagged_diff in &diff.modified_taggeds {
        for change in &tagged_diff.changes {
            match change {
                TaggedChange::RemovedVariant(variant) => {
                    errors.push(Error {
                        error_type: ErrorType::MigrationVariantRemoved {
                            tagged_name: tagged_diff.name.clone(),
                            variant_name: variant.name.clone(),
                        },
                        filepath: "".to_string(),
                        locations: vec![],
                    });
                }
                _ => {}
            }
        }
    }

    errors
}

// These are semantic errors in the diff
// Which involves the schema changes that are not acknoledged by the new schema.
#[derive(Debug)]
pub struct DetailedTaggedDiff {
    pub name: String,
    pub changes: Vec<TaggedChange>,
}

#[derive(Debug)]
pub struct DetailedRecordDiff {
    pub name: String,
    pub changes: Vec<RecordChange>,
}

#[derive(Debug)]
pub enum TaggedChange {
    AddedVariant(crate::ast::Variant),
    RemovedVariant(crate::ast::Variant),
    ModifiedVariant {
        name: String,
        old: crate::ast::Variant,
        new: crate::ast::Variant,
    },
}

#[derive(Debug)]
pub enum RecordChange {
    AddedField(crate::ast::Column),
    RemovedField(crate::ast::Column),
    ModifiedField { name: String, changes: ColumnDiff },
}

// Function to diff two Schema values
pub fn diff_schema(old_schema: &crate::ast::Schema, new_schema: &crate::ast::Schema) -> SchemaDiff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified_records = Vec::new();
    let mut modified_taggeds = Vec::new();

    // Collect `Tagged` and `Record` definitions from both schemas
    let mut old_defs: Vec<ast::Definition> = vec![];
    for file in old_schema.files.iter() {
        for def in &file.definitions {
            match def {
                crate::ast::Definition::Tagged { .. } | crate::ast::Definition::Record { .. } => {
                    old_defs.push(def.clone());
                }
                _ => continue,
            }
        }
    }

    let mut new_defs: Vec<ast::Definition> = vec![];
    for new_file in new_schema.files.iter() {
        for new_def in &new_file.definitions {
            match new_def {
                crate::ast::Definition::Tagged { .. } | crate::ast::Definition::Record { .. } => {
                    new_defs.push(new_def.clone());
                }
                _ => continue,
            }
        }
    }

    // Find added and modified definitions
    for new_def in &new_defs {
        if let Some(old_def) = old_defs.iter().find(|&d| match (d, new_def) {
            (
                crate::ast::Definition::Tagged { name: name1, .. },
                crate::ast::Definition::Tagged { name: name2, .. },
            ) => name1 == name2,
            (
                crate::ast::Definition::Record { name: name1, .. },
                crate::ast::Definition::Record { name: name2, .. },
            ) => name1 == name2,
            _ => false,
        }) {
            if old_def != new_def {
                match new_def {
                    crate::ast::Definition::Tagged { name, .. } => {
                        let changes = find_tagged_changes(old_def, new_def);
                        modified_taggeds.push(DetailedTaggedDiff {
                            name: name.clone(),
                            changes,
                        });
                    }
                    crate::ast::Definition::Record { name, .. } => {
                        let changes = find_record_changes(old_def, new_def);
                        modified_records.push(DetailedRecordDiff {
                            name: name.clone(),
                            changes,
                        });
                    }
                    _ => {}
                }
            }
        } else {
            added.push((*new_def).clone());
        }
    }

    // Find removed definitions
    for old_def in &old_defs {
        if !new_defs.iter().any(|d| match (d, old_def) {
            (
                crate::ast::Definition::Tagged { name: name2, .. },
                crate::ast::Definition::Tagged { name: name1, .. },
            ) => name1 == name2,
            (
                crate::ast::Definition::Record { name: name2, .. },
                crate::ast::Definition::Record { name: name1, .. },
            ) => name1 == name2,
            _ => false,
        }) {
            removed.push((*old_def).clone());
        }
    }

    SchemaDiff {
        added,
        removed,
        modified_records,
        modified_taggeds,
    }
}

// Function to find changes between two Tagged definitions
fn find_tagged_changes(
    def1: &crate::ast::Definition,
    def2: &crate::ast::Definition,
) -> Vec<TaggedChange> {
    match (def1, def2) {
        (
            crate::ast::Definition::Tagged { variants: v1, .. },
            crate::ast::Definition::Tagged { variants: v2, .. },
        ) => diff_variants(v1, v2),
        _ => vec![],
    }
}

// Function to find changes between two Record definitions
fn find_record_changes(
    def1: &crate::ast::Definition,
    def2: &crate::ast::Definition,
) -> Vec<RecordChange> {
    match (def1, def2) {
        (
            crate::ast::Definition::Record { fields: f1, .. },
            crate::ast::Definition::Record { fields: f2, .. },
        ) => diff_fields(&ast::collect_columns(f1), &ast::collect_columns(f2)),
        _ => vec![],
    }
}

// Function to find changes between two lists of fields
fn diff_fields(
    fields1: &[crate::ast::Column],
    fields2: &[crate::ast::Column],
) -> Vec<RecordChange> {
    let mut changes = Vec::new();

    for field2 in fields2 {
        if let Some(field1) = fields1.iter().find(|f| f.name == field2.name) {
            if let Some(column_diff) = diff_column(field1, field2) {
                changes.push(RecordChange::ModifiedField {
                    name: field2.name.clone(),
                    changes: column_diff,
                });
            }
        } else {
            changes.push(RecordChange::AddedField(field2.clone()));
        }
    }

    for field1 in fields1 {
        if !fields2.iter().any(|f| f.name == field1.name) {
            changes.push(RecordChange::RemovedField(field1.clone()));
        }
    }

    changes
}

// Function to find changes between two lists of variants
fn diff_variants(
    variants1: &[crate::ast::Variant],
    variants2: &[crate::ast::Variant],
) -> Vec<TaggedChange> {
    let mut changes = Vec::new();

    for variant2 in variants2 {
        if let Some(variant1) = variants1.iter().find(|v| v.name == variant2.name) {
            if variant1 != variant2 {
                changes.push(TaggedChange::ModifiedVariant {
                    name: variant2.name.clone(),
                    old: variant1.clone(),
                    new: variant2.clone(),
                });
            }
        } else {
            changes.push(TaggedChange::AddedVariant(variant2.clone()));
        }
    }

    for variant1 in variants1 {
        if !variants2.iter().any(|v| v.name == variant1.name) {
            changes.push(TaggedChange::RemovedVariant(variant1.clone()));
        }
    }

    changes
}

fn default_values_equal(old: &crate::ast::DefaultValue, new: &crate::ast::DefaultValue) -> bool {
    use crate::ast::{DefaultValue, QueryValue};

    match (old, new) {
        (DefaultValue::Now, DefaultValue::Now) => true,
        (DefaultValue::Value(old), DefaultValue::Value(new)) => match (old, new) {
            (QueryValue::String((_, old)), QueryValue::String((_, new))) => old == new,
            (QueryValue::Int((_, old)), QueryValue::Int((_, new))) => old == new,
            (QueryValue::Float((_, old)), QueryValue::Float((_, new))) => old == new,
            (QueryValue::Bool((_, old)), QueryValue::Bool((_, new))) => old == new,
            (QueryValue::Null(_), QueryValue::Null(_)) => true,
            _ => false,
        },
        _ => false,
    }
}

fn directives_equal(old: &crate::ast::ColumnDirective, new: &crate::ast::ColumnDirective) -> bool {
    match (old, new) {
        (
            crate::ast::ColumnDirective::Default { value: old, .. },
            crate::ast::ColumnDirective::Default { value: new, .. },
        ) => default_values_equal(old, new),
        _ => old == new,
    }
}

fn diff_column(old: &crate::ast::Column, new: &crate::ast::Column) -> Option<ColumnDiff> {
    let mut has_changes = false;
    let mut diff = ColumnDiff {
        type_changed: None,
        nullable_changed: None,
        added_directives: Vec::new(),
        removed_directives: Vec::new(),
    };

    // Create HashMaps with directive keys
    let mut old_directives = std::collections::HashMap::new();
    let mut new_directives = std::collections::HashMap::new();

    // Helper function to get directive key
    let get_key = |directive: &crate::ast::ColumnDirective| -> String {
        match directive {
            crate::ast::ColumnDirective::PrimaryKey => "_key".to_string(),
            crate::ast::ColumnDirective::Unique => "_uniq".to_string(),
            crate::ast::ColumnDirective::Index => "_idx".to_string(),
            crate::ast::ColumnDirective::CreatedAt => "_createdAt".to_string(),
            crate::ast::ColumnDirective::UpdatedAt => "_updatedAt".to_string(),
            crate::ast::ColumnDirective::Default { .. } => "_default".to_string(),
        }
    };

    // Populate HashMaps
    for directive in &old.directives {
        old_directives.insert(get_key(directive), directive.clone());
    }
    for directive in &new.directives {
        new_directives.insert(get_key(directive), directive.clone());
    }

    // Find added directives
    for (key, directive) in &new_directives {
        if !old_directives
            .get(key)
            .is_some_and(|old| directives_equal(old, directive))
        {
            diff.added_directives.push(directive.clone());
            has_changes = true;
        }
    }

    // Find removed directives
    for (key, directive) in &old_directives {
        if !new_directives
            .get(key)
            .is_some_and(|new| directives_equal(directive, new))
        {
            diff.removed_directives.push(directive.clone());
            has_changes = true;
        }
    }

    if has_changes {
        Some(diff)
    } else {
        None
    }
}
