//! Optional lexical index handle for read-only or unavailable storage.
use std::path::Path;

use orbit_common::OrbitError;

use super::store::LexicalStore;

/// An opened search index, or the explanation for its absence.
#[derive(Clone)]
pub enum LexicalIndex {
    /// The connection opened; individual operations may still fail.
    Ready(LexicalStore),
    /// This storage would not yield a usable index. Carries the
    /// operator-facing reason returned by `store()` to callers that need it.
    Unavailable(String),
}

impl LexicalIndex {
    /// Open the index at `path`, or report it unavailable when the storage
    /// refuses to open or create one.
    ///
    /// Only a readonly/access refusal degrades this way — a read-only mount, a
    /// state directory this caller cannot write, an index whose schema cannot
    /// be built here. Everything else is state that exists and failed: a
    /// malformed file, an unusable WAL, an incompatible layout. Those keep
    /// propagating, so a broken index is never mistaken for an absent one.
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        match LexicalStore::open(path) {
            Ok(store) => Ok(Self::Ready(store)),
            Err(error) if error.is_readonly_or_access_failure() => {
                orbit_common::tracing::warn!(
                    target: "orbit.search.index",
                    path = %path.display(),
                    error = %error,
                    "storage refused the optional search index; continuing without it"
                );
                Ok(Self::Unavailable(unavailable_reason(path, &error)))
            }
            Err(error) => Err(error),
        }
    }

    /// Open an existing index without creating or migrating it. Missing or
    /// access-refused storage is unavailable rather than a startup failure.
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        match LexicalStore::open_read_only(path) {
            Ok(store) => Ok(Self::Ready(store)),
            Err(error) if error.is_readonly_or_access_failure() || !path.exists() => {
                Ok(Self::Unavailable(unavailable_reason(path, &error)))
            }
            Err(error) => Err(error),
        }
    }

    /// Whether indexing writes cannot land through this handle: the store
    /// opened read-only, or no store opened at all because the storage
    /// refused it. Best-effort indexers skip instead of attempting a write
    /// known to fail on every task write; the index then stays stale until
    /// a reindex from writable storage. [ORB-24077]
    pub fn is_read_only(&self) -> bool {
        match self {
            Self::Ready(store) => store.is_read_only(),
            Self::Unavailable(_) => true,
        }
    }

    /// The opened store, or the operator-facing reason it is unavailable.
    pub fn store(&self) -> Result<&LexicalStore, OrbitError> {
        match self {
            Self::Ready(store) => Ok(store),
            Self::Unavailable(reason) => Err(OrbitError::Store(reason.clone())),
        }
    }
}

fn unavailable_reason(path: &Path, error: &OrbitError) -> String {
    format!(
        "search index '{}' is unavailable: this storage will not open or create it ({error}); \
         run `orbit search reindex` where the state directory is writable",
        path.display()
    )
}
