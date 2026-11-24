use super::{valid_component, Bundle, BundleHash};
use std::collections::BTreeMap;
use std::fmt;

/// A verified namespace chain, in execution order from root to head.
#[derive(Debug)]
pub struct History {
    namespace: String,
    bundles: Vec<Bundle>,
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
        Ok(Self {
            namespace: namespace.into(),
            bundles: order
                .into_iter()
                .map(|hash| by_hash.remove(&hash).unwrap())
                .collect(),
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
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
