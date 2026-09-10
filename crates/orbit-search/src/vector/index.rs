//! The optional semantic index handle: an open [`VectorStore`], or the
//! explained reason there is none.
//!
//! Unlike the task registry and the audit database, the semantic index is not
//! authoritative state. A workspace that never ran `orbit semantic index` has
//! no database at all, and Orbit must still open — including on a read-only
//! mount, where creating one is impossible. Storage that refuses the index is
//! therefore a startup outcome rather than a startup failure.
//!
//! It is not an empty corpus, though. A caller that asked for semantic ranking
//! gets the unavailability by name from [`SemanticIndex::store`], never a
//! complete-looking empty result it cannot tell apart from a genuinely
//! unindexed workspace.

use std::path::Path;

use orbit_common::OrbitError;

use super::store::VectorStore;

/// An opened semantic index, or the explanation for its absence.
#[derive(Clone)]
pub enum SemanticIndex {
    /// The index is open and can answer semantic reads and writes.
    Ready(VectorStore),
    /// This storage would not yield a usable index. Carries the
    /// operator-facing explanation reported to every semantic caller.
    Unavailable(String),
}

impl SemanticIndex {
    /// Open the index at `path`, or report it unavailable when the storage
    /// refuses to open or create one.
    ///
    /// Only a readonly/access refusal degrades this way — a read-only mount, a
    /// state directory this caller cannot write, an index whose schema cannot
    /// be built here. Everything else is state that exists and failed: a
    /// malformed file, an unusable WAL, an incompatible layout. Those keep
    /// propagating, so a broken index is never mistaken for an absent one.
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        match VectorStore::open(path) {
            Ok(store) => Ok(Self::Ready(store)),
            Err(error) if error.is_readonly_or_access_failure() => {
                orbit_common::tracing::warn!(
                    target: "orbit.search.vector",
                    path = %path.display(),
                    error = %error,
                    "storage refused the optional semantic index; continuing without it"
                );
                Ok(Self::Unavailable(unavailable_reason(path, &error)))
            }
            Err(error) => Err(error),
        }
    }

    /// The open store, or the error every semantic operation reports when
    /// there is no index to consult.
    pub fn store(&self) -> Result<&VectorStore, OrbitError> {
        match self {
            Self::Ready(store) => Ok(store),
            Self::Unavailable(reason) => Err(OrbitError::Store(reason.clone())),
        }
    }
}

fn unavailable_reason(path: &Path, error: &OrbitError) -> String {
    format!(
        "semantic index '{}' is unavailable: this storage will not open or create it ({error}); \
         run `orbit semantic index` where the state directory is writable",
        path.display()
    )
}
