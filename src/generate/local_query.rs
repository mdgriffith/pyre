use crate::ast;

pub const SESSION_ERROR: &str =
    "Local queries cannot reference Session; use explicit inputs or execute on the server.";

/// Inspect authored query arguments only, never permissions added during SQL generation.
pub fn references_session(query: &ast::Query) -> bool {
    query.operation == ast::QueryOperation::Query
        && query.fields.iter().any(|field| match field {
            ast::TopLevelQueryField::Field(field) => field_references_session(field),
            _ => false,
        })
}

fn field_references_session(field: &ast::QueryField) -> bool {
    field.fields.iter().any(|field| match field {
        ast::ArgField::Field(field) => field_references_session(field),
        ast::ArgField::Arg(arg) => match &arg.arg {
            ast::Arg::Where(predicate) => where_references_session(predicate),
            ast::Arg::Limit(value) => value_references_session(value),
            ast::Arg::OrderBy(_, _) => false,
        },
        _ => false,
    })
}

fn where_references_session(predicate: &ast::WhereArg) -> bool {
    match predicate {
        ast::WhereArg::Constant(_) => false,
        ast::WhereArg::Column(session, _, _, value, _) => {
            *session || value_references_session(value)
        }
        ast::WhereArg::Exists(_, body) => where_references_session(body),
        ast::WhereArg::And(items) | ast::WhereArg::Or(items) => {
            items.iter().any(where_references_session)
        }
    }
}

fn value_references_session(value: &ast::QueryValue) -> bool {
    match value {
        ast::QueryValue::Variable((_, variable)) => variable.session_field.is_some(),
        ast::QueryValue::Fn(function) => function.args.iter().any(value_references_session),
        ast::QueryValue::LiteralTypeValue((_, literal)) => literal
            .fields
            .iter()
            .flatten()
            .any(|(_, value)| value_references_session(value)),
        _ => false,
    }
}
