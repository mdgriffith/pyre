pub use pyre::db::diagnostic::{context, database_location, operation_error};

pub fn format_libsql_error(error: &libsql::Error) -> String {
    pyre::db::diagnostic::database_error(error)
}
