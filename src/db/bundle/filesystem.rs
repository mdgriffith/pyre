use super::{
    valid_component, valid_payload_path, Bundle, BundleError, Catalog, History, HistoryError,
    ASSERTIONS_PREFIX, MANIFEST_PATH,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Read finalized bundles from `<migration_root>/<namespace>/<folder>/`.
/// The caller supplies a stable tree for the duration of the read.
pub struct FilesystemCatalog {
    migration_root: PathBuf,
}

impl FilesystemCatalog {
    pub fn new(migration_root: impl Into<PathBuf>) -> Self {
        Self {
            migration_root: migration_root.into(),
        }
    }

    /// A folder without a manifest is a draft and does not enter finalized history.
    pub fn read_history(&self, namespace: &str) -> Result<History, Error> {
        self.read_named_history(namespace)
            .map(|(history, _)| history)
    }

    pub fn read_named_history(
        &self,
        namespace: &str,
    ) -> Result<
        (
            History,
            std::collections::BTreeMap<String, super::BundleHash>,
        ),
        Error,
    > {
        if !valid_component(namespace) {
            return Err(Error::History(HistoryError::InvalidNamespace(
                namespace.into(),
            )));
        }
        let scope = self.migration_root.join(namespace);
        let mut bundles = Vec::new();
        let mut names = std::collections::BTreeMap::new();
        if let Some(metadata) = optional_metadata(&scope)? {
            if !metadata.is_dir() {
                return Err(invalid_entry(&scope, "expected a namespace directory"));
            }
            let mut directories = entries(&scope)?;
            directories.sort_unstable_by_key(|entry| entry.file_name());
            for entry in directories {
                let path = entry.path();
                let kind = at(&path, entry.file_type())?;
                if kind.is_symlink() {
                    return Err(invalid_entry(
                        &path,
                        "unexpected symbolic link in migration namespace",
                    ));
                }
                if !kind.is_dir() {
                    continue;
                }
                if let Some(manifest) = optional_metadata(&path.join(MANIFEST_PATH))? {
                    let bundle = read_bundle(namespace, &path, manifest)?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| invalid_entry(&path, "folder name must be UTF-8"))?;
                    names.insert(name, bundle.hash().clone());
                    bundles.push(bundle);
                }
            }
        }
        Ok((
            History::new(namespace, bundles).map_err(Error::History)?,
            names,
        ))
    }
}

#[cfg(feature = "database")]
pub mod authoring;

impl Catalog for FilesystemCatalog {
    type Error = Error;

    // Filesystem reading is synchronous; async hosts can schedule read_history
    // on their blocking pool when loading large catalogs.
    async fn load_history(&self, namespace: &str) -> Result<History, Self::Error> {
        self.read_history(namespace)
    }
}

fn read_bundle(namespace: &str, directory: &Path, manifest: fs::Metadata) -> Result<Bundle, Error> {
    let path = directory.join(MANIFEST_PATH);
    if !manifest.is_file() {
        return Err(invalid_entry(&path, "expected a regular manifest file"));
    }
    let manifest = at(&path, fs::read(&path))?;
    let mut files = Vec::new();
    for entry in entries(directory)? {
        let name = entry.file_name();
        if name == MANIFEST_PATH {
            continue;
        }
        if name == "assertions" {
            let path = entry.path();
            if !at(&path, entry.file_type())?.is_dir() {
                return Err(invalid_entry(&path, "expected an assertions directory"));
            }
            for assertion in entries(&path)? {
                files.push(read_payload(assertion, ASSERTIONS_PREFIX)?);
            }
        } else {
            files.push(read_payload(entry, "")?);
        }
    }
    Bundle::verify(namespace, &manifest, files).map_err(|source| Error::Bundle {
        path: directory.into(),
        source,
    })
}

fn read_payload(entry: fs::DirEntry, prefix: &str) -> Result<(String, Vec<u8>), Error> {
    let path = entry.path();
    let filename = entry.file_name();
    let filename = filename
        .to_str()
        .ok_or_else(|| invalid_entry(&path, "file name is not UTF-8"))?;
    let relative = format!("{prefix}{filename}");
    if !valid_payload_path(&relative) || !at(&path, entry.file_type())?.is_file() {
        return Err(invalid_entry(
            &path,
            "unexpected bundle entry; expected a regular format-1 payload file",
        ));
    }
    Ok((relative, at(&path, fs::read(&path))?))
}

fn entries(path: &Path) -> Result<Vec<fs::DirEntry>, Error> {
    at(path, fs::read_dir(path))?
        .map(|entry| at(path, entry))
        .collect()
}

fn optional_metadata(path: &Path) -> Result<Option<fs::Metadata>, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => at(path, Err(error)),
    }
}

fn at<T>(path: &Path, result: io::Result<T>) -> Result<T, Error> {
    result.map_err(|source| Error::Io {
        path: path.into(),
        source,
    })
}

fn invalid_entry(path: &Path, message: &str) -> Error {
    Error::Io {
        path: path.into(),
        source: io::Error::new(io::ErrorKind::InvalidData, message),
    }
}

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, source: io::Error },
    Bundle { path: PathBuf, source: BundleError },
    History(HistoryError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Bundle { path, source } => write!(f, "{}: {source}", path.display()),
            Self::History(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Bundle { source, .. } => Some(source),
            Self::History(error) => Some(error),
        }
    }
}
