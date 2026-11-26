//! Filesystem drafts and serialized publication of immutable artifacts.

use super::{
    at, entries, invalid_entry, optional_metadata, read_payload, valid_component, Error,
    FilesystemCatalog, ASSERTIONS_PREFIX, MANIFEST_PATH,
};
use crate::db::bundle::{
    author::{self, Draft},
    Bundle, BundleHash, History, MIGRATION_PATH, SCHEMA_PATH,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const DRAFT: &str = "draft.json";
const MANIFEST_TEMP: &str = ".pyre-pending-manifest";
const EVIDENCE_DIR: &str = ".pyre-use";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    predecessor: Option<BundleHash>,
    grace_seconds: Option<u64>,
    external_identities: Vec<crate::ast::ExternalIdentity>,
}

impl FilesystemCatalog {
    pub fn write_draft(&self, name: &str, draft: &Draft) -> Result<PathBuf, Error> {
        let path = self.draft_path(&draft.namespace, name)?;
        let _lock = lock(path.parent().unwrap())?;
        if self
            .read_history(&draft.namespace)?
            .head()
            .map(Bundle::hash)
            != draft.predecessor.as_ref()
        {
            return Err(invalid_entry(
                &path,
                "draft predecessor changed during generation",
            ));
        }
        at(&path, fs::create_dir(&path))?;
        write_file(&path.join(MIGRATION_PATH), draft.sql.as_bytes())?;
        write_file(&path.join(SCHEMA_PATH), draft.schema.as_bytes())?;
        if !draft.assertions.is_empty() {
            let assertions = path.join("assertions");
            at(&assertions, fs::create_dir(&assertions))?;
            for (name, sql) in &draft.assertions {
                if !super::valid_payload_path(&format!("assertions/{name}")) {
                    return Err(invalid_entry(&assertions, "invalid assertion filename"));
                }
                write_file(&assertions.join(name), sql.as_bytes())?;
            }
        }
        write_metadata(&path, draft)?;
        Ok(path)
    }

    pub fn draft(&self, namespace: &str, name: &str) -> Result<Draft, Error> {
        let path = self.draft_path(namespace, name)?;
        if !at(&path, fs::symlink_metadata(&path))?.is_dir()
            || optional_metadata(&path.join(MANIFEST_PATH))?.is_some()
        {
            return Err(invalid_entry(
                &path,
                "expected an unfinalized draft directory",
            ));
        }
        let metadata_path = path.join(DRAFT);
        let raw = read_regular(&metadata_path)?;
        let meta: Metadata = serde_json::from_slice(&raw)
            .map_err(|e| invalid_entry(&metadata_path, &e.to_string()))?;
        let mut draft = Draft {
            namespace: namespace.into(),
            predecessor: meta.predecessor,
            grace_seconds: meta.grace_seconds,
            external_identities: meta.external_identities,
            schema: String::new(),
            sql: String::new(),
            assertions: Default::default(),
        };
        for entry in entries(&path)? {
            let item = entry.path();
            match entry.file_name().to_str() {
                Some(DRAFT) => {}
                Some(MIGRATION_PATH) => draft.sql = read_text(&item)?,
                Some(SCHEMA_PATH) => draft.schema = read_text(&item)?,
                Some("assertions") if at(&item, entry.file_type())?.is_dir() => {
                    for assertion in entries(&item)? {
                        let (relative, bytes) = read_payload(assertion, ASSERTIONS_PREFIX)?;
                        let sql = String::from_utf8(bytes)
                            .map_err(|e| invalid_entry(&item, &e.to_string()))?;
                        draft
                            .assertions
                            .insert(relative[ASSERTIONS_PREFIX.len()..].into(), sql);
                    }
                }
                _ => return Err(invalid_entry(&item, "unexpected draft entry")),
            }
        }
        for file in [MIGRATION_PATH, SCHEMA_PATH] {
            if optional_metadata(&path.join(file))?.is_none() {
                return Err(invalid_entry(
                    &path.join(file),
                    "required draft file is missing",
                ));
            }
        }
        Ok(draft)
    }

    pub async fn finalize(
        &self,
        namespace: &str,
        name: &str,
        grace_override: Option<u64>,
    ) -> Result<Bundle, Error> {
        let path = self.draft_path(namespace, name)?;
        let mut draft = self.draft(namespace, name)?;
        let original = serde_json::to_vec(&draft).expect("draft serializes");
        if grace_override.is_some() {
            draft.grace_seconds = grace_override;
        }
        let history = self.read_history(namespace)?;
        let (candidate, _) = draft
            .candidate(&history)
            .map_err(|e| invalid_entry(&path, &e))?;
        author::check(&history, Some(&candidate), None)
            .await
            .map_err(|e| invalid_entry(&path, &e))?;
        let scope = path.parent().unwrap();
        let _lock = lock(scope)?;
        if self.read_history(namespace)?.head().map(Bundle::hash) != candidate.predecessor()
            || serde_json::to_vec(&self.draft(namespace, name)?).expect("draft serializes")
                != original
        {
            return Err(invalid_entry(
                &path,
                "draft or catalog head changed during replay; recheck before finalizing",
            ));
        }
        let temp = scope.join(MANIFEST_TEMP);
        write_file(&temp, &candidate.manifest_json())?;
        let metadata = path.join(DRAFT);
        let old_metadata = read_regular(&metadata)?;
        at(&metadata, fs::remove_file(&metadata))?;
        let manifest_path = path.join(MANIFEST_PATH);
        let result = at(&manifest_path, fs::rename(&temp, &manifest_path));
        if result.is_err() {
            write_file(&metadata, &old_metadata)?;
        }
        result?;
        Ok(candidate)
    }

    /// Mark a history as potentially used before attempting a CLI application.
    /// This fails closed when the target's commit outcome is uncertain.
    pub fn note_application(&self, history: &History) -> Result<(), Error> {
        let scope = self.migration_root.join(history.namespace());
        let _lock = lock(&scope)?;
        let current = self.read_history(history.namespace())?;
        if history
            .bundles()
            .iter()
            .zip(current.bundles())
            .any(|(a, b)| a.hash() != b.hash())
            || current.bundles().len() < history.bundles().len()
        {
            return Err(invalid_entry(
                &scope,
                "catalog changed before application; reload finalized history",
            ));
        }
        let evidence = scope.join(EVIDENCE_DIR);
        if optional_metadata(&evidence)?.is_none() {
            at(&evidence, fs::create_dir(&evidence))?;
        }
        for bundle in history.bundles() {
            let marker = evidence.join(bundle.hash().to_string());
            if optional_metadata(&marker)?.is_none() {
                write_file(&marker, &[])?;
            }
        }
        Ok(())
    }

    /// The caller must attest the artifact is unshared. The CLI additionally
    /// checks Git history and the catalog's local-use evidence.
    pub fn unfinalize(
        &self,
        namespace: &str,
        name: &str,
        known_shared: impl FnOnce(&Path) -> bool,
    ) -> Result<(), Error> {
        let path = self.draft_path(namespace, name)?;
        let scope = path.parent().unwrap();
        let _lock = lock(scope)?;
        let (history, names) = self.read_named_history(namespace)?;
        let head = history
            .head()
            .filter(|b| names.get(name) == Some(b.hash()))
            .ok_or_else(|| invalid_entry(&path, "only the finalized head can be unfinalized"))?;
        let manifest = path.join(MANIFEST_PATH);
        if optional_metadata(&scope.join(EVIDENCE_DIR).join(head.hash().to_string()))?.is_some()
            || known_shared(&manifest)
        {
            return Err(invalid_entry(
                &path,
                "migration has local-use or sharing evidence; create a follow-up migration",
            ));
        }
        let metadata = path.join(DRAFT);
        let content = Metadata {
            predecessor: head.predecessor().cloned(),
            grace_seconds: Some(head.grace_seconds()),
            external_identities: head.external_identities().to_vec(),
        };
        let temp = scope.join(MANIFEST_TEMP);
        write_file(
            &temp,
            &serde_json::to_vec_pretty(&content).expect("draft metadata serializes"),
        )?;
        at(&manifest, fs::remove_file(&manifest))?;
        at(&metadata, fs::rename(&temp, &metadata))?;
        Ok(())
    }

    fn draft_path(&self, namespace: &str, name: &str) -> Result<PathBuf, Error> {
        if !valid_component(namespace) || !valid_component(name) || name.starts_with('.') {
            return Err(invalid_entry(
                &self.migration_root,
                "invalid draft name or namespace",
            ));
        }
        Ok(self.migration_root.join(namespace).join(name))
    }
}

fn lock(scope: &Path) -> Result<fs::File, Error> {
    at(scope, fs::create_dir_all(scope))?;
    if !at(scope, fs::symlink_metadata(scope))?.is_dir() {
        return Err(invalid_entry(
            scope,
            "expected a regular namespace directory",
        ));
    }
    let path = scope.join(".pyre-author.lock");
    let lock = at(
        &path,
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path),
    )?;
    at(&path, lock.lock())?;
    Ok(lock)
}

fn write_metadata(path: &Path, draft: &Draft) -> Result<(), Error> {
    write_file(
        &path.join(DRAFT),
        &serde_json::to_vec_pretty(&Metadata {
            predecessor: draft.predecessor.clone(),
            grace_seconds: draft.grace_seconds,
            external_identities: draft.external_identities.clone(),
        })
        .expect("draft metadata serializes"),
    )
}

fn read_regular(path: &Path) -> Result<Vec<u8>, Error> {
    if !at(path, fs::symlink_metadata(path))?.is_file() {
        return Err(invalid_entry(path, "expected a regular file"));
    }
    at(path, fs::read(path))
}

fn read_text(path: &Path) -> Result<String, Error> {
    String::from_utf8(read_regular(path)?).map_err(|e| invalid_entry(path, &e.to_string()))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use io::Write;
    let mut file = at(
        path,
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path),
    )?;
    at(path, file.write_all(bytes))?;
    at(path, file.sync_all())
}
