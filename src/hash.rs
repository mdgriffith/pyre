use crate::ast::*;
use sha2::{Digest, Sha256};

pub fn hash_query_interface(query: &Query) -> String {
    let mut hasher = Sha256::new();

    // Hash operation
    hasher.update(format!("{:?}", query.operation));

    // Hash name
    hasher.update(&query.name);

    // Hash args
    for arg in &query.args {
        let type_string = arg.type_.clone().unwrap_or("".to_string());
        hasher.update(&arg.name);
        hasher.update(type_string);
    }

    // Hash fields
    hash_fields(&mut hasher, &query.fields);

    format!("{:x}", hasher.finalize())
}

pub fn hash_query_full(query: &Query) -> String {
    let mut hasher = Sha256::new();

    // Hash operation
    hasher.update(format!("{:?}", query.operation));

    // Hash name
    hasher.update(&query.name);

    // Hash args (excluding Location fields)
    for arg in &query.args {
        let type_string = arg.type_.clone().unwrap_or("".to_string());
        hasher.update(&arg.name);
        hasher.update(type_string);
    }

    // Hash fields (excluding Location fields)
    hash_fields(&mut hasher, &query.fields);

    format!("{:x}", hasher.finalize())
}
fn hash_fields(hasher: &mut Sha256, fields: &[TopLevelQueryField]) {
    for field in fields {
        match field {
            TopLevelQueryField::Field(query_field) => {
                hasher.update(&query_field.name);
                if let Some(alias) = &query_field.alias {
                    hasher.update(alias);
                }
                if let Some(set) = &query_field.set {
                    hash_query_value(hasher, set);
                }
                for directive in &query_field.directives {
                    hasher.update(directive);
                }
                for arg_field in &query_field.fields {
                    match arg_field {
                        ArgField::Field(query_field) => {
                            hash_fields(hasher, &[TopLevelQueryField::Field(query_field.clone())])
                        }
                        ArgField::Arg(located_arg) => hash_arg(hasher, &located_arg.arg),
                        ArgField::Lines { count } => hasher.update(count.to_string()),
                        ArgField::QueryComment { .. } => {}
                    }
                }
            }
            TopLevelQueryField::Lines { .. } => {}
            TopLevelQueryField::Comment { .. } => {}
        }
    }
}

fn hash_arg(hasher: &mut Sha256, arg: &Arg) {
    match arg {
        Arg::Limit(value) => {
            hasher.update("limit");
            hash_query_value(hasher, value);
        }
        Arg::OrderBy(direction, field) => {
            hasher.update("order_by");
            hasher.update(direction_to_string(direction));
            hasher.update(field);
        }
        Arg::Where(where_arg) => {
            hasher.update("where");
            hash_where_arg(hasher, where_arg);
        }
    }
}

fn hash_where_arg(hasher: &mut Sha256, where_arg: &WhereArg) {
    match where_arg {
        WhereArg::Exists(path, body) => {
            hasher.update("exists");
            hasher.update((path.len() as u64).to_le_bytes());
            for (segment, _) in path {
                hasher.update((segment.len() as u64).to_le_bytes());
                hasher.update(segment);
            }
            hash_where_arg(hasher, body);
        }
        WhereArg::Column(is_session_var, column, operator, value, _field_name_range) => {
            hasher.update(&is_session_var.to_string());
            hasher.update(column);
            hasher.update(format!("{:?}", operator));
            hash_query_value(hasher, value);
        }
        WhereArg::And(args) | WhereArg::Or(args) => {
            hasher.update(if matches!(where_arg, WhereArg::And(_)) {
                "and"
            } else {
                "or"
            });
            for arg in args {
                hash_where_arg(hasher, arg);
            }
        }
    }
}

fn hash_query_value(hasher: &mut Sha256, value: &QueryValue) {
    match value {
        QueryValue::Fn(func) => {
            hasher.update("fn");
            hasher.update(&func.name);
            for arg in &func.args {
                hash_query_value(hasher, arg);
            }
        }
        QueryValue::Variable((_, var)) => {
            hasher.update("variable");
            hasher.update(&var.name);
        }
        QueryValue::String((_, s)) => {
            hasher.update("string");
            hasher.update(s);
        }
        QueryValue::Int((_, i)) => {
            hasher.update("int");
            hasher.update(&i.to_string());
        }
        QueryValue::Float((_, f)) => {
            hasher.update("float");
            hasher.update(&f.to_string());
        }
        QueryValue::Bool((_, b)) => {
            hasher.update("bool");
            hasher.update(&b.to_string());
        }
        QueryValue::Null(_) => {
            hasher.update("null");
        }
        QueryValue::LiteralTypeValue((_, LiteralTypeValueDetails { name, fields })) => {
            hasher.update("literal_type");
            hasher.update(&name.to_string());
            if let Some(fields) = fields {
                for (field_name, field_value) in fields {
                    hasher.update(field_name);
                    hash_query_value(hasher, field_value);
                }
            }
        }
    }
}
