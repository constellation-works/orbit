//! Optional lexical index handle for read-only or unavailable storage.
use std::path::Path;

use orbit_common::OrbitError;

use super::store::LexicalStore;

/// An opened search index, or the explanation for its absence.
#[derive(Clone)]
pub enum LexicalIndex {
    /// The index is open and can answer search reads and writes.
    Ready(LexicalStore),
    /// This storage would not yield a usable index. Carries the
    /// operator-facing explanation reported to every search caller.
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
    /// unreadable storage is unavailable rather than a startup failure.
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        match LexicalStore::open_read_only(path) {
            Ok(store) => Ok(Self::Ready(store)),
            Err(error) if error.is_readonly_or_access_failure() || !path.exists() => {
                Ok(Self::Unavailable(unavailable_reason(path, &error)))
            }
            Err(error) => Err(error),
        }
    }

    /// The open store, or the error every search operation reports when
    /// there is no index to consult.
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
