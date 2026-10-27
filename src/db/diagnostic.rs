/// A database location suitable for diagnostics, never URL credentials or parameters.
pub fn database_location(database: &str) -> String {
    let resolved = database
        .strip_prefix('$')
        .and_then(|name| std::env::var(name).ok());
    let database = resolved.as_deref().unwrap_or(database);
    if let Some((scheme, target)) = database.split_once("://") {
        let target = target.split(['?', '#']).next().unwrap_or(target);
        let target = target.rsplit_once('@').map_or(target, |(_, host)| host);
        format!("{scheme}://{target}")
    } else {
        database.to_string()
    }
}

/// Driver messages can contain URLs too. Preserve useful details without credentials.
pub fn redact_database_urls(message: &str) -> String {
    message
        .split_inclusive(char::is_whitespace)
        .map(|part| {
            if let Some(scheme_end) = part.find("://") {
                let start = part[..scheme_end]
                    .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '-' && c != '.')
                    .map_or(0, |index| index + 1);
                let end = part[scheme_end + 3..]
                    .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ')' | '>'))
                    .map_or(part.len(), |index| scheme_end + 3 + index);
                format!(
                    "{}{}{}",
                    &part[..start],
                    database_location(&part[start..end]),
                    &part[end..]
                )
            } else {
                part.to_string()
            }
        })
        .collect()
}

pub fn database_error(error: &libsql::Error) -> String {
    use libsql::Error::*;
    let message = match error {
        ConnectionFailed(cause) => format!("Could not establish a database connection: {cause}"),
        SqliteFailure(_, cause) | RemoteSqliteFailure(_, _, cause) => cause.clone(),
        NullValue => "Expected a value in the database result, but found NULL.".into(),
        Misuse(cause) => format!("Unexpected use of the database driver: {cause}"),
        ExecuteReturnedRows => {
            "A database statement returned rows where Pyre expected none.".into()
        }
        QueryReturnedNoRows => {
            "The database query returned no rows where Pyre expected one.".into()
        }
        InvalidColumnName(name) => format!("The database result has no column named {name}."),
        ColumnNotFound(index) => format!("The database result is missing column {index}."),
        InvalidColumnIndex => "Pyre tried to read a column outside the database result.".into(),
        InvalidColumnType => {
            "A database result column has a different type than Pyre expected.".into()
        }
        ToSqlConversionFailure(cause) => format!("Could not convert database data: {cause}"),
        Sqlite3SyntaxError(_, _, cause) => format!("SQL syntax error: {cause}"),
        Sqlite3ParserError(cause) => format!("Could not parse the SQL statement: {cause}"),
        Sqlite3UnsupportedStatement => {
            "This SQL statement is not supported by the database driver.".into()
        }
        Hrana(cause) => format!("Communication with the remote database failed: {cause}"),
        WriteDelegation(cause) => {
            format!("The database could not forward a write to its primary: {cause}")
        }
        Bincode(cause) => format!("Could not encode or decode the database protocol: {cause}"),
        Replication(cause) => format!("Database replication failed: {cause}"),
        Sync(cause) => format!("Database synchronization failed: {cause}"),
        SyncNotSupported(cause) => {
            format!("This database connection does not support synchronization: {cause}")
        }
        LoadExtensionNotSupported => {
            "Loading extensions requires a local database connection.".into()
        }
        AuthorizerNotSupported => {
            "Database authorizers require a local database connection.".into()
        }
        UpdateHookNotSupported => {
            "Database update hooks require a local database connection.".into()
        }
        InvalidUTF8Path => "The database path contains invalid UTF-8 characters.".into(),
        FreezeNotSupported(cause) => format!("This database does not support freezing: {cause}"),
        InvalidParserState(cause) => {
            format!("The database SQL parser entered an unexpected state: {cause}")
        }
        InvalidTlsConfiguration(cause) => {
            format!("Could not configure a secure database connection: {cause}")
        }
        TransactionalBatchError(cause) => {
            format!("A statement in the database transaction failed: {cause}")
        }
        InvalidBlobSize(expected) => format!(
            "The database returned binary data of an unexpected size; expected {expected} bytes."
        ),
        WalConflict => {
            "Database replication encountered conflicting write-ahead log entries.".into()
        }
        ReservedBytesNotSupported => {
            "This database uses reserved bytes that the driver does not support.".into()
        }
        _ => format!("The database operation failed: {error}"),
    };
    redact_database_urls(&message)
}

pub fn context(database: &str, namespace: &str) -> String {
    format!(
        "Schema: {namespace}\nDatabase: {}",
        database_location(database)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_and_driver_causes_do_not_expose_url_credentials() {
        let url = "libsql://user:password@example.turso.io/db?authToken=secret#token";
        assert_eq!(database_location(url), "libsql://example.turso.io/db");
        let error =
            libsql::Error::ConnectionFailed(format!("Failed to connect to '{url}': unavailable"));
        let message = operation_error(url, "Campaign", "Could not connect.", &error);
        assert!(message.contains("Schema: Campaign"));
        assert!(message.contains("libsql://example.turso.io/db"));
        for secret in ["user", "password", "authToken", "secret", "#token"] {
            assert!(!message.contains(secret), "{message}");
        }
        assert_eq!(
            database_location("/tmp/local?name#1.db"),
            "/tmp/local?name#1.db"
        );
    }
}

pub fn operation_error(
    database: &str,
    namespace: &str,
    operation: &str,
    error: &libsql::Error,
) -> String {
    format!(
        "{operation}\n{}\n\n{}",
        context(database, namespace),
        database_error(error)
    )
}
