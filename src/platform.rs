use crate::ast;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub enum PrimitiveType {
    Number,
    Float,
    Int,
    String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FuncDefinition {
    pub name: String,
    pub arg_types: Vec<String>,
    pub return_type: String,
}

fn insert_fn(map: &mut HashMap<String, FuncDefinition>, def: FuncDefinition) {
    map.insert(def.name.clone(), def);
}

#[rustfmt::skip]
pub fn add_builtin(fns: &mut HashMap<String, FuncDefinition>) {

    insert_fn(fns, FuncDefinition { name: "max".to_string(), arg_types: vec!["number".to_string(),"number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "min".to_string(), arg_types: vec!["number".to_string(),"number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "abs".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "acos".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "asin".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "atan".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "atan2".to_string(), arg_types: vec!["number".to_string(), "number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "ceil".to_string(), arg_types: vec!["number".to_string()], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "cos".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "exp".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "floor".to_string(), arg_types: vec!["number".to_string()], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "ln".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "log".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "mod".to_string(), arg_types: vec!["number".to_string(), "number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "pi".to_string(), arg_types: vec![], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "pow".to_string(), arg_types: vec!["number".to_string(), "number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "random".to_string(), arg_types: vec![], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "randomblob".to_string(), arg_types: vec!["Int".to_string()], return_type: "Blob".to_string() });
    insert_fn(fns, FuncDefinition { name: "round".to_string(), arg_types: vec!["number".to_string()], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "sign".to_string(), arg_types: vec!["number".to_string()], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "sin".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "sqrt".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "tan".to_string(), arg_types: vec!["number".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "trunc".to_string(), arg_types: vec!["number".to_string()], return_type: "Int".to_string() });

    // String Functions
    insert_fn(fns, FuncDefinition { name: "length".to_string(), arg_types: vec!["String".to_string()], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "lower".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "upper".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "substr".to_string(), arg_types: vec!["String".to_string(), "Int".to_string(), "Int".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "trim".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "ltrim".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "rtrim".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "replace".to_string(), arg_types: vec!["String".to_string(), "String".to_string(), "String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "like".to_string(), arg_types: vec!["String".to_string(), "String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "hex".to_string(), arg_types: vec!["Blob".to_string()], return_type: "String".to_string() });

    // Date and Time Functions
    insert_fn(fns, FuncDefinition { name: "date".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "time".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "datetime".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "julianday".to_string(), arg_types: vec!["String".to_string()], return_type: "Float".to_string() });
    insert_fn(fns, FuncDefinition { name: "strftime".to_string(), arg_types: vec!["String".to_string()], return_type: "String".to_string() });

    // Control Flow Functions
    insert_fn(fns, FuncDefinition { name: "ifnull".to_string(), arg_types: vec!["String".to_string(), "String".to_string()], return_type: "String".to_string() });
    insert_fn(fns, FuncDefinition { name: "nullif".to_string(), arg_types: vec!["String".to_string(), "String".to_string()], return_type: "String".to_string() });

    // Other Functions
    insert_fn(fns, FuncDefinition { name: "now".to_string(), arg_types: vec![], return_type: "DateTime".to_string() });
    insert_fn(fns, FuncDefinition { name: "total_changes".to_string(), arg_types: vec![], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "changes".to_string(), arg_types: vec![], return_type: "Int".to_string() });
    insert_fn(fns, FuncDefinition { name: "last_insert_rowid".to_string(), arg_types: vec![], return_type: "Int".to_string() });

}

pub fn to_serialization_type(type_: &str) -> ast::SerializationType {
    match type_ {
        "String" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::Text),
        "Int" | "Sequence.Int" => {
            ast::SerializationType::Concrete(ast::ConcreteSerializationType::Integer)
        }
        "Float" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::Real),
        "Bool" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::Integer),
        "DateTime" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::Integer),
        "Date" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::Text),
        "JSON" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::JsonB),
        "Id.Int" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::IdInt),
        "Id.Uuid" => ast::SerializationType::Concrete(ast::ConcreteSerializationType::IdUuid),
        _ => ast::SerializationType::FromType(type_.to_string()),
    }
}
