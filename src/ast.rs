use crate::ext::string;
use serde::{Deserialize, Serialize};

pub mod diff;

#[derive(Debug)]
pub struct Database {
    pub schemas: Vec<Schema>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    pub namespace: String,
    pub sync_mode: SyncMode,
    pub session: Option<SessionDetails>,
    pub files: Vec<SchemaFile>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncMode {
    Synced,
    QueryOnly,
}

impl SyncMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncMode::Synced => "synced",
            SyncMode::QueryOnly => "query-only",
        }
    }

    pub fn syncable_literal(&self) -> &'static str {
        match self {
            SyncMode::Synced => "true",
            SyncMode::QueryOnly => "false",
        }
    }
}

pub const DEFAULT_SCHEMANAME: &str = "_default";

impl Default for Schema {
    fn default() -> Self {
        Schema {
            namespace: DEFAULT_SCHEMANAME.to_string(),
            sync_mode: SyncMode::Synced,
            session: None,
            files: Vec::new(),
        }
    }
}

pub fn default_session_details() -> SessionDetails {
    SessionDetails {
        fields: Vec::new(),
        start: None,
        end: None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionDetails {
    pub fields: Vec<Field>,

    pub start: Option<Location>,
    pub end: Option<Location>,
}

pub fn is_empty_schema(schema: &Schema) -> bool {
    for file in schema.files.iter() {
        if file.definitions.len() > 0 {
            return true;
        }
    }
    return false;
}

#[derive(Debug, Clone, PartialEq)]
pub struct SchemaFile {
    pub path: String,
    pub definitions: Vec<Definition>,
}

#[repr(u8)]
#[derive(Debug, Clone, PartialEq)]
pub enum Definition {
    Lines {
        count: usize,
    },
    Comment {
        text: String,
    },
    Tagged {
        name: String,
        variants: Vec<Variant>,
        start: Option<Location>,
        end: Option<Location>,
    },
    SyncMode(SyncMode),
    Session(SessionDetails),
    Record {
        name: String,
        fields: Vec<Field>,
        start: Option<Location>,
        end: Option<Location>,

        start_name: Option<Location>,
        end_name: Option<Location>,
    },
}
pub fn get_permissions(record: &RecordDetails, operation: &QueryOperation) -> Option<WhereArg> {
    let mut all_matching_wheres = Vec::new();

    for field in &record.fields {
        if let Field::FieldDirective(directive) = field {
            match directive {
                FieldDirective::Permissions(perm) => match perm {
                    PermissionDetails::Public => return None, // Public allows everything (no restrictions)
                    PermissionDetails::Star(where_arg) => return Some(where_arg.clone()),
                    PermissionDetails::OnOperation(ops) => {
                        for op in ops {
                            for op_type in &op.operations {
                                if *op_type == *operation {
                                    all_matching_wheres.push(op.where_.clone());
                                }
                            }
                        }
                        // Continue checking other permission directives instead of returning None
                    }
                },
                _ => {}
            }
        }
    }

    match all_matching_wheres.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        _ => Some(WhereArg::And(all_matching_wheres)),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordDetails {
    pub name: String,
    pub fields: Vec<Field>,

    pub start: Option<Location>,
    pub end: Option<Location>,

    pub start_name: Option<Location>,
    pub end_name: Option<Location>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Option<Vec<Field>>,
    pub start: Option<Location>,
    pub end: Option<Location>,

    pub start_name: Option<Location>,
    pub end_name: Option<Location>,

    pub inline_comment: Option<String>,
}

pub fn to_variant(name: &str) -> Variant {
    Variant {
        name: name.to_string(),
        fields: None,
        start: None,
        end: None,
        start_name: None,
        end_name: None,
        inline_comment: None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Column(Column),
    ColumnLines { count: usize },
    ColumnComment { text: String },
    FieldDirective(FieldDirective),
}

pub fn has_default_value(col: &Column) -> bool {
    col.directives.iter().any(|d| match d {
        ColumnDirective::Default { .. } => true,
        _ => false,
    })
}

pub fn get_primary_id_field_name(fields: &Vec<Field>) -> Option<String> {
    for field in fields.iter() {
        match field {
            Field::Column(col) => {
                if is_primary_key(col) {
                    return Some(col.name.clone());
                }
            }
            _ => {}
        }
    }
    None
}

pub fn is_field_primary_key(field_names: &Vec<String>, field: &Vec<Field>) -> bool {
    field.iter().any(|f| match f {
        Field::Column(col) => is_primary_key(col) && field_names.contains(&col.name),
        _ => false,
    })
}

pub fn is_primary_key(col: &Column) -> bool {
    col.directives
        .iter()
        .any(|d| *d == ColumnDirective::PrimaryKey)
}

pub fn to_watched_operations(record: &RecordDetails) -> Vec<QueryOperation> {
    let mut ops = Vec::new();
    for field in record.fields.iter() {
        match field {
            Field::FieldDirective(FieldDirective::Watched(details)) => {
                if details.selects {
                    ops.push(QueryOperation::Query);
                }
                if details.inserts {
                    ops.push(QueryOperation::Insert);
                }
                if details.updates {
                    ops.push(QueryOperation::Update);
                }
                if details.deletes {
                    ops.push(QueryOperation::Delete);
                }
            }
            _ => {}
        }
    }
    ops
}

pub fn get_tablename(record_name: &str, fields: &Vec<Field>) -> String {
    for field in fields.iter() {
        match field {
            Field::FieldDirective(FieldDirective::TableName((_, name))) => return name.to_string(),
            _ => {}
        }
    }

    string::pluralize(&string::decapitalize(record_name))
}

pub fn has_fieldname(field: &Field, desired_name: &str) -> bool {
    match field {
        Field::Column(Column { name, .. }) => name == desired_name,
        _ => false,
    }
}

/// Ensure that a record has an `updatedAt` DateTime field with a default value of `now` and an index.
/// If the field already exists, it's left unchanged. Otherwise, it's added to the fields.
pub fn ensure_updated_at_field(fields: &mut Vec<Field>) {
    // Check if updatedAt field already exists
    if fields.iter().any(|f| has_fieldname(f, "updatedAt")) {
        return;
    }

    // Create the updatedAt field with @index directive
    let updated_at_field = Field::Column(Column {
        name: "updatedAt".to_string(),
        type_: ColumnType::DateTime,
        nullable: false,
        directives: vec![
            ColumnDirective::Default {
                id: "now".to_string(),
                value: DefaultValue::Now,
                start: None,
                end: None,
            },
            ColumnDirective::Index,
        ],
        start: None,
        end: None,
        start_name: None,
        end_name: None,
        start_typename: None,
        end_typename: None,
        inline_comment: None,
    });

    // Add it to the fields
    fields.push(updated_at_field);
}

pub fn has_field_or_linkname(field: &Field, desired_name: &str) -> bool {
    match field {
        Field::Column(Column { name, .. }) => name == desired_name,
        Field::FieldDirective(FieldDirective::Link(link)) => link.link_name == desired_name,
        _ => false,
    }
}

pub fn has_link_named(field: &Field, desired_name: &str) -> bool {
    match field {
        Field::FieldDirective(FieldDirective::Link(link)) => link.link_name == desired_name,
        _ => false,
    }
}

pub fn is_column(field: &Field) -> bool {
    match field {
        Field::Column { .. } => true,
        _ => false,
    }
}

pub fn is_column_space(field: &Field) -> bool {
    match field {
        Field::ColumnLines { .. } => true,
        _ => false,
    }
}

pub fn link_equivalent(a: &LinkDetails, b: &LinkDetails) -> bool {
    a.local_ids == b.local_ids && a.foreign == b.foreign
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldDirective {
    Watched(WatchedDetails),
    TableName((Range, String)),
    Link(LinkDetails),
    Index(IndexDirective),
    Unique(IndexDirective),
    Permissions(PermissionDetails),
    Timestamps,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedColumn {
    pub name: String,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexDirective {
    pub columns: Vec<IndexedColumn>,
    pub where_: Option<WhereArg>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterializedIndex {
    pub unique: bool,
    pub columns: Vec<IndexedColumn>,
    pub where_: Option<WhereArg>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDetails {
    Star(WhereArg),
    OnOperation(Vec<PermissionOnOperation>),
    Public,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOnOperation {
    pub operations: Vec<QueryOperation>,
    pub where_: WhereArg,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOperations {
    pub select: Option<WhereArg>,
    pub insert: Option<WhereArg>,
    pub update: Option<WhereArg>,
    pub delete: Option<WhereArg>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatchedDetails {
    pub selects: bool,
    pub inserts: bool,
    pub updates: bool,
    pub deletes: bool,
}

pub fn link_identity(local_table: &str, link: &LinkDetails) -> String {
    format!(
        "{}_{}_{}_{}_fk",
        local_table,
        &link.local_ids.join("_"),
        link.foreign.table,
        &link.foreign.fields.join("-"),
    )
}

pub fn linked_to_unique_field(link: &LinkDetails) -> bool {
    // Fallback: check if any field is named "id" (primary keys are always unique)
    // For proper uniqueness checking, use linked_to_unique_field_with_record
    link.foreign.fields.iter().any(|f| f == "id")
}

/// Check if a link points to unique fields by examining the foreign table's schema.
/// This properly checks for UNIQUE constraints and PRIMARY KEY constraints.
pub fn linked_to_unique_field_with_record(
    link: &LinkDetails,
    foreign_record: &RecordDetails,
) -> bool {
    // If linking to a single field, check if that field has UNIQUE or PRIMARY KEY constraint
    if link.foreign.fields.len() == 1 {
        let field_name = &link.foreign.fields[0];
        for field in &foreign_record.fields {
            match field {
                Field::Column(column) => {
                    if column.name == *field_name {
                        // Check if this column has UNIQUE or PRIMARY KEY constraint
                        return column.directives.iter().any(|d| {
                            matches!(d, ColumnDirective::Unique | ColumnDirective::PrimaryKey)
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // For multi-field links, we'd need to check for composite UNIQUE constraints
    // For now, fall back to checking if all fields are "id" (which is a common pattern)
    // TODO: Support composite UNIQUE constraints
    link.foreign.fields.iter().all(|f| f == "id")
}

/// Check if a field in a record has a UNIQUE or PRIMARY KEY constraint.
pub fn field_is_unique(field_name: &str, record: &RecordDetails) -> bool {
    for field in &record.fields {
        match field {
            Field::Column(column) => {
                if column.name == field_name {
                    // Check if this column has UNIQUE or PRIMARY KEY constraint
                    return column.directives.iter().any(|d| {
                        matches!(d, ColumnDirective::Unique | ColumnDirective::PrimaryKey)
                    });
                }
            }
            _ => {}
        }
    }
    false
}

pub fn to_reciprocal(local_namespace: &str, local_table: &str, link: &LinkDetails) -> LinkDetails {
    LinkDetails {
        link_name: string::pluralize(&string::decapitalize(local_table)),
        local_ids: link.foreign.fields.clone(),

        foreign: Qualified {
            schema: local_namespace.to_string(),
            table: local_table.to_string(),
            fields: link.local_ids.clone(),
        },
        start_name: None,
        end_name: None,
        inline_comment: None,
    }
}

pub fn get_foreign_tablename(schema: &Schema, link: &LinkDetails) -> String {
    for file in schema.files.iter() {
        for definition in file.definitions.iter() {
            match definition {
                Definition::Record { name, fields, .. } => {
                    if name == &link.foreign.table {
                        return get_tablename(&link.foreign.table, fields);
                    }
                }
                _ => {}
            }
        }
    }
    link.foreign.table.clone()
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkDetails {
    pub link_name: String,
    pub local_ids: Vec<String>,

    pub foreign: Qualified,

    pub start_name: Option<Location>,
    pub end_name: Option<Location>,

    pub inline_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Qualified {
    pub schema: String,
    pub table: String,
    pub fields: Vec<String>,
}

pub fn collect_columns(fields: &Vec<Field>) -> Vec<Column> {
    let mut columns = Vec::new();
    for field in fields {
        match field {
            Field::Column(column) => columns.push(column.clone()),
            _ => {}
        }
    }
    columns
}

pub fn collect_links(fields: &Vec<Field>) -> Vec<LinkDetails> {
    let mut links = Vec::new();
    for field in fields {
        match field {
            Field::FieldDirective(FieldDirective::Link(link)) => links.push(link.clone()),
            _ => {}
        }
    }
    links
}

pub fn collect_indexes(fields: &Vec<Field>) -> Vec<MaterializedIndex> {
    let mut indexes = Vec::new();

    for field in fields {
        match field {
            Field::FieldDirective(FieldDirective::Index(details)) => {
                indexes.push(MaterializedIndex {
                    unique: false,
                    columns: details.columns.clone(),
                    where_: details.where_.clone(),
                });
            }
            Field::FieldDirective(FieldDirective::Unique(details)) => {
                indexes.push(MaterializedIndex {
                    unique: true,
                    columns: details.columns.clone(),
                    where_: details.where_.clone(),
                });
            }
            _ => {}
        }
    }

    indexes
}

pub fn with_timestamps_fields(fields: &Vec<Field>) -> Vec<Field> {
    let mut result = fields.clone();
    if !fields
        .iter()
        .any(|field| matches!(field, Field::FieldDirective(FieldDirective::Timestamps)))
    {
        return result;
    }

    result.push(Field::Column(managed_timestamp_column(
        "createdAt",
        ColumnDirective::CreatedAt,
    )));
    result.push(Field::Column(managed_timestamp_column(
        "updatedAt",
        ColumnDirective::UpdatedAt,
    )));
    result
}

fn managed_timestamp_column(name: &str, directive: ColumnDirective) -> Column {
    Column {
        name: name.to_string(),
        type_: ColumnType::DateTime,
        nullable: false,
        directives: vec![directive],
        start: None,
        end: None,
        start_name: None,
        end_name: None,
        start_typename: None,
        end_typename: None,
        inline_comment: None,
    }
}

/// Represents the type of a column in a schema
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnType {
    // Primitive types
    String,
    Int,
    Float,
    Bool,
    DateTime,
    Date,
    Json,
    JsonTyped(Box<ColumnType>),
    List(Box<ColumnType>),
    Dict(Box<ColumnType>),
    Nullable(Box<ColumnType>),

    // ID types with branding
    IdInt {
        table: String,
    },
    IdUuid {
        table: String,
    },

    // Foreign key reference (e.g., User.id)
    ForeignKey {
        table: String,
        field: String,
        serialization_type: Option<ConcreteSerializationType>,
    },

    // Custom/user-defined types
    Custom(String),
}

impl ColumnType {
    /// Convert the ColumnType to its string representation
    pub fn to_string(&self) -> String {
        match self {
            ColumnType::String => "String".to_string(),
            ColumnType::Int => "Int".to_string(),
            ColumnType::Float => "Float".to_string(),
            ColumnType::Bool => "Bool".to_string(),
            ColumnType::DateTime => "DateTime".to_string(),
            ColumnType::Date => "Date".to_string(),
            ColumnType::Json => "Json".to_string(),
            ColumnType::JsonTyped(inner) => format!("Json<{}>", inner.to_string()),
            ColumnType::List(inner) => format!("List<{}>", inner.to_string()),
            ColumnType::Dict(inner) => format!("Dict<{}>", inner.to_string()),
            ColumnType::Nullable(inner) => format!("{}?", inner.to_string()),
            ColumnType::IdInt { table } => {
                if table.is_empty() {
                    "Id.Int".to_string()
                } else {
                    format!("Id.Int<{}>", table)
                }
            }
            ColumnType::IdUuid { table } => {
                if table.is_empty() {
                    "Id.Uuid".to_string()
                } else {
                    format!("Id.Uuid<{}>", table)
                }
            }
            ColumnType::ForeignKey { table, field, .. } => format!("{}.{}", table, field),
            ColumnType::Custom(name) => name.clone(),
        }
    }

    /// Get the serialization type for this column type
    pub fn to_serialization_type(&self) -> SerializationType {
        match self {
            ColumnType::String => SerializationType::Concrete(ConcreteSerializationType::Text),
            ColumnType::Int => SerializationType::Concrete(ConcreteSerializationType::Integer),
            ColumnType::Float => SerializationType::Concrete(ConcreteSerializationType::Real),
            ColumnType::Bool => SerializationType::Concrete(ConcreteSerializationType::Integer),
            ColumnType::DateTime => {
                SerializationType::Concrete(ConcreteSerializationType::DateTime)
            }
            ColumnType::Date => SerializationType::Concrete(ConcreteSerializationType::Date),
            ColumnType::Json => SerializationType::Concrete(ConcreteSerializationType::JsonB),
            ColumnType::JsonTyped(_) => {
                SerializationType::Concrete(ConcreteSerializationType::JsonB)
            }
            ColumnType::List(_) | ColumnType::Dict(_) => {
                SerializationType::Concrete(ConcreteSerializationType::JsonB)
            }
            ColumnType::Nullable(inner) => inner.to_serialization_type(),
            ColumnType::IdInt { .. } => {
                SerializationType::Concrete(ConcreteSerializationType::IdInt)
            }
            ColumnType::IdUuid { .. } => {
                SerializationType::Concrete(ConcreteSerializationType::IdUuid)
            }
            ColumnType::ForeignKey {
                serialization_type, ..
            } => SerializationType::Concrete(
                serialization_type
                    .clone()
                    // Parsed, untypechecked foreign keys retain the historical integer fallback.
                    .unwrap_or(ConcreteSerializationType::Integer),
            ),
            ColumnType::Custom(name) => SerializationType::FromType(name.clone()),
        }
    }

    /// Returns the concrete query type for a resolved foreign key.
    pub fn query_type_string(&self) -> String {
        match self {
            ColumnType::ForeignKey {
                table,
                serialization_type: Some(ConcreteSerializationType::IdUuid),
                ..
            } => format!("Id.Uuid<{}>", table),
            ColumnType::ForeignKey {
                serialization_type: Some(ConcreteSerializationType::Text),
                ..
            } => "String".to_string(),
            ColumnType::ForeignKey {
                serialization_type: Some(ConcreteSerializationType::Real),
                ..
            } => "Float".to_string(),
            ColumnType::ForeignKey {
                serialization_type: Some(ConcreteSerializationType::Date),
                ..
            } => "Date".to_string(),
            ColumnType::ForeignKey {
                serialization_type: Some(ConcreteSerializationType::DateTime),
                ..
            } => "DateTime".to_string(),
            _ => self.to_string(),
        }
    }

    /// Check if this type contains a dot (for foreign key detection during parsing)
    pub fn contains_dot(&self) -> bool {
        match self {
            ColumnType::ForeignKey { .. } => true,
            ColumnType::JsonTyped(inner)
            | ColumnType::List(inner)
            | ColumnType::Dict(inner)
            | ColumnType::Nullable(inner) => inner.contains_dot(),
            _ => false,
        }
    }

    /// Check if this is an ID type
    pub fn is_id_type(&self) -> bool {
        matches!(self, ColumnType::IdInt { .. } | ColumnType::IdUuid { .. })
    }

    /// Get the table name for ID types or foreign keys
    pub fn table_name(&self) -> Option<&str> {
        match self {
            ColumnType::IdInt { table } | ColumnType::IdUuid { table } => {
                if table.is_empty() {
                    None
                } else {
                    Some(table)
                }
            }
            ColumnType::ForeignKey { table, .. } => Some(table),
            ColumnType::Nullable(inner) => inner.table_name(),
            _ => None,
        }
    }

    /// Get the name of a custom type for looking up in the type context.
    /// Returns Some(&name) for Custom types, None for built-in types.
    pub fn get_custom_type_name(&self) -> Option<&str> {
        match self {
            ColumnType::Custom(name) => Some(name),
            ColumnType::Nullable(inner) => inner.get_custom_type_name(),
            _ => None,
        }
    }

    pub fn collect_custom_type_names(&self, names: &mut Vec<String>) {
        match self {
            ColumnType::Custom(name) => names.push(name.clone()),
            ColumnType::JsonTyped(inner)
            | ColumnType::List(inner)
            | ColumnType::Dict(inner)
            | ColumnType::Nullable(inner) => inner.collect_custom_type_names(names),
            _ => {}
        }
    }

    pub fn is_document_container(&self) -> bool {
        matches!(self, ColumnType::List(_) | ColumnType::Dict(_))
    }

    pub fn contains_document_container(&self) -> bool {
        match self {
            ColumnType::List(_) | ColumnType::Dict(_) => true,
            ColumnType::JsonTyped(inner) | ColumnType::Nullable(inner) => {
                inner.contains_document_container()
            }
            _ => false,
        }
    }

    pub fn is_typed_json(&self) -> bool {
        matches!(self, ColumnType::JsonTyped(_))
    }

    pub fn is_json_like(&self) -> bool {
        matches!(self, ColumnType::Json | ColumnType::JsonTyped(_))
    }

    fn split_generic(type_str: &str) -> Option<(&str, &str)> {
        let mut start_index = None;
        let mut depth = 0usize;

        for (index, ch) in type_str.char_indices() {
            match ch {
                '<' => {
                    if start_index.is_none() {
                        start_index = Some(index);
                    }
                    depth += 1;
                }
                '>' => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                    if depth == 0 {
                        let start = start_index?;
                        if index != type_str.len() - 1 {
                            return None;
                        }
                        return Some((&type_str[..start], &type_str[start + 1..index]));
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Check if this is a Bool type
    pub fn is_bool(&self) -> bool {
        matches!(self, ColumnType::Bool)
    }

    /// Parse a type string into a ColumnType.
    /// Handles primitive types, ID types, foreign key references, and custom types.
    pub fn from_str(type_str: &str) -> Self {
        if let Some(inner) = type_str.strip_suffix('?') {
            return ColumnType::Nullable(Box::new(ColumnType::from_str(inner)));
        }

        match type_str {
            "String" => ColumnType::String,
            "Int" => ColumnType::Int,
            "Float" => ColumnType::Float,
            "Bool" => ColumnType::Bool,
            "DateTime" => ColumnType::DateTime,
            "Date" => ColumnType::Date,
            "JSON" | "Json" => ColumnType::Json,
            "Id.Int" => ColumnType::IdInt {
                table: String::new(),
            },
            "Id.Uuid" => ColumnType::IdUuid {
                table: String::new(),
            },
            _ => {
                if let Some(table) = type_str
                    .strip_prefix("Id.Int<")
                    .and_then(|value| value.strip_suffix('>'))
                {
                    return ColumnType::IdInt {
                        table: table.to_string(),
                    };
                }

                if let Some(table) = type_str
                    .strip_prefix("Id.Uuid<")
                    .and_then(|value| value.strip_suffix('>'))
                {
                    return ColumnType::IdUuid {
                        table: table.to_string(),
                    };
                }

                if let Some((name, inner)) = ColumnType::split_generic(type_str) {
                    let inner_type = ColumnType::from_str(inner);
                    return match name {
                        "Json" => ColumnType::JsonTyped(Box::new(inner_type)),
                        "List" => ColumnType::List(Box::new(inner_type)),
                        "Dict" => ColumnType::Dict(Box::new(inner_type)),
                        _ => ColumnType::Custom(type_str.to_string()),
                    };
                }

                // Check for foreign key reference (TableName.fieldName pattern)
                if type_str.contains('.') && !type_str.starts_with("Id.") {
                    let parts: Vec<&str> = type_str.split('.').collect();
                    if parts.len() == 2 {
                        ColumnType::ForeignKey {
                            table: parts[0].to_string(),
                            field: parts[1].to_string(),
                            serialization_type: None,
                        }
                    } else {
                        ColumnType::Custom(type_str.to_string())
                    }
                } else {
                    ColumnType::Custom(type_str.to_string())
                }
            }
        }
    }
}

impl std::fmt::Display for ColumnType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColumnType::String => write!(f, "String"),
            ColumnType::Int => write!(f, "Int"),
            ColumnType::Float => write!(f, "Float"),
            ColumnType::Bool => write!(f, "Bool"),
            ColumnType::DateTime => write!(f, "DateTime"),
            ColumnType::Date => write!(f, "Date"),
            ColumnType::Json => write!(f, "Json"),
            ColumnType::JsonTyped(inner) => write!(f, "Json<{}>", inner),
            ColumnType::List(inner) => write!(f, "List<{}>", inner),
            ColumnType::Dict(inner) => write!(f, "Dict<{}>", inner),
            ColumnType::Nullable(inner) => write!(f, "{}?", inner),
            ColumnType::IdInt { table } => {
                if table.is_empty() {
                    write!(f, "Id.Int")
                } else {
                    write!(f, "Id.Int<{}>", table)
                }
            }
            ColumnType::IdUuid { table } => {
                if table.is_empty() {
                    write!(f, "Id.Uuid")
                } else {
                    write!(f, "Id.Uuid<{}>", table)
                }
            }
            ColumnType::ForeignKey { table, field, .. } => write!(f, "{}.{}", table, field),
            ColumnType::Custom(name) => write!(f, "{}", name),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub type_: ColumnType,
    pub nullable: bool,
    pub directives: Vec<ColumnDirective>,

    pub start: Option<Location>,
    pub end: Option<Location>,

    pub start_name: Option<Location>,
    pub end_name: Option<Location>,

    pub start_typename: Option<Location>,
    pub end_typename: Option<Location>,

    pub inline_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub offset: usize,
    pub line: u32,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Range {
    pub start: Location,
    pub end: Location,
}

pub fn empty_range() -> Range {
    Range {
        start: Location {
            offset: 0,
            line: 0,
            column: 0,
        },
        end: Location {
            offset: 0,
            line: 0,
            column: 0,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnDirective {
    PrimaryKey,
    Unique,
    Index,
    CreatedAt,
    UpdatedAt,
    Default {
        id: String,
        value: DefaultValue,
        start: Option<Location>,
        end: Option<Location>,
    },
    // Check(String),
}

pub fn is_created_at(col: &Column) -> bool {
    col.directives
        .iter()
        .any(|directive| matches!(directive, ColumnDirective::CreatedAt))
}

pub fn is_updated_at(col: &Column) -> bool {
    col.directives
        .iter()
        .any(|directive| matches!(directive, ColumnDirective::UpdatedAt))
}

pub fn is_managed_timestamp(col: &Column) -> bool {
    is_created_at(col) || is_updated_at(col)
}

pub fn is_integer_primary_key(col: &Column) -> bool {
    is_primary_key(col)
        && matches!(
            col.type_,
            ColumnType::Int | ColumnType::IdInt { .. } | ColumnType::ForeignKey { .. }
        )
}

// CURRENT_TIME, CURRENT_DATE or CURRENT_TIMESTAMP
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DefaultValue {
    Now,
    Value(QueryValue),
}

// https://sqlite.org/datatype3.html
// NULL. The value is a NULL value.
// INTEGER. The value is a signed integer, stored in 0, 1, 2, 3, 4, 6, or 8 bytes depending on the magnitude of the value.
// REAL. The value is a floating point value, stored as an 8-byte IEEE floating point number.
// TEXT. The value is a text string, stored using the database encoding (UTF-8, UTF-16BE or UTF-16LE).
// BLOB. The value is a blob of data, stored exactly as it was input.
#[derive(Debug, Clone, PartialEq)]
pub enum SerializationType {
    Concrete(ConcreteSerializationType),
    FromType(String), // defined as another named type.
}

impl SerializationType {
    pub fn into_concrete(self) -> Option<ConcreteSerializationType> {
        match self {
            SerializationType::Concrete(type_) => Some(type_),
            SerializationType::FromType(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConcreteSerializationType {
    Integer,
    Real,
    Text,
    Blob,
    Date,     // stored as a string
    DateTime, // stored as unix epoch integer
    VectorBlob {
        vector_type: VectorType,
        dimensionality: u8,
    },
    JsonB,  // This is a blob, but we know it's valid json
    IdInt,  // Branded integer ID type
    IdUuid, // Branded UUID ID type (stored as TEXT)
}

impl ConcreteSerializationType {
    pub fn to_sql_type(&self) -> String {
        match self {
            ConcreteSerializationType::Integer => "INTEGER".to_string(),
            ConcreteSerializationType::Real => "REAL".to_string(),
            ConcreteSerializationType::Text => "TEXT".to_string(),
            ConcreteSerializationType::Blob => "BLOB".to_string(),
            ConcreteSerializationType::Date => "TEXT".to_string(), // Dates stored as strings
            ConcreteSerializationType::DateTime => "INTEGER".to_string(), // DateTime as unix epoch
            ConcreteSerializationType::VectorBlob { .. } => "BLOB".to_string(),
            ConcreteSerializationType::JsonB => "BLOB".to_string(),
            ConcreteSerializationType::IdInt => "INTEGER".to_string(),
            ConcreteSerializationType::IdUuid => "TEXT".to_string(),
        }
    }
}

// Taken from:
// https://docs.turso.tech/features/ai-and-embeddings#types
#[derive(Debug, Clone, PartialEq)]
pub enum VectorType {
    Float64,
    Float32,
    Float16,
    BFloat16,
    Float8,
    Float1,
}

// Queries
//
#[derive(Debug, Clone)]
pub struct QueryList {
    pub queries: Vec<QueryDef>,
}

#[derive(Debug, Clone)]
pub enum QueryDef {
    Query(Query),
    QueryComment { text: String },
    QueryLines { count: usize },
}

#[derive(Debug, Clone)]
pub struct Query {
    pub interface_hash: String,
    pub full_hash: String,

    pub operation: QueryOperation,
    pub name: String,
    pub args: Vec<QueryParamDefinition>,
    pub fields: Vec<TopLevelQueryField>,

    pub start: Option<Location>,
    pub end: Option<Location>,
}

// This is the first layer of fields in a query
//
#[derive(Debug, Clone)]
pub enum TopLevelQueryField {
    Field(QueryField),
    Lines { count: usize },
    Comment { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryOperation {
    Query,
    Insert,
    Update,
    Delete,
}

impl QueryOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueryOperation::Query => "query",
            QueryOperation::Insert => "insert",
            QueryOperation::Update => "update",
            QueryOperation::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueryParamDefinition {
    pub name: String,
    pub type_: Option<String>,
    pub nullable: bool,
    pub omittable: bool,
    pub start_name: Option<Location>,
    pub end_name: Option<Location>,

    pub start_type: Option<Location>,
    pub end_type: Option<Location>,
}

pub fn to_typescript_type(type_: &str) -> String {
    match type_ {
        "String" => "\"string\"".to_string(),
        "Int" => "\"number\"".to_string(),
        "Bool" => "\"bool\"".to_string(),
        "DateTime" => "\"date\"".to_string(),
        _ => type_.to_string(),
    }
}

// The "Select Alias" is the value that is in the JSON payload
// It's used for rectangle returns though.
// For the form is qualified like {table_alias}__{field_alias}
// This isn't currently used for nested returns (which is what is used)
pub fn get_select_alias(table_alias: &str, query_field: &QueryField) -> String {
    let field_alias = get_aliased_name(query_field);

    format!("{}__{}", table_alias, field_alias)
}

//
pub fn get_aliased_name(field: &QueryField) -> String {
    match &field.alias {
        Some(alias) => alias.to_string(),
        None => field.name.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct QueryField {
    pub name: String,
    pub alias: Option<String>,
    pub set: Option<QueryValue>,
    pub directives: Vec<String>,
    pub fields: Vec<ArgField>,

    pub start_fieldname: Option<Location>,
    pub end_fieldname: Option<Location>,

    pub start: Option<Location>,
    pub end: Option<Location>,
}

#[derive(Debug, Clone)]
pub enum ArgField {
    Field(QueryField),
    Arg(LocatedArg),
    Lines { count: usize },
    QueryComment { text: String },
}

#[derive(Debug, Clone)]
pub struct LocatedArg {
    pub arg: Arg,
    pub start: Option<Location>,
    pub end: Option<Location>,
}

pub fn collect_query_fields(fields: &Vec<ArgField>) -> Vec<&QueryField> {
    let mut args = Vec::new();
    for field in fields {
        match field {
            ArgField::Field(arg) => args.push(arg),
            _ => {}
        }
    }
    args
}

pub fn collect_wheres(fields: &Vec<ArgField>) -> Vec<WhereArg> {
    let mut wheres = Vec::new();
    for field in fields {
        if let ArgField::Arg(arg) = field {
            if let Arg::Where(where_arg) = &arg.arg {
                wheres.push(where_arg.clone());
            }
        }
    }
    wheres
}

#[derive(Debug, Clone)]
pub enum Arg {
    Limit(QueryValue),
    OrderBy(Direction, String),
    Where(WhereArg),
}

#[derive(Debug, Clone)]
pub enum Direction {
    Asc,
    Desc,
}

pub fn direction_to_string(direction: &Direction) -> String {
    match direction {
        Direction::Asc => "Asc".to_string(),
        Direction::Desc => "Desc".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WhereArg {
    Column(bool, String, Operator, QueryValue, Range), // bool indicates if column is from session, String is field name without Session. prefix, Range is the location of the field name (including "Session." if present)
    And(Vec<WhereArg>),
    Or(Vec<WhereArg>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueryValue {
    Fn(FnDetails),

    LiteralTypeValue((Range, LiteralTypeValueDetails)),
    Variable((Range, VariableDetails)),
    String((Range, String)),
    Int((Range, i32)),
    Float((Range, f32)),
    Bool((Range, bool)),
    Null(Range),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiteralTypeValueDetails {
    pub name: String,
    // Union variant field assignments (e.g., for Create { name = $name, description = $description })
    pub fields: Option<Vec<(String, QueryValue)>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FnDetails {
    pub name: String,
    pub args: Vec<QueryValue>,

    pub location: Range,
    pub location_fn_name: Range,
    pub location_arg: Range,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariableDetails {
    pub name: String,
    pub session_field: Option<String>,
}

pub fn to_pyre_variable_name(var: &VariableDetails) -> String {
    match &var.session_field {
        Some(session_field) => format!("Session.{}", session_field),
        None => format!("${}", var.name),
    }
}

pub fn session_field_name(col: &Column) -> String {
    // field.name.to_string()
    format!("Session.{}", col.name)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operator {
    Equal,
    NotEqual,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    In,
    NotIn,
    Like,
    NotLike,
}

/// Post-processing pass to resolve ID brands for columns.
///
/// This function:
/// 1. Sets the brand on ID columns (IdInt or IdUuid) with no brand to the record name
/// 2. For foreign key references (type like "User.id"), resolves the brand from the referenced column
pub fn resolve_id_brands(database: &mut Database) {
    // Build a lookup table of table name -> record definition
    let mut table_lookup: std::collections::HashMap<String, RecordDetails> =
        std::collections::HashMap::new();

    for schema in database.schemas.iter() {
        for file in schema.files.iter() {
            for definition in file.definitions.iter() {
                if let Definition::Record { name, fields, .. } = definition {
                    table_lookup.insert(
                        name.clone(),
                        RecordDetails {
                            name: name.clone(),
                            fields: fields.clone(),
                            start: None,
                            end: None,
                            start_name: None,
                            end_name: None,
                        },
                    );
                }
            }
        }
    }

    // Now iterate through all records and their columns to set brands
    let table_lookup_ref = &table_lookup; // Immutable borrow for lookup

    for schema in database.schemas.iter_mut() {
        for file in schema.files.iter_mut() {
            for definition in file.definitions.iter_mut() {
                if let Definition::Record {
                    name: record_name,
                    fields,
                    ..
                } = definition
                {
                    for field in fields.iter_mut() {
                        if let Field::Column(ref mut column) = field {
                            // Check if this is an ID type without a brand
                            if let ColumnType::IdInt { table } | ColumnType::IdUuid { table } =
                                &mut column.type_
                            {
                                if table.is_empty() {
                                    // Set brand to the record name for ID types
                                    *table = record_name.clone();
                                }
                            } else if let ColumnType::ForeignKey { table, field, .. } =
                                &column.type_
                            {
                                // This is a foreign key reference like "User.id"
                                // The brand should already be set during parsing, but let's resolve any additional info if needed
                                let ref_table = table.clone();
                                let ref_field = field.clone();

                                if let Some(ref_record) = table_lookup_ref.get(&ref_table) {
                                    // Look up the referenced column to get its brand
                                    for ref_field_def in ref_record.fields.iter() {
                                        if let Field::Column(ref_col) = ref_field_def {
                                            if ref_col.name == ref_field {
                                                // Check if the referenced column is an ID type with a brand
                                                if let ColumnType::IdInt { table: ref_brand }
                                                | ColumnType::IdUuid { table: ref_brand } =
                                                    &ref_col.type_
                                                {
                                                    // Preserve the referenced ID's brand when set and
                                                    // always retain its concrete storage representation.
                                                    column.type_ = ColumnType::ForeignKey {
                                                        table: if ref_brand.is_empty() {
                                                            ref_table.clone()
                                                        } else {
                                                            ref_brand.clone()
                                                        },
                                                        field: ref_field.clone(),
                                                        serialization_type: ref_col
                                                            .type_
                                                            .to_serialization_type()
                                                            .into_concrete(),
                                                    };
                                                }
                                                break;
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
