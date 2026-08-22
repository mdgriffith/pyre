use crate::ast;
use crate::error;
use crate::parser;
use crate::typecheck;
use serde;
use serde::{Deserialize, Serialize};

pub mod to_schema;

pub const MIGRATION_TABLE: &str = "_pyre_migrations";

// List all tables
// Returns list of string
pub const LIST_TABLES: &str = "SELECT name FROM sqlite_master WHERE type='table';";

pub const LIST_MIGRATIONS: &str = "SELECT name FROM _pyre_migrations;";

// Add this near the top with other constants
pub const GET_SCHEMA: &str = "SELECT schema FROM _pyre_migrations WHERE finished_at IS NOT NULL AND error IS NULL AND schema IS NOT NULL ORDER BY id DESC LIMIT 1;";

pub const IS_INITIALIZED: &str = r#"
SELECT 
  CASE 
    WHEN EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='_pyre_migrations')
    THEN 1
    ELSE 0
  END as is_initialized
"#;

// This produces a JSON object that can be decoded into an `IntrospectionRaw`
pub const INTROSPECT_SQL: &str = r#"
WITH RECURSIVE
  -- Get all tables except system tables
  all_tables AS (
    SELECT name 
    FROM sqlite_master 
    WHERE type='table' 
    AND name NOT IN ('sqlite_sequence', '_pyre_migrations', '_pyre_sync')
  ),
  -- Get table info for each table
  table_info AS (
    SELECT 
      t.name as table_name,
      jsonb_group_array(
        jsonb_object(
          'cid', c.cid,
          'name', c.name,
          'type', c.type,
          'notnull', c.'notnull',
          'dflt_value', c.dflt_value,
          'pk', c.pk
        )
      ) as columns_json
    FROM all_tables t
    CROSS JOIN pragma_table_info(t.name) c
    GROUP BY t.name
  ),
  -- Get foreign key info for each table
  foreign_keys AS (
    SELECT 
      t.name as table_name,
      jsonb_group_array(
        jsonb_object(
          'id', f.id,
          'seq', f.seq,
          'table', f.'table',
          'from', f.'from',
          'to', f.'to',
          'on_update', f.on_update,
          'on_delete', f.on_delete,
          'match', f.'match'
        )
      ) as fks_json
    FROM all_tables t
    CROSS JOIN pragma_foreign_key_list(t.name) f
    GROUP BY t.name
  ),
  -- Get index columns for each table/index
  index_columns AS (
    SELECT
      t.name as table_name,
      il.name as index_name,
      il.'unique' as is_unique,
      ix.seqno as seqno,
      CASE
        WHEN ix.name IS NULL AND il.name = 'uniq_' || t.name || '_singleton' THEN '1'
        ELSE ix.name
      END as column_name,
      ix.desc as is_desc,
      CASE
        WHEN ix.name IS NULL AND il.name = 'uniq_' || t.name || '_singleton' THEN 1
        ELSE 0
      END as is_expression
    FROM all_tables t
    CROSS JOIN pragma_index_list(t.name) il
    CROSS JOIN pragma_index_xinfo(il.name) ix
    WHERE il.origin != 'pk'
      AND ix.key = 1
      AND (ix.name IS NOT NULL OR il.name = 'uniq_' || t.name || '_singleton')
  ),
  -- Group columns for each index
  index_defs AS (
    SELECT
      ic.table_name as table_name,
      ic.index_name as index_name,
      max(ic.is_unique) as is_unique,
      jsonb_group_array(
        jsonb_object(
          'name', ic.column_name,
          'desc', CASE WHEN ic.is_desc = 1 THEN jsonb('true') ELSE jsonb('false') END,
          'expression', CASE WHEN ic.is_expression = 1 THEN jsonb('true') ELSE jsonb('false') END
        )
      ) as columns_json
    FROM index_columns ic
    GROUP BY ic.table_name, ic.index_name
  ),
  -- Get index metadata including optional partial WHERE clause
  indexes AS (
    SELECT
      d.table_name as table_name,
      jsonb_group_array(
        jsonb_object(
          'name', d.index_name,
          'unique', CASE WHEN d.is_unique = 1 THEN jsonb('true') ELSE jsonb('false') END,
          'columns', jsonb(d.columns_json),
          'where_clause',
            CASE
              WHEN sm.sql IS NOT NULL AND instr(lower(sm.sql), ' where ') > 0
                THEN trim(substr(sm.sql, instr(lower(sm.sql), ' where ') + 7))
              ELSE NULL
            END
        )
      ) as indexes_json
    FROM index_defs d
    LEFT JOIN sqlite_master sm ON sm.type='index' AND sm.name = d.index_name
    GROUP BY d.table_name
  ),
  -- Get migration state
  migration_state AS (
    SELECT 
      CASE 
        WHEN EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='_pyre_migrations') THEN
          jsonb_object(
            'MigrationTable',
            jsonb_object(
              'migrations',
              jsonb_group_array(
                jsonb_object('name', name)
              )
            )
          )
        ELSE jsonb('{"NoMigrationTable": null}')
      END as state_json
    FROM _pyre_migrations
  )
SELECT json_object(
  'tables',
  jsonb_group_array(
    jsonb_object(
      'name', ti.table_name,
      'columns', jsonb(ti.columns_json),
      'foreign_keys', COALESCE(jsonb(fk.fks_json), jsonb('[]')),
      'indexes', COALESCE(jsonb(ix.indexes_json), jsonb('[]'))
    )
  ),
  'migration_state', json((SELECT state_json FROM migration_state)),
  'schema_source', COALESCE(
    (SELECT schema FROM _pyre_migrations WHERE finished_at IS NOT NULL AND error IS NULL AND schema IS NOT NULL ORDER BY id DESC LIMIT 1),
    ''
  ),
  'links', jsonb('[]')
) as result
FROM table_info ti
LEFT JOIN foreign_keys fk ON ti.table_name = fk.table_name
LEFT JOIN indexes ix ON ti.table_name = ix.table_name;
"#;

// This is the same sql as INTROSPECT_SQL but does not query
// the migration table or schema table.
// This is used to introspect an uninitialized database.
pub const INTROSPECT_UNINITIALIZED_SQL: &str = r#"
WITH RECURSIVE
  -- Get all tables except system tables
  all_tables AS (
    SELECT name 
    FROM sqlite_master 
    WHERE type='table' 
    AND name NOT IN ('sqlite_sequence', '_pyre_migrations', '_pyre_sync')
  ),
  -- Get table info for each table
  table_info AS (
    SELECT 
      t.name as table_name,
      json_group_array(
        json_object(
          'cid', c.cid,
          'name', c.name,
          'type', c.type,
          'notnull', c.'notnull',
          'dflt_value', c.dflt_value,
          'pk', c.pk
        )
      ) as columns_json
    FROM all_tables t
    CROSS JOIN pragma_table_info(t.name) c
    GROUP BY t.name
  ),
  -- Get foreign key info for each table
  foreign_keys AS (
    SELECT 
      t.name as table_name,
      json_group_array(
        json_object(
          'id', f.id,
          'seq', f.seq,
          'table', f.'table',
          'from', f.'from',
          'to', f.'to',
          'on_update', f.on_update,
          'on_delete', f.on_delete,
          'match', f.'match'
        )
      ) as fks_json
    FROM all_tables t
    CROSS JOIN pragma_foreign_key_list(t.name) f
    GROUP BY t.name
  ),
  -- Get index columns for each table/index
  index_columns AS (
    SELECT
      t.name as table_name,
      il.name as index_name,
      il.'unique' as is_unique,
      ix.seqno as seqno,
      CASE
        WHEN ix.name IS NULL AND il.name = 'uniq_' || t.name || '_singleton' THEN '1'
        ELSE ix.name
      END as column_name,
      ix.desc as is_desc,
      CASE
        WHEN ix.name IS NULL AND il.name = 'uniq_' || t.name || '_singleton' THEN 1
        ELSE 0
      END as is_expression
    FROM all_tables t
    CROSS JOIN pragma_index_list(t.name) il
    CROSS JOIN pragma_index_xinfo(il.name) ix
    WHERE il.origin != 'pk'
      AND ix.key = 1
      AND (ix.name IS NOT NULL OR il.name = 'uniq_' || t.name || '_singleton')
  ),
  -- Group columns for each index
  index_defs AS (
    SELECT
      ic.table_name as table_name,
      ic.index_name as index_name,
      max(ic.is_unique) as is_unique,
      json_group_array(
        json_object(
          'name', ic.column_name,
          'desc', CASE WHEN ic.is_desc = 1 THEN json('true') ELSE json('false') END,
          'expression', CASE WHEN ic.is_expression = 1 THEN json('true') ELSE json('false') END
        )
      ) as columns_json
    FROM index_columns ic
    GROUP BY ic.table_name, ic.index_name
  ),
  -- Get index metadata including optional partial WHERE clause
  indexes AS (
    SELECT
      d.table_name as table_name,
      json_group_array(
        json_object(
          'name', d.index_name,
          'unique', CASE WHEN d.is_unique = 1 THEN json('true') ELSE json('false') END,
          'columns', json(d.columns_json),
          'where_clause',
            CASE
              WHEN sm.sql IS NOT NULL AND instr(lower(sm.sql), ' where ') > 0
                THEN trim(substr(sm.sql, instr(lower(sm.sql), ' where ') + 7))
              ELSE NULL
            END
        )
      ) as indexes_json
    FROM index_defs d
    LEFT JOIN sqlite_master sm ON sm.type='index' AND sm.name = d.index_name
    GROUP BY d.table_name
  )
SELECT json_object(
  'tables',
  json_group_array(
    json_object(
      'name', ti.table_name,
      'columns', json(ti.columns_json),
      'foreign_keys', COALESCE(json(fk.fks_json), json('[]')),
      'indexes', COALESCE(json(ix.indexes_json), json('[]'))
    )
  ),
  'migration_state', json('{"NoMigrationTable": null}'),
  'schema_source', '',
  'links', json('[]')
) as result
FROM table_info ti
LEFT JOIN foreign_keys fk ON ti.table_name = fk.table_name
LEFT JOIN indexes ix ON ti.table_name = ix.table_name;
"#;

/*
Introspection is used to drive migrations.


First introspection is run.

Then we diff `Introspection` with `Schema` which produces a `Diff`, which can be turned into a
Migration SQL.


*/

#[derive(Debug, Serialize, Deserialize)]
pub struct Migration {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MigrationState {
    NoMigrationTable,
    MigrationTable { migrations: Vec<Migration> },
}

#[derive(Debug)]
pub enum SchemaResult {
    FailedToParse {
        source: String,
        errors: Vec<error::Error>,
    },
    FailedToTypecheck {
        schema: ast::Schema,
        errors: Vec<error::Error>,
    },
    Success {
        schema: ast::Schema,
        context: typecheck::Context,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Link {
    pub table_name: String,
    pub field_name: String,
    pub foreign_key_field: String,
    pub related_table: String,
    pub link_type: String, // "many-to-one" or "one-to-many"
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IntrospectionRaw {
    pub tables: Vec<Table>,
    pub migration_state: MigrationState,
    pub schema_source: String,
    pub links: Vec<Link>,
}

#[derive(Debug)]
pub struct Introspection {
    pub tables: Vec<Table>,
    pub migration_state: MigrationState,
    pub schema: SchemaResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedColumnInfo {
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_boolish")]
    pub desc: bool,
    #[serde(default, deserialize_with = "deserialize_boolish")]
    pub expression: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_boolish")]
    pub unique: bool,
    #[serde(default)]
    pub columns: Vec<IndexedColumnInfo>,
    #[serde(default)]
    pub where_clause: Option<String>,
}

// Intermediates

#[derive(Debug, Serialize, Deserialize)]
pub struct DbTable {
    pub name: String,
}

/*



// [
//   {
//     "id": "0",
//     "seq": "0",
//     "table": "rulebooks",
//     "from": "rulebookId",
//     "to": "id",
//     "on_update": "CASCADE",
//     "on_delete": "CASCADE",
//     "match": "NONE"
//   },
//   {
//     "id": "1",
//     "seq": "0",
//     "table": "users",
//     "from": "userId",
//     "to": "id",
//     "on_update": "CASCADE",
//     "on_delete": "CASCADE",
//     "match": "NONE"
//   }
// ]


*/
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(try_from = "String")]
pub enum ForeignKeyAction {
    #[serde(rename = "CASCADE")]
    Cascade,
    #[serde(rename = "RESTRICT")]
    Restrict,
    #[serde(rename = "NO ACTION")]
    NoAction,
    #[serde(rename = "SET NULL")]
    SetNull,
    #[serde(rename = "SET DEFAULT")]
    SetDefault,
}

impl TryFrom<String> for ForeignKeyAction {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "CASCADE" => Ok(ForeignKeyAction::Cascade),
            "RESTRICT" => Ok(ForeignKeyAction::Restrict),
            "NO ACTION" => Ok(ForeignKeyAction::NoAction),
            "SET NULL" => Ok(ForeignKeyAction::SetNull),
            "SET DEFAULT" => Ok(ForeignKeyAction::SetDefault),
            _ => Err(format!("Unknown foreign key action: {}", value)),
        }
    }
}

/// Specifies how NULL values in foreign keys are handled during constraint checking.
/// Note: In current SQLite versions, this is effectively a no-op as only SIMPLE
/// matching behavior is implemented, regardless of the specified value.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(try_from = "String")]
pub enum ForeignKeyMatch {
    /// SIMPLE matching allows a foreign key to be NULL unless the parent key is a composite key
    /// and only some columns of the foreign key are NULL. If the foreign key is a composite key
    /// and any column is NULL, then all columns must be NULL for the constraint to be satisfied.
    Simple,

    /// FULL matching requires that either all or none of the foreign key columns be NULL.
    /// If any foreign key column is NULL, then all columns must be NULL for the constraint
    /// to be satisfied. If all foreign key columns are non-NULL, they must match a parent key.
    Full,

    /// NONE matching allows any column in the foreign key to be NULL, regardless of whether
    /// other columns in the foreign key are NULL or not. This is the default behavior if
    /// no MATCH clause is specified.
    None,
}

impl TryFrom<String> for ForeignKeyMatch {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "SIMPLE" => Ok(ForeignKeyMatch::Simple),
            "FULL" => Ok(ForeignKeyMatch::Full),
            "NONE" => Ok(ForeignKeyMatch::None),
            _ => Err(format!("Unknown foreign key match: {}", value)),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ForeignKey {
    pub id: usize,
    pub seq: usize,
    pub table: String,
    pub from: String,
    pub to: String,
    pub on_update: ForeignKeyAction,
    pub on_delete: ForeignKeyAction,
    #[serde(rename = "match")]
    pub match_: ForeignKeyMatch,
}

// [
//   {
//     "cid": "0",
//     "name": "id",
//     "type": "INTEGER",
//     "notnull": "1",
//     "dflt_value": null,
//     "pk": "1"
//   },
//   {
//     "cid": "1",
//     "name": "createdAt",
//     "type": "DATETIME",
//     "notnull": "1",
//     "dflt_value": "CURRENT_TIMESTAMP",
//     "pk": "0"
//   },
// ]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ColumnInfo {
    pub cid: usize,
    pub name: String,
    #[serde(rename = "type")]
    pub column_type: String,
    #[serde(deserialize_with = "deserialize_notnull")]
    pub notnull: bool,

    #[serde(rename = "dflt_value")]
    pub default_value: Option<String>,

    #[serde(deserialize_with = "deserialize_notnull")]
    pub pk: bool,

    #[serde(default)]
    pub indexed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MigrationRun {
    pub name: String,
}

fn deserialize_notnull<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let i: i32 = serde::Deserialize::deserialize(deserializer)?;
    match i {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(serde::de::Error::custom("unexpected value")),
    }
}

fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Boolish {
        Bool(bool),
        Int(i64),
        Str(String),
    }

    match Boolish::deserialize(deserializer)? {
        Boolish::Bool(value) => Ok(value),
        Boolish::Int(value) => Ok(value != 0),
        Boolish::Str(value) => {
            let lowered = value.to_ascii_lowercase();
            match lowered.as_str() {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(serde::de::Error::custom("unexpected boolean-ish value")),
            }
        }
    }
}

pub fn from_raw(mut raw: IntrospectionRaw) -> Introspection {
    if raw.schema_source.is_empty() {
        let context = typecheck::empty_context();
        return Introspection {
            tables: raw.tables,
            migration_state: raw.migration_state,
            schema: SchemaResult::Success {
                schema: ast::Schema::default(),
                context,
            },
        };
    }

    let mut schema = ast::Schema {
        namespace: ast::DEFAULT_SCHEMANAME.to_string(),
        sync_mode: ast::SyncMode::Synced,
        session: None,
        files: vec![],
    };

    // Attempt to parse the schema source
    let schema_result = match parser::run("schema.pyre", &raw.schema_source, &mut schema) {
        Ok(()) => {
            if schema.namespace == ast::DEFAULT_SCHEMANAME {
                if let Some(namespace) = infer_stored_schema_namespace(&schema) {
                    schema.namespace = namespace;
                }
            }

            // Create a Database from the schema
            let database = ast::Database {
                schemas: vec![schema.clone()],
            };

            // Typecheck the schema
            match typecheck::check_schema(&database) {
                Ok(context) => {
                    // Extract links from parsed schema
                    raw.links = extract_links(&schema, &raw.tables);
                    SchemaResult::Success { schema, context }
                }
                Err(errors) => SchemaResult::FailedToTypecheck { schema, errors },
            }
        }
        Err(err) => {
            let source = raw.schema_source.clone();
            if let Some(parsing_error) = parser::convert_parsing_error(err) {
                SchemaResult::FailedToParse {
                    source,
                    errors: vec![parsing_error],
                }
            } else {
                SchemaResult::FailedToParse {
                    source,
                    errors: vec![error::Error {
                        error_type: error::ErrorType::ParsingError(error::ParsingErrorDetails {
                            expecting: error::Expecting::PyreFile,
                        }),
                        filepath: "schema.pyre".to_string(),
                        locations: vec![],
                    }],
                }
            }
        }
    };

    Introspection {
        tables: raw.tables,
        migration_state: raw.migration_state,
        schema: schema_result,
    }
}

fn infer_stored_schema_namespace(schema: &ast::Schema) -> Option<String> {
    let mut namespaces = std::collections::HashSet::new();

    for file in &schema.files {
        for definition in &file.definitions {
            if let ast::Definition::Record { fields, .. } = definition {
                for link in ast::collect_links(fields) {
                    if link.foreign.schema != ast::DEFAULT_SCHEMANAME {
                        namespaces.insert(link.foreign.schema);
                    }
                }
            }
        }
    }

    if namespaces.len() == 1 {
        namespaces.into_iter().next()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_infers_namespace_from_self_qualified_stored_links() {
        let introspection = from_raw(IntrospectionRaw {
            tables: vec![],
            migration_state: MigrationState::NoMigrationTable,
            schema_source: r#"
record Parent {
    @public
    id       Id.Int @id
    children @link(Campaign.Child.parentId)
}

record Child {
    @public
    id       Id.Int @id
    parentId Parent.id
    parent   @link(parentId, Campaign.Parent.id)
}
"#
            .to_string(),
            links: vec![],
        });

        match introspection.schema {
            SchemaResult::Success { schema, context } => {
                assert_eq!(schema.namespace, "Campaign");
                assert_eq!(context.valid_namespaces.len(), 1);
                assert!(context.valid_namespaces.contains("Campaign"));
            }
            SchemaResult::FailedToTypecheck { errors, .. } => {
                panic!("schema should typecheck after namespace inference: {errors:?}")
            }
            SchemaResult::FailedToParse { errors, .. } => {
                panic!("schema should parse: {errors:?}")
            }
        }
    }
}

pub fn extract_links(schema: &ast::Schema, tables: &[Table]) -> Vec<Link> {
    let mut links = Vec::new();
    let table_map: std::collections::HashMap<String, &Table> =
        tables.iter().map(|t| (t.name.clone(), t)).collect();

    // Extract links from all records in the schema
    for file in &schema.files {
        for def in &file.definitions {
            if let ast::Definition::Record { name, fields, .. } = def {
                let table_name = ast::get_tablename(name, fields);
                let record_links = ast::collect_links(fields);

                // Get primary key field name for this record
                let primary_key_name = ast::get_primary_id_field_name(fields);

                for link in record_links {
                    // Get the actual table name for the foreign table
                    let foreign_table_name = ast::get_foreign_tablename(schema, &link);

                    // Determine link type: if local_ids contains non-primary-key fields, it's many-to-one
                    // Otherwise (all local_ids are primary keys), it's one-to-many (reverse link)
                    let is_many_to_one = link
                        .local_ids
                        .iter()
                        .any(|id| primary_key_name.as_ref().map(|pk| id != pk).unwrap_or(true));

                    let link_type = if is_many_to_one {
                        "many-to-one"
                    } else {
                        "one-to-many"
                    };

                    // For many-to-one, the foreign_key_field is the first local_id (the FK field)
                    // For one-to-many, we need to find the FK field in the related table
                    let foreign_key_field = if is_many_to_one {
                        link.local_ids[0].clone()
                    } else {
                        // For one-to-many, find the FK field in the related table that points to this table
                        // Look for a FK in the related table that points to this table's id
                        if let Some(related_table) = table_map.get(&foreign_table_name) {
                            related_table
                                .foreign_keys
                                .iter()
                                .find(|fk| fk.table == table_name && fk.to == "id")
                                .map(|fk| fk.from.clone())
                                .unwrap_or_else(|| {
                                    // Fallback: use first local_id (should be "id" for one-to-many)
                                    link.local_ids[0].clone()
                                })
                        } else {
                            link.local_ids[0].clone()
                        }
                    };

                    links.push(Link {
                        table_name: table_name.clone(),
                        field_name: link.link_name.clone(),
                        foreign_key_field,
                        related_table: foreign_table_name,
                        link_type: link_type.to_string(),
                    });
                }
            }
        }
    }

    links
}
