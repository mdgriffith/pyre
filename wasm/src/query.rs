use crate::cache;
use pyre::ast;
use pyre::db::introspect;
use pyre::error;
use pyre::generate::sql::to_sql::SqlAndParams;
use pyre::parser;
use pyre::typecheck;

const QUERY_FILE: &str = "query.pyre";

/**
 * Dynamically parse a query and return the sql that is generated
 */
pub fn query_to_sql(
    context: &typecheck::Context,
    query_source: &str,
) -> Result<Vec<SqlAndParams>, Vec<error::Error>> {
    match parser::parse_query(&QUERY_FILE, query_source) {
        Ok(query_list) => {
            let query_list: ast::QueryList = query_list;

            // Find the first query in the list
            // We're only running exactly one query in this context.
            let mut found_query = None;
            for query_def in &query_list.queries {
                match query_def {
                    ast::QueryDef::Query(query) => {
                        if found_query.is_some() {
                            // Found more than one query
                            return Err(vec![error::Error {
                                error_type: error::ErrorType::ParsingError(
                                    error::ParsingErrorDetails {
                                        expecting: error::Expecting::PyreFile,
                                    },
                                ),
                                filepath: QUERY_FILE.to_string(),
                                locations: vec![],
                            }]);
                        }
                        found_query = Some(query);
                    }
                    _ => continue,
                }
            }

            // Extract the query or return error if none found
            match found_query {
                Some(query) => {
                    let mut errors = Vec::new();
                    // Typecheck and generate
                    let query_info: typecheck::QueryInfo =
                        typecheck::check_query(context, &mut errors, &query);

                    if errors.len() > 0 {
                        return Err(errors);
                    }

                    let mut sql = Vec::new();
                    for field in &query.fields {
                        match field {
                            ast::TopLevelQueryField::Field(query_field) => {
                                let table = context.tables.get(&query_field.name).unwrap();
                                let prepared = pyre::generate::sql::to_string(
                                    context,
                                    query,
                                    &query_info,
                                    table,
                                    query_field,
                                );
                                for prepared in prepared {
                                    sql.push(SqlAndParams::Sql(prepared.sql));
                                }
                            }
                            _ => (),
                        }
                    }
                    Ok(sql)
                }
                None => {
                    return Err(vec![error::Error {
                        error_type: error::ErrorType::ParsingError(error::ParsingErrorDetails {
                            expecting: error::Expecting::PyreFile,
                        }),
                        filepath: QUERY_FILE.to_string(),
                        locations: vec![],
                    }]);
                }
            }
        }
        Err(err) => match parser::convert_parsing_error(err) {
            Some(error) => Err(vec![error]),
            None => Err(vec![error::Error {
                error_type: error::ErrorType::ParsingError(error::ParsingErrorDetails {
                    expecting: error::Expecting::PyreFile,
                }),
                filepath: QUERY_FILE.to_string(),
                locations: vec![],
            }]),
        },
    }
}

pub fn query_to_sql_wasm(query_source: String) -> Result<Vec<SqlAndParams>, Vec<String>> {
    let introspection = match cache::get() {
        Some(introspection) => introspection,
        None => {
            return Err(vec!["No schema found".to_string()]);
        }
    };

    match &introspection.schema {
        introspect::SchemaResult::Success { context, .. } => {
            match query_to_sql(&context, &query_source) {
                Ok(result) => Ok(result),
                Err(errors) => {
                    let mut formatted_errors = Vec::new();
                    for error in errors {
                        formatted_errors.push(error::format_error(&query_source, &error, false));
                    }
                    Err(formatted_errors)
                }
            }
        }
        _ => Err(vec!["No schema found".to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> typecheck::Context {
        let mut schema = ast::Schema::default();
        parser::run(
            "schema.pyre",
            r#"
type EventPayload
   = Created { title String }
   | Deleted

record Event {
    @public
    id       Int @id
    ownerId  Int @immutable
    code     String @immutable
    payload  EventPayload @immutable
    title    String
}
"#,
            &mut schema,
        )
        .expect("schema parses");
        typecheck::check_schema(&ast::Database {
            schemas: vec![schema],
        })
        .expect("schema typechecks")
    }

    fn assert_immutable_error(source: &str, field: &str) {
        let errors = match query_to_sql(&context(), source) {
            Ok(_) => panic!("immutable update must fail"),
            Err(errors) => errors,
        };
        assert!(errors.iter().any(|error| matches!(
            error.error_type,
            error::ErrorType::ImmutableColumnCannotBeUpdated { field: ref actual }
                if actual == field
        )));
    }

    #[test]
    fn query_to_sql_enforces_immutable_assignments_before_generating_sql() {
        assert_immutable_error(
            "update SetOwner($id: Int, $ownerId: Int) { event { @where { id == $id } ownerId = $ownerId } }",
            "ownerId",
        );
        assert_immutable_error(
            "update SetCode($id: Int) { event { @where { id == $id } code = \"fixed\" } }",
            "code",
        );
        assert_immutable_error(
            "update SetPayload($id: Int, $title: String) { event { @where { id == $id } payload = Created { title = $title } } }",
            "payload",
        );

        let sql = query_to_sql(
            &context(),
            "update Rename($id: Int, $title: String) { event { @where { id == $id } title = $title ownerId } }",
        )
        .expect("mutable update should generate SQL");
        assert!(!sql.is_empty());
        assert!(sql.iter().any(|statement| match statement {
            SqlAndParams::Sql(sql) => sql.contains("title = $title"),
            SqlAndParams::SqlWithParams { sql, .. } => sql.contains("title = $title"),
        }));
    }
}
