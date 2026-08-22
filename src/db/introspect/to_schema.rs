use crate::ast::{
    Column, ColumnDirective, ColumnType, Definition, Field, FieldDirective, SchemaFile,
};
use crate::db::introspect::{ColumnInfo, Introspection};

pub fn to_schema(introspection: &Introspection) -> SchemaFile {
    let mut definitions = Vec::new();

    for table in &introspection.tables {
        let mut fields = Vec::new();

        // Convert columns to fields
        for column in &table.columns {
            fields.push(Field::Column(column_info_to_column(column)));
        }

        if table.indexes.iter().any(|index| {
            index.name == format!("uniq_{}_singleton", table.name)
                && index.unique
                && index.columns.len() == 1
                && index.columns[0].expression
                && index.columns[0].name == "1"
        }) {
            fields.push(Field::FieldDirective(FieldDirective::Singleton));
        }

        // Add the record definition
        definitions.push(Definition::Record {
            name: table.name.clone(),
            fields,
            start: None,
            end: None,
            start_name: None,
            end_name: None,
        });
    }

    SchemaFile {
        path: String::from("schema.sql"), // Default path
        definitions,
    }
}

fn column_info_to_column(info: &ColumnInfo) -> Column {
    let mut directives = Vec::new();

    // Handle primary key
    if info.pk {
        directives.push(ColumnDirective::PrimaryKey);
    }

    // Handle index directive
    if info.indexed {
        directives.push(ColumnDirective::Index);
    }

    // Handle not null constraint
    let nullable = !info.notnull;

    // Map SQLite column type to ColumnType
    let type_ = match info.column_type.to_lowercase().as_str() {
        "integer" => ColumnType::Int,
        "real" => ColumnType::Float,
        "text" => ColumnType::String,
        "blob" => ColumnType::String, // Default to text for blob
        _ => ColumnType::Custom(info.column_type.clone()), // Use custom for unknown types
    };

    Column {
        name: info.name.clone(),
        type_,
        nullable,
        directives,
        start: None,
        end: None,
        start_name: None,
        end_name: None,
        start_typename: None,
        end_typename: None,
        inline_comment: None,
    }
}
