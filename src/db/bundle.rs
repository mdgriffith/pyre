//! Storage-independent format-1 migration artifacts and catalog reads.

#[cfg(feature = "filesystem")]
pub mod filesystem;
mod history;

pub use history::{History, HistoryError};

use crate::{ast, parser, typecheck};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::{fmt, future::Future, str::FromStr};

pub const FORMAT: u32 = 1;
pub const DEFAULT_GRACE_SECONDS: u64 = 86_400;
pub const MANIFEST_PATH: &str = "manifest.json";
pub const MIGRATION_PATH: &str = "migration.sql";
pub const SCHEMA_PATH: &str = "schema.pyre";
const ASSERTIONS_PREFIX: &str = "assertions/";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BundleHash(#[serde(deserialize_with = "deserialize_hash")] String);

fn deserialize_hash<'de, D: serde::Deserializer<'de>>(input: D) -> Result<String, D::Error> {
    String::deserialize(input)?
        .parse::<BundleHash>()
        .map(|hash| hash.0)
        .map_err(serde::de::Error::custom)
}

impl FromStr for BundleHash {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() == 64
            && value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(Self(value.into()))
        } else {
            Err("expected a 64-character lowercase hexadecimal SHA-256 hash")
        }
    }
}

impl fmt::Display for BundleHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct BundleMetadata {
    pub namespace: String,
    pub predecessor: Option<BundleHash>,
    /// The exact, already validated version supplied by the compiler.
    pub target_api_version: String,
    pub grace_seconds: Option<u64>,
    pub external_identities: Vec<ast::ExternalIdentity>,
}

// Field declaration order is the format-1 canonical JSON order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    format: u32,
    namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<BundleHash>,
    // An explicit null is required for a root; omission is not equivalent.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    predecessor: Option<BundleHash>,
    target_api_version: String,
    #[serde(
        default,
        deserialize_with = "deserialize_grace",
        skip_serializing_if = "Option::is_none"
    )]
    grace_seconds: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_identities",
        skip_serializing_if = "Vec::is_empty"
    )]
    external_identities: Vec<ast::ExternalIdentity>,
}

fn deserialize_grace<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(input).map(Some)
}

// Keep manifest decoding strict without changing other identity consumers.
fn deserialize_identities<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Vec<ast::ExternalIdentity>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Identity {
        namespace: String,
        record: String,
        field: String,
        kind: ast::IdentityKind,
    }

    Vec::<Box<serde_json::value::RawValue>>::deserialize(input)?
        .into_iter()
        .map(|raw| {
            if !raw.get().starts_with('{') {
                return Err(serde::de::Error::custom(
                    "external identity must be an object",
                ));
            }
            let i: Identity = serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
            Ok(ast::ExternalIdentity {
                namespace: i.namespace,
                record: i.record,
                field: i.field,
                kind: i.kind,
            })
        })
        .collect()
}

/// A sealed bundle. All access is read-only; catalogs reconstruct it through `verify`.
#[derive(Debug, Clone)]
pub struct Bundle {
    manifest: Manifest, // Always stored without the hash.
    hash: BundleHash,
    files: BTreeMap<String, String>,
}

impl Bundle {
    /// Seal the exact UTF-8 payload files with compiler-supplied metadata.
    pub fn seal(
        metadata: BundleMetadata,
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self, BundleError> {
        Self::build(
            Manifest {
                format: FORMAT,
                namespace: metadata.namespace,
                hash: None,
                predecessor: metadata.predecessor,
                target_api_version: metadata.target_api_version,
                grace_seconds: metadata.grace_seconds,
                external_identities: metadata.external_identities,
            },
            files,
            None,
        )
    }

    /// Read an artifact in its catalog scope. The payload excludes `manifest.json`.
    pub fn verify(
        namespace: &str,
        manifest_json: &[u8],
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self, BundleError> {
        if !manifest_json.trim_ascii_start().starts_with(b"{") {
            return Err(BundleError::ManifestNotObject);
        }
        let mut manifest: Manifest = serde_json::from_slice(manifest_json)?;
        if manifest.namespace != namespace {
            return Err(BundleError::NamespaceMismatch {
                expected: namespace.into(),
                actual: manifest.namespace,
            });
        }
        let claimed = manifest.hash.take().ok_or(BundleError::MissingHash)?;
        Self::build(manifest, files, Some(claimed))
    }

    fn build(
        mut manifest: Manifest,
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
        claimed: Option<BundleHash>,
    ) -> Result<Self, BundleError> {
        if manifest.format != FORMAT {
            return Err(BundleError::UnsupportedFormat(manifest.format));
        }
        if !valid_component(&manifest.namespace) {
            return Err(BundleError::InvalidNamespace(manifest.namespace));
        }
        if manifest.target_api_version.trim().is_empty() {
            return Err(BundleError::EmptyTargetApiVersion);
        }
        match manifest.grace_seconds {
            Some(0) => return Err(BundleError::ZeroGrace),
            Some(DEFAULT_GRACE_SECONDS) => manifest.grace_seconds = None,
            _ => {}
        }
        let identities = &mut manifest.external_identities;
        identities.sort_by(|a, b| identity_key(a).cmp(&identity_key(b)));
        if let Some(pair) = identities
            .windows(2)
            .find(|pair| identity_key(&pair[0]) == identity_key(&pair[1]))
        {
            return Err(BundleError::DuplicateIdentity {
                namespace: pair[0].namespace.clone(),
                record: pair[0].record.clone(),
                field: pair[0].field.clone(),
            });
        }

        let mut payload = BTreeMap::new();
        for (path, bytes) in files {
            if !valid_payload_path(&path) {
                return Err(BundleError::UnexpectedPath(path));
            }
            let text =
                String::from_utf8(bytes).map_err(|_| BundleError::InvalidUtf8(path.clone()))?;
            if payload.insert(path.clone(), text).is_some() {
                return Err(BundleError::DuplicatePath(path));
            }
        }
        for path in [MIGRATION_PATH, SCHEMA_PATH] {
            if !payload.contains_key(path) {
                return Err(BundleError::MissingPath(path));
            }
        }

        let hash = content_hash(&manifest, &payload);
        if let Some(claimed) = claimed {
            if hash != claimed {
                return Err(BundleError::HashMismatch {
                    claimed,
                    computed: hash,
                });
            }
        }
        let bundle = Self {
            manifest,
            hash,
            files: payload,
        };
        bundle.load_target_schema()?;
        Ok(bundle)
    }

    pub fn hash(&self) -> &BundleHash {
        &self.hash
    }

    pub fn namespace(&self) -> &str {
        &self.manifest.namespace
    }

    pub fn predecessor(&self) -> Option<&BundleHash> {
        self.manifest.predecessor.as_ref()
    }

    pub fn target_api_version(&self) -> &str {
        &self.manifest.target_api_version
    }

    pub fn grace_seconds(&self) -> u64 {
        self.manifest.grace_seconds.unwrap_or(DEFAULT_GRACE_SECONDS)
    }

    pub fn external_identities(&self) -> &[ast::ExternalIdentity] {
        &self.manifest.external_identities
    }

    pub fn manifest_json(&self) -> Vec<u8> {
        let mut manifest = self.manifest.clone();
        manifest.hash = Some(self.hash.clone());
        manifest_json(&manifest)
    }

    pub fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    pub fn migration_sql(&self) -> &str {
        &self.files[MIGRATION_PATH]
    }

    pub fn schema_source(&self) -> &str {
        &self.files[SCHEMA_PATH]
    }

    /// Assertion filenames and exact SQL, in filename order.
    pub fn assertions(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files.iter().filter_map(|(path, sql)| {
            path.strip_prefix(ASSERTIONS_PREFIX)
                .map(|filename| (filename, sql.as_str()))
        })
    }

    /// Compile the standalone schema using only this artifact's namespace and identities.
    pub fn load_target_schema(&self) -> Result<typecheck::Context, BundleError> {
        let source = self.schema_source();
        let mut schema = ast::Schema {
            namespace: self.namespace().into(),
            ..ast::Schema::default()
        };
        parser::run(SCHEMA_PATH, source, &mut schema)
            .map_err(|err| BundleError::Schema(parser::render_error(source, err, false)))?;
        let mut database = ast::Database {
            schemas: vec![schema],
        };
        ast::resolve_id_brands(&mut database);
        typecheck::check_schema_with_external_identities(&database, self.external_identities())
            .map_err(|errors| {
                BundleError::Schema(
                    errors
                        .iter()
                        .map(|err| crate::error::format_error(source, err, false))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            })
    }
}

fn manifest_json(manifest: &Manifest) -> Vec<u8> {
    serde_json::to_vec(manifest).expect("a format-1 manifest is JSON serializable")
}

fn identity_key(identity: &ast::ExternalIdentity) -> (&str, &str, &str) {
    (&identity.namespace, &identity.record, &identity.field)
}

fn valid_component(s: &str) -> bool {
    !s.is_empty()
        && !s.ends_with(['.', ' '])
        && !s
            .chars()
            .any(|c| c.is_control() || "<>:/\\|?*\"".contains(c))
}

fn valid_payload_path(path: &str) -> bool {
    matches!(path, MIGRATION_PATH | SCHEMA_PATH)
        || path
            .strip_prefix(ASSERTIONS_PREFIX)
            .is_some_and(|filename| {
                valid_component(filename)
                    && filename
                        .strip_suffix(".sql")
                        .is_some_and(|stem| !stem.is_empty())
            })
}

fn content_hash(manifest: &Manifest, files: &BTreeMap<String, String>) -> BundleHash {
    fn frame(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }

    let mut digest = Sha256::new();
    digest.update(b"pyre-migration-bundle\0v1\0");
    frame(&mut digest, &manifest_json(manifest));
    digest.update((files.len() as u64).to_be_bytes());
    for (path, content) in files {
        frame(&mut digest, path.as_bytes());
        frame(&mut digest, content.as_bytes());
    }
    BundleHash(format!("{:x}", digest.finalize()))
}

/// Read the finalized chain for one namespace from a catalog.
/// Storage implementations verify each artifact before returning a validated `History`.
pub trait Catalog {
    type Error: std::error::Error;

    fn load_history(
        &self,
        namespace: &str,
    ) -> impl Future<Output = Result<History, Self::Error>> + Send;
}

#[derive(Debug)]
pub enum BundleError {
    Json(serde_json::Error),
    ManifestNotObject,
    MissingHash,
    UnsupportedFormat(u32),
    InvalidNamespace(String),
    NamespaceMismatch {
        expected: String,
        actual: String,
    },
    EmptyTargetApiVersion,
    ZeroGrace,
    DuplicateIdentity {
        namespace: String,
        record: String,
        field: String,
    },
    UnexpectedPath(String),
    DuplicatePath(String),
    InvalidUtf8(String),
    MissingPath(&'static str),
    HashMismatch {
        claimed: BundleHash,
        computed: BundleHash,
    },
    Schema(String),
}

impl From<serde_json::Error> for BundleError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid {MANIFEST_PATH}: {error}"),
            Self::ManifestNotObject => write!(f, "{MANIFEST_PATH} must be a JSON object"),
            Self::MissingHash => write!(f, "{MANIFEST_PATH} must contain a bundle hash"),
            Self::UnsupportedFormat(format) => {
                write!(f, "unsupported migration bundle format {format}")
            }
            Self::InvalidNamespace(namespace) => {
                write!(f, "invalid migration namespace {namespace:?}")
            }
            Self::NamespaceMismatch { expected, actual } => {
                write!(f, "expected namespace {expected:?}, found {actual:?}")
            }
            Self::EmptyTargetApiVersion => write!(f, "targetApiVersion must not be blank"),
            Self::ZeroGrace => write!(f, "graceSeconds must be positive"),
            Self::DuplicateIdentity {
                namespace,
                record,
                field,
            } => write!(
                f,
                "duplicate external identity {namespace}.{record}.{field}"
            ),
            Self::UnexpectedPath(path) => write!(f, "unexpected bundle path {path:?}"),
            Self::DuplicatePath(path) => write!(f, "duplicate bundle path {path:?}"),
            Self::InvalidUtf8(path) => write!(f, "bundle file {path:?} is not UTF-8"),
            Self::MissingPath(path) => write!(f, "missing bundle file {path:?}"),
            Self::HashMismatch { claimed, computed } => write!(
                f,
                "bundle hash mismatch: expected {claimed}, computed {computed}"
            ),
            Self::Schema(error) => write!(f, "invalid bundled {SCHEMA_PATH}:\n{error}"),
        }
    }
}

impl std::error::Error for BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}
