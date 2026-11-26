use super::{valid_component, Bundle, BundleError, BundleHash};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A verified namespace chain, in execution order from root to head.
#[derive(Debug)]
pub struct History {
    namespace: String,
    bundles: Vec<Bundle>,
    #[cfg(feature = "database")]
    reserved: BTreeSet<Vec<String>>,
    #[cfg(feature = "database")]
    current: BTreeSet<Vec<String>>,
}

impl History {
    pub fn new(
        namespace: &str,
        bundles: impl IntoIterator<Item = Bundle>,
    ) -> Result<Self, HistoryError> {
        if !valid_component(namespace) {
            return Err(HistoryError::InvalidNamespace(namespace.into()));
        }
        let mut by_hash = BTreeMap::new();
        for bundle in bundles {
            let hash = bundle.hash().clone();
            if bundle.namespace() != namespace {
                return Err(HistoryError::NamespaceMismatch {
                    hash,
                    expected: namespace.into(),
                    actual: bundle.namespace().into(),
                });
            }
            if by_hash.insert(hash.clone(), bundle).is_some() {
                return Err(HistoryError::DuplicateBundle(hash));
            }
        }

        let links = by_hash
            .iter()
            .map(|(hash, bundle)| (hash, bundle.predecessor()))
            .collect();
        let order: Vec<BundleHash> = chain_order(&links)?.into_iter().cloned().collect();
        let bundles: Vec<_> = order
            .into_iter()
            .map(|hash| by_hash.remove(&hash).unwrap())
            .collect();
        let mut predecessor = None;
        let mut reserved = BTreeSet::new();
        let mut previous_names = BTreeSet::new();
        for bundle in &bundles {
            bundle
                .verify_api_transition(predecessor)
                .map_err(|source| HistoryError::ApiVersion {
                    bundle: bundle.hash().clone(),
                    source,
                })?;
            let context =
                bundle
                    .load_target_schema()
                    .map_err(|source| HistoryError::ApiVersion {
                        bundle: bundle.hash().clone(),
                        source,
                    })?;
            let names = crate::schema_contract::published_names(&context);
            if let Some(name) = names
                .difference(&previous_names)
                .find(|name| reserved.contains(*name))
            {
                return Err(HistoryError::ReservedName {
                    bundle: bundle.hash().clone(),
                    name: name.join("."),
                });
            }
            reserved.extend(previous_names.difference(&names).cloned());
            previous_names = names;
            predecessor = Some(bundle);
        }
        Ok(Self {
            namespace: namespace.into(),
            bundles,
            #[cfg(feature = "database")]
            reserved,
            #[cfg(feature = "database")]
            current: previous_names,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn bundles(&self) -> &[Bundle] {
        &self.bundles
    }

    pub fn head(&self) -> Option<&Bundle> {
        self.bundles.last()
    }

    #[cfg(feature = "database")]
    pub(super) fn check_new_names(&self, bundle: &Bundle) -> Result<(), HistoryError> {
        let context = bundle
            .load_target_schema()
            .map_err(|source| HistoryError::ApiVersion {
                bundle: bundle.hash().clone(),
                source,
            })?;
        let names = crate::schema_contract::published_names(&context);
        if let Some(name) = names
            .difference(&self.current)
            .find(|name| self.reserved.contains(*name))
        {
            return Err(HistoryError::ReservedName {
                bundle: bundle.hash().clone(),
                name: name.join("."),
            });
        }
        Ok(())
    }
}

fn chain_order<'a>(
    links: &BTreeMap<&'a BundleHash, Option<&'a BundleHash>>,
) -> Result<Vec<&'a BundleHash>, HistoryError> {
    let mut root = None;
    let mut successors = BTreeMap::new();
    for (&hash, &predecessor) in links {
        if let Some(predecessor) = predecessor {
            if !links.contains_key(predecessor) {
                return Err(HistoryError::MissingPredecessor {
                    bundle: hash.clone(),
                    predecessor: predecessor.clone(),
                });
            }
            if let Some(first) = successors.insert(predecessor, hash) {
                return Err(HistoryError::Fork {
                    predecessor: predecessor.clone(),
                    first: first.clone(),
                    second: hash.clone(),
                });
            }
        } else if let Some(first) = root.replace(hash) {
            return Err(HistoryError::MultipleRoots {
                first: first.clone(),
                second: hash.clone(),
            });
        }
    }

    let mut ordered = Vec::with_capacity(links.len());
    let mut current = root;
    while let Some(hash) = current {
        ordered.push(hash);
        current = successors.remove(hash);
    }
    // With no missing links or forks, anything not reachable from the sole
    // root is a cycle. Its edges remain in `successors`.
    if let Some((&hash, _)) = successors.first_key_value() {
        return Err(HistoryError::Cycle {
            at: hash.clone(),
            disconnected: root.is_some(),
        });
    }
    Ok(ordered)
}

#[derive(Debug)]
pub enum HistoryError {
    ReservedName {
        bundle: BundleHash,
        name: String,
    },
    InvalidNamespace(String),
    NamespaceMismatch {
        hash: BundleHash,
        expected: String,
        actual: String,
    },
    DuplicateBundle(BundleHash),
    MissingPredecessor {
        bundle: BundleHash,
        predecessor: BundleHash,
    },
    Fork {
        predecessor: BundleHash,
        first: BundleHash,
        second: BundleHash,
    },
    MultipleRoots {
        first: BundleHash,
        second: BundleHash,
    },
    Cycle {
        at: BundleHash,
        disconnected: bool,
    },
    ApiVersion {
        bundle: BundleHash,
        source: BundleError,
    },
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedName { bundle, name } => write!(
                f,
                "bundle {bundle}: published name {name} is permanently reserved"
            ),
            Self::InvalidNamespace(namespace) => {
                write!(f, "invalid migration namespace {namespace:?}")
            }
            Self::NamespaceMismatch {
                hash,
                expected,
                actual,
            } => write!(
                f,
                "bundle {hash} belongs to namespace {actual:?}, expected {expected:?}"
            ),
            Self::DuplicateBundle(hash) => write!(f, "duplicate migration bundle {hash}"),
            Self::MissingPredecessor {
                bundle,
                predecessor,
            } => write!(
                f,
                "bundle {bundle} references missing predecessor {predecessor}"
            ),
            Self::Fork {
                predecessor,
                first,
                second,
            } => write!(
                f,
                "bundle {predecessor} has multiple successors: {first} and {second}"
            ),
            Self::MultipleRoots { first, second } => write!(
                f,
                "migration history has multiple roots: {first} and {second}"
            ),
            Self::Cycle {
                at,
                disconnected: false,
            } => write!(f, "migration history has a cycle at {at} and no root"),
            Self::Cycle {
                at,
                disconnected: true,
            } => write!(
                f,
                "migration history has a cycle at {at} disconnected from the root"
            ),
            Self::ApiVersion { bundle, source } => write!(f, "bundle {bundle}: {source}"),
        }
    }
}

impl std::error::Error for HistoryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_cycles_with_or_without_a_root() {
        // A genuine content-addressed cycle would require a SHA-256 fixed
        // point or related preimages. Exercise the graph pass directly.
        let a = "a".repeat(64).parse().unwrap();
        let b = "b".repeat(64).parse().unwrap();
        let root = "0".repeat(64).parse().unwrap();
        assert!(matches!(
            chain_order(&BTreeMap::from([(&a, Some(&a))])),
            Err(HistoryError::Cycle {
                disconnected: false,
                ..
            })
        ));
        assert!(matches!(
            chain_order(&BTreeMap::from([
                (&a, Some(&b)),
                (&b, Some(&a)),
                (&root, None)
            ])),
            Err(HistoryError::Cycle {
                disconnected: true,
                ..
            })
        ));
    }
}
