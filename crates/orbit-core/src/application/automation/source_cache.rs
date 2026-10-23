//! Source facts shared for one scheduler tick or routine inspection request.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use orbit_automation::{AutomationError, DeferredReason};
use orbit_types::workflow::automation::SourceRevision;

type HeadKey = (PathBuf, String, String);
type LookupKey = (String, String, String);
type InspectionKey = (PathBuf, String, bool);
/// `None` records that libgit2 cannot open the checkout, so Git commands run.
type InspectionHead = Option<(String, SourceRevision)>;
type Cached<T> = Result<T, DeferredReason>;

/// A fetched head belongs to one Git object store; local inspection heads are
/// scoped to a checkout and request. Provider responses can be
/// reused across independent clones of the same provider repository. Failures
/// are shared too, so an unavailable source is attempted only once per tick.
#[derive(Default)]
pub(crate) struct SourceCache {
    heads: RefCell<BTreeMap<HeadKey, Cached<SourceRevision>>>,
    lookups: RefCell<BTreeMap<LookupKey, Cached<String>>>,
    inspections: RefCell<BTreeMap<InspectionKey, Cached<InspectionHead>>>,
}

impl SourceCache {
    pub(super) fn inspection(
        &self,
        root: PathBuf,
        branch: &str,
        preparation: bool,
        read: impl FnOnce() -> Result<InspectionHead, AutomationError>,
    ) -> Result<InspectionHead, AutomationError> {
        cached(&self.inspections, (root, branch.into(), preparation), read)
    }

    pub(super) fn head(
        &self,
        git_dir: PathBuf,
        repository: &str,
        branch: &str,
        fetch: impl FnOnce() -> Result<SourceRevision, AutomationError>,
    ) -> Result<SourceRevision, AutomationError> {
        cached(
            &self.heads,
            (git_dir, repository.into(), branch.into()),
            fetch,
        )
    }

    pub(super) fn lookup(
        &self,
        repository: &str,
        branch: &str,
        commit: &str,
        lookup: impl FnOnce() -> Result<String, AutomationError>,
    ) -> Result<String, AutomationError> {
        cached(
            &self.lookups,
            (repository.into(), branch.into(), commit.into()),
            lookup,
        )
    }
}

fn cached<K: Ord, T: Clone>(
    entries: &RefCell<BTreeMap<K, Cached<T>>>,
    key: K,
    query: impl FnOnce() -> Result<T, AutomationError>,
) -> Result<T, AutomationError> {
    if let Some(result) = entries.borrow().get(&key) {
        return result.clone().map_err(AutomationError::Deferred);
    }
    // Release the borrow before a command runs. Source commands have their
    // own bounded deadline; a cache never extends or shares that budget.
    let result = query().map_err(|error| match error {
        AutomationError::Deferred(reason) => reason,
        other => DeferredReason::CachedFailure(other.into()),
    });
    entries.borrow_mut().insert(key, result.clone());
    result.map_err(AutomationError::Deferred)
}
