use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use super::shared::Options;

pub async fn lsp(options: &Options<'_>) -> io::Result<()> {
    let in_dir = options.in_dir.to_path_buf();
    let (service, socket) = LspService::new(move |client| Backend {
        client,
        in_dir,
        documents: Mutex::new(HashMap::new()),
        published: Mutex::new(HashSet::new()),
        validation: tokio::sync::Mutex::new(()),
    });

    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
    Ok(())
}

struct Backend {
    client: Client,
    in_dir: PathBuf,
    documents: Mutex<HashMap<Url, String>>,
    published: Mutex<HashSet<Url>>,
    validation: tokio::sync::Mutex<()>,
}

impl Backend {
    async fn validate(&self) {
        let _validation = self.validation.lock().await;
        let documents = self.documents.lock().unwrap().clone();
        let mut found = match crate::filesystem::collect_filepaths(&self.in_dir) {
            Ok(found) => found,
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Pyre check failed: {error}"))
                    .await;
                return;
            }
        };

        let mut overlays = HashMap::new();
        for (uri, source) in &documents {
            let Ok(path) = uri.to_file_path() else {
                continue;
            };
            let path = path.to_string_lossy().into_owned();
            add_new_document(&mut found, &self.in_dir, &path, source);
            overlays.insert(path.clone(), source.clone());
            replace_source(&mut found, &path, source);
            for query_path in &found.query_files {
                if paths_match(query_path, &path) {
                    overlays.insert(query_path.clone(), source.clone());
                }
            }
        }

        let errors = match parse_errors(&found, &overlays) {
            errors if !errors.is_empty() => errors,
            _ => match super::check::run_check(found, false, Some(&overlays)) {
                Ok(file_errors) => file_errors
                    .into_iter()
                    .flat_map(|file_error| file_error.errors)
                    .collect(),
                Err(error) => {
                    self.client
                        .log_message(MessageType::ERROR, format!("Pyre check failed: {error}"))
                        .await;
                    Vec::new()
                }
            },
        };

        let mut diagnostics: HashMap<Url, Vec<Diagnostic>> = HashMap::new();
        for error in errors {
            let Some(uri) = filepath_to_url(&error.filepath) else {
                continue;
            };
            let range = error
                .locations
                .first()
                .and_then(|location| location.primary.first())
                .map(|range| {
                    let source = documents.get(&uri).cloned().or_else(|| {
                        uri.to_file_path()
                            .ok()
                            .and_then(|path| std::fs::read_to_string(path).ok())
                    });
                    to_lsp_range(range, source.as_deref())
                })
                .unwrap_or_default();
            diagnostics.entry(uri).or_default().push(Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("pyre".into()),
                code: Some(NumberOrString::String(pyre::error::to_error_title(
                    &error.error_type,
                ))),
                message: pyre::error::to_error_description(&error, false),
                ..Diagnostic::default()
            });
        }

        let previous = self.published.lock().unwrap().clone();
        for uri in previous {
            if !diagnostics.contains_key(&uri) {
                self.client.publish_diagnostics(uri, Vec::new(), None).await;
            }
        }
        for (uri, items) in &diagnostics {
            self.client
                .publish_diagnostics(uri.clone(), items.clone(), None)
                .await;
        }
        *self.published.lock().unwrap() = diagnostics.into_keys().collect();
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "pyre".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Pyre language server initialized")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .insert(params.text_document.uri, params.text_document.text);
        self.validate().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents
                .lock()
                .unwrap()
                .insert(params.text_document.uri, change.text);
            self.validate().await;
        }
    }

    async fn did_save(&self, _: DidSaveTextDocumentParams) {
        self.validate().await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
        self.validate().await;
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let Ok(path) = uri.to_file_path() else {
            return Ok(None);
        };
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&uri)
            .cloned()
            .or_else(|| std::fs::read_to_string(&path).ok());
        let Some(source) = source else {
            return Ok(None);
        };

        let options = Options {
            in_dir: &self.in_dir,
            enable_color: false,
        };
        let formatted = super::format::format_source(&options, &path.to_string_lossy(), &source)
            .ok()
            .flatten();

        Ok(formatted.filter(|text| text != &source).map(|new_text| {
            vec![TextEdit {
                range: full_document_range(&source),
                new_text,
            }]
        }))
    }
}

fn replace_source(found: &mut pyre::filesystem::Found, path: &str, source: &str) {
    if let Some(session) = &mut found.session_file {
        if paths_match(&session.path, path) {
            session.content = source.into();
            return;
        }
    }
    for files in found.schema_files.values_mut() {
        for file in files {
            if paths_match(&file.path, path) {
                file.content = source.into();
                return;
            }
        }
    }
}

fn add_new_document(found: &mut pyre::filesystem::Found, in_dir: &Path, path: &str, source: &str) {
    if found
        .query_files
        .iter()
        .any(|existing| paths_match(existing, path))
        || found
            .schema_files
            .values()
            .flatten()
            .any(|existing| paths_match(&existing.path, path))
        || found
            .session_file
            .as_ref()
            .is_some_and(|existing| paths_match(&existing.path, path))
    {
        return;
    }

    let path_buf = Path::new(path);
    let absolute_in_dir = absolute_path(in_dir);
    if !path_buf.starts_with(&absolute_in_dir) {
        return;
    }
    if path_buf
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("pyre")
    {
        return;
    }
    if path_buf.file_name().and_then(|name| name.to_str()) == Some("session.pyre") {
        found.session_file = Some(pyre::filesystem::SchemaFile {
            path: path.into(),
            content: source.into(),
        });
    } else if is_schema_path(in_dir, path_buf) {
        let namespace = pyre::filesystem::get_namespace(path_buf, &absolute_in_dir);
        found
            .schema_files
            .entry(namespace)
            .or_default()
            .push(pyre::filesystem::SchemaFile {
                path: path.into(),
                content: source.into(),
            });
    } else {
        found.query_files.push(path.into());
    }
}

fn is_schema_path(in_dir: &Path, path: &Path) -> bool {
    let absolute_in_dir = absolute_path(in_dir);
    let Ok(relative) = path.strip_prefix(absolute_in_dir) else {
        return path.file_name().and_then(|name| name.to_str()) == Some("schema.pyre");
    };
    relative.file_name().and_then(|name| name.to_str()) == Some("schema.pyre")
        || relative
            .components()
            .any(|component| component.as_os_str() == "schema")
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn parse_errors(
    found: &pyre::filesystem::Found,
    overlays: &HashMap<String, String>,
) -> Vec<pyre::error::Error> {
    let mut errors = Vec::new();
    let schema_sources = found
        .schema_files
        .values()
        .flatten()
        .chain(found.session_file.iter());
    for file in schema_sources {
        let mut schema = pyre::ast::Schema::default();
        if let Err(error) = pyre::parser::run(&file.path, &file.content, &mut schema) {
            if let Some(error) = pyre::parser::convert_parsing_error(error) {
                errors.push(error);
            }
        }
    }
    for path in &found.query_files {
        let source = overlays
            .get(path)
            .cloned()
            .or_else(|| std::fs::read_to_string(path).ok());
        if let Some(source) = source {
            if let Err(error) = pyre::parser::parse_query(path, &source) {
                if let Some(error) = pyre::parser::convert_parsing_error(error) {
                    errors.push(error);
                }
            }
        }
    }
    errors
}

fn paths_match(left: &str, right: &str) -> bool {
    left == right || Path::new(right).ends_with(left) || Path::new(left).ends_with(right)
}

fn filepath_to_url(filepath: &str) -> Option<Url> {
    let path = Path::new(filepath);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    Url::from_file_path(absolute).ok()
}

fn to_lsp_range(range: &pyre::error::Range, source: Option<&str>) -> Range {
    Range {
        start: Position {
            line: range.start.line.saturating_sub(1),
            character: lsp_character(source, range.start.line, range.start.column),
        },
        end: Position {
            line: range.end.line.saturating_sub(1),
            character: lsp_character(source, range.end.line, range.end.column),
        },
    }
}

fn lsp_character(source: Option<&str>, line: u32, column: usize) -> u32 {
    source
        .and_then(|source| source.lines().nth(line.saturating_sub(1) as usize))
        .map(|line| {
            line.chars()
                .take(column.saturating_sub(1))
                .map(char::len_utf16)
                .sum::<usize>() as u32
        })
        .unwrap_or_else(|| column.saturating_sub(1) as u32)
}

fn full_document_range(source: &str) -> Range {
    let mut lines = source.split('\n');
    let mut line = 0;
    let mut character = 0;
    while let Some(text) = lines.next() {
        character = text.encode_utf16().count() as u32;
        if lines.clone().next().is_some() {
            line += 1;
        }
    }
    Range::new(Position::new(0, 0), Position::new(line, character))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_columns_use_utf16_code_units() {
        assert_eq!(lsp_character(Some("a😀b"), 1, 3), 3);
    }

    #[test]
    fn schema_detection_ignores_parent_directory_names() {
        let in_dir = Path::new("/workspace/schema-service/pyre");
        assert!(!is_schema_path(
            in_dir,
            Path::new("/workspace/schema-service/pyre/queries/list.pyre")
        ));
        assert!(is_schema_path(
            in_dir,
            Path::new("/workspace/schema-service/pyre/schema/App/schema.pyre")
        ));
    }

    #[test]
    fn full_document_edits_include_trailing_newline() {
        assert_eq!(
            full_document_range("query Test {}\n").end,
            Position::new(1, 0)
        );
    }
}
