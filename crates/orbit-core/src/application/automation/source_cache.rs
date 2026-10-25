//! Source facts shared for one scheduler tick or routine inspection request.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use orbit_automation::{AutomationError, DeferredReason};
use orbit_common::OrbitError;
use orbit_types::task::Task;
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
type Preparations = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

#[derive(Default)]
pub(crate) struct SourceCache {
    heads: RefCell<BTreeMap<HeadKey, Cached<SourceRevision>>>,
    lookups: RefCell<BTreeMap<LookupKey, Cached<String>>>,
    inspections: RefCell<BTreeMap<InspectionKey, Cached<InspectionHead>>>,
    // Request-scoped task snapshot shared by the state routines inspected in
    // one status request: the same workspace population and hydrated tasks
    // serve every routine instead of one scan and hydration per routine.
    // Only a status inspection populates it; it lives for one request, so a
    // later request re-reads the store.
    task_snapshots: RefCell<BTreeMap<String, Vec<orbit_types::task::TaskEnvelopeV2>>>,
    // Hydrated tasks and active-preparation scans are workspace-scoped: the
    // status report shares one cache across every discovered workspace, so
    // the workspace key keeps one checkout's tasks from serving another's
    // routines.
    tasks: RefCell<BTreeMap<(String, String), Task>>,
    preparations: RefCell<BTreeMap<(String, &'static str), Preparations>>,
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

    /// Task envelopes of one workspace in the observed statuses, fetched once
    /// per request; each routine's page is derived from this snapshot.
    pub(super) fn task_snapshot(
        &self,
        workspace: &str,
        fetch: impl FnOnce() -> Result<Vec<orbit_types::task::TaskEnvelopeV2>, OrbitError>,
    ) -> Result<Vec<orbit_types::task::TaskEnvelopeV2>, OrbitError> {
        if let Some(snapshot) = self.task_snapshots.borrow().get(workspace) {
            return Ok(snapshot.clone());
        }
        let snapshot = fetch()?;
        self.task_snapshots
            .borrow_mut()
            .insert(workspace.to_string(), snapshot.clone());
        Ok(snapshot)
    }

    pub(super) fn task(
        &self,
        workspace: &str,
        id: &str,
        fetch: impl FnOnce() -> Result<Task, OrbitError>,
    ) -> Result<Task, OrbitError> {
        let key = (workspace.to_string(), id.to_string());
        if let Some(task) = self.tasks.borrow().get(&key) {
            return Ok(task.clone());
        }
        let task = fetch()?;
        self.tasks.borrow_mut().insert(key, task.clone());
        Ok(task)
    }

    pub(super) fn preparations(
        &self,
        workspace: &str,
        kind: &'static str,
        fetch: impl FnOnce() -> Result<Preparations, OrbitError>,
    ) -> Result<Preparations, OrbitError> {
        let key = (workspace.to_string(), kind);
        if let Some(preparations) = self.preparations.borrow().get(&key) {
            return Ok(preparations.clone());
        }
        let preparations = fetch()?;
        self.preparations
            .borrow_mut()
            .insert(key, preparations.clone());
        Ok(preparations)
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
