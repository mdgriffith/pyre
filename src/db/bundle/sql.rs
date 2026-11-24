//! Conservative SQL subset shared by native and WASM migration hosts.

use super::Bundle;
use fallible_iterator::FallibleIterator;
use libsql_sqlite3_parser::{
    ast::{Cmd, QualifiedName, Stmt, ToTokens, TokenStream},
    lexer::sql::{Parser, TokenType},
};
use std::fmt;

#[derive(Debug)]
pub struct Error {
    file: String,
    statement: usize,
    reason: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} statement {}: {}",
            self.file, self.statement, self.reason
        )
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub struct Plan<'a> {
    pub statements: Vec<&'a str>,
    pub assertions: Vec<(&'a str, &'a str)>,
}

pub fn plan(bundle: &Bundle) -> Result<Plan<'_>, Error> {
    let statements = parse(super::MIGRATION_PATH, bundle.migration_sql(), false)?;
    let assertions = bundle
        .assertions()
        .map(|(name, sql)| {
            parse(name, sql, true).map(|mut statements| (name, statements.remove(0)))
        })
        .collect::<Result<_, _>>()?;
    Ok(Plan {
        statements,
        assertions,
    })
}

fn parse<'a>(file: &str, source: &'a str, assertion: bool) -> Result<Vec<&'a str>, Error> {
    let mut statements = Vec::new();
    let mut parser = Box::new(Parser::new(source.as_bytes()));
    let mut start = 0;
    loop {
        let fail = |reason: String| Error {
            file: file.into(),
            statement: statements.len() + 1,
            reason,
        };
        let Some(command) = parser.next().map_err(|e| fail(e.to_string()))? else {
            break;
        };
        match &command {
            Cmd::Stmt(Stmt::Select(_)) if assertion => {},
            _ if assertion => return Err(fail("assertions must be a single read-only SELECT".into())),
            Cmd::Stmt(Stmt::CreateTable { temporary: false, .. }
                | Stmt::CreateIndex { .. }
                | Stmt::AlterTable(..)
                | Stmt::DropTable { .. }
                | Stmt::DropIndex { .. }
                | Stmt::Insert { .. }
                | Stmt::Update { .. }
                | Stmt::Delete { .. }) => {},
            Cmd::Stmt(Stmt::Pragma(..)) => return Err(fail("PRAGMA is not supported in migration SQL; use a rebuild that does not change connection settings such as foreign_keys".into())),
            _ => return Err(fail("unsupported migration statement; use transactional table/index DDL or INSERT, UPDATE, DELETE".into())),
        }
        command
            .to_tokens(&mut CheckTokens)
            .map_err(|reason| fail(reason.into()))?;
        if let Cmd::Stmt(stmt) = &command {
            let target: Option<&QualifiedName> = match stmt {
                Stmt::CreateTable { tbl_name, .. }
                | Stmt::Delete { tbl_name, .. }
                | Stmt::Insert { tbl_name, .. }
                | Stmt::Update { tbl_name, .. }
                | Stmt::DropTable { tbl_name, .. }
                | Stmt::AlterTable(tbl_name, _) => Some(tbl_name),
                Stmt::CreateIndex { idx_name, .. } | Stmt::DropIndex { idx_name, .. } => {
                    Some(idx_name)
                }
                _ => None,
            };
            if target
                .and_then(|name| name.db_name.as_ref())
                .is_some_and(|db| {
                    !db.0
                        .trim_matches(['\'', '"', '`', '[', ']'])
                        .eq_ignore_ascii_case("main")
                })
            {
                return Err(fail(
                    "migration writes must target the main database".into(),
                ));
            }
        }
        if assertion && !statements.is_empty() {
            return Err(fail("assertions must be a single read-only SELECT".into()));
        }
        let end = parser.offset();
        statements.push(&source[start..end]);
        start = end;
    }
    if assertion && statements.is_empty() {
        return Err(Error {
            file: file.into(),
            statement: 1,
            reason: "assertion is empty".into(),
        });
    }
    Ok(statements)
}

struct CheckTokens;

impl TokenStream for CheckTokens {
    type Error = &'static str;

    fn append(&mut self, token: TokenType, value: Option<&str>) -> Result<(), Self::Error> {
        if matches!(token, TokenType::TK_ROLLBACK) {
            return Err("ROLLBACK conflict actions are not supported in runner-owned transactions");
        }
        if matches!(token, TokenType::TK_VARIABLE) {
            return Err("migration SQL does not accept bind parameters");
        }
        if let Some(value) = value {
            let name = value
                .trim_start_matches(['\'', '"', '`', '['])
                .to_ascii_lowercase();
            if name.starts_with("_pyre_")
                || name.starts_with("sqlite_")
                || name.starts_with("libsql_")
            {
                return Err(
                    "references to reserved Pyre, SQLite or libSQL objects are not supported",
                );
            }
            if name.starts_with("pragma_")
                || name.trim_end_matches(['\'', '"', '`', ']']) == "load_extension"
            {
                return Err("connection operations are not supported in migration SQL");
            }
        }
        Ok(())
    }
}
