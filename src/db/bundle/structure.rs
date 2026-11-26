//! Structural comparison shared by baseline, promotion and explicit audits.

use crate::db::{diff, introspect};
use crate::generate::sql::to_sql::SqlAndParams;
use fallible_iterator::FallibleIterator;
use libsql_sqlite3_parser::{
    ast::{Cmd, CreateTableBody, Stmt, ToTokens, TokenStream},
    lexer::sql::{Parser, TokenType},
};
use std::collections::BTreeMap;

pub const READ_STRUCTURE: &str = "select type, name, sql from main.sqlite_schema where sql is not null and lower(substr(name,1,7)) != 'sqlite_' and lower(name) not in ('_pyre_migrations','_pyre_external_identities','_pyre_sync','_pyre_database_policy','_pyre_finalized_schema','_pyre_migration_receipts','_pyre_migration_bundles','_pyre_migration_drafts')";

pub fn audit(
    context: &crate::typecheck::Context,
    actual: Vec<(String, String, String)>,
) -> Result<(), String> {
    let empty = introspect::from_raw(introspect::IntrospectionRaw {
        tables: vec![],
        migration_state: introspect::MigrationState::NoMigrationTable,
        schema_source: String::new(),
        links: vec![],
        schema_namespace: None,
        external_identities: vec![],
    });
    let ddl = diff::to_sql::to_sql(&diff::diff(context, &context.database.schemas[0], &empty));
    let mut expected = BTreeMap::new();
    for statement in ddl {
        let SqlAndParams::Sql(source) = statement else {
            return Err("unexpected parameterized schema definition".into());
        };
        let command = parse(&source)?;
        let (kind, name) = match &command {
            Cmd::Stmt(Stmt::CreateTable { tbl_name, .. }) => ("table", &tbl_name.name.0),
            Cmd::Stmt(Stmt::CreateIndex { idx_name, .. }) => ("index", &idx_name.name.0),
            _ => return Err("unexpected generated schema definition".into()),
        };
        expected.insert(
            (kind.to_string(), name.trim_matches('"').to_string()),
            definition(command),
        );
    }
    let actual = actual
        .into_iter()
        .map(|(kind, name, source)| Ok(((kind, name), definition(parse(&source)?))))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let mut changes = Vec::new();
    for (name, definition) in &expected {
        match actual.get(name) {
            None => changes.push(format!("missing {} {}", name.0, name.1)),
            Some(actual) if actual != definition => {
                changes.push(format!("different definition for {} {}", name.0, name.1))
            }
            _ => {}
        }
    }
    for name in actual.keys().filter(|name| !expected.contains_key(*name)) {
        changes.push(format!("unexpected {} {}", name.0, name.1));
    }
    if changes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "physical schema differs from the expected schema:\n{}",
            changes.join("\n")
        ))
    }
}

fn parse(source: &str) -> Result<Cmd, String> {
    Box::new(Parser::new(source.as_bytes()))
        .next()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "empty catalog definition".into())
}

fn definition(command: Cmd) -> Vec<Tokens> {
    match command {
        Cmd::Stmt(Stmt::CreateTable {
            body:
                CreateTableBody::ColumnsAndConstraints {
                    columns,
                    constraints,
                    options,
                },
            ..
        }) => {
            // Physical column order is irrelevant to generated named queries.
            let mut definition: Vec<_> = columns.iter().map(tokens).collect();
            definition.extend(
                constraints
                    .into_iter()
                    .flatten()
                    .map(|constraint| tokens(&constraint)),
            );
            definition.sort();
            definition.push(vec![format!("table options {}", options.bits())]);
            definition
        }
        Cmd::Stmt(Stmt::CreateIndex {
            unique,
            idx_name,
            tbl_name,
            columns,
            where_clause,
            ..
        }) => {
            // SQLite strips IF NOT EXISTS, and ASC is implicit.
            vec![tokens(&Stmt::CreateIndex {
                unique,
                idx_name,
                tbl_name,
                columns,
                where_clause,
                if_not_exists: false,
            })]
        }
        other => vec![tokens(&other)],
    }
}

type Tokens = Vec<String>;

fn tokens(value: &impl ToTokens) -> Tokens {
    #[derive(Default)]
    struct Stream(Tokens);
    impl TokenStream for Stream {
        type Error = std::convert::Infallible;
        fn append(&mut self, kind: TokenType, value: Option<&str>) -> Result<(), Self::Error> {
            let value = match (kind, value) {
                (TokenType::TK_ID, Some(name)) => {
                    name.trim_matches(['"', '`', '[', ']']).to_ascii_lowercase()
                }
                (_, Some(value)) => value.to_string(),
                (_, None) => String::new(),
            };
            self.0.push(format!("{}:{value}", kind as u16));
            Ok(())
        }
    }
    let mut stream = Stream::default();
    value.to_tokens(&mut stream).unwrap();
    stream.0
}
