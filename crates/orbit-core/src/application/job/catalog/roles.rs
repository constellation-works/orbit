//! Reliability's catalog role projection, cached by definition bytes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use orbit_common::OrbitError;
use orbit_engine::activity_job;
use orbit_types::workflow::JobActivityRoles;

use crate::OrbitRuntime;

/// Definition files in loader order, each with the bytes it held when read.
type JobDefinitionSources = Vec<(PathBuf, Arc<[u8]>)>;

#[derive(Clone)]
struct JobRolesMemo {
    sources: JobDefinitionSources,
    job_ids: Vec<String>,
    roles: JobActivityRoles,
}

// Bound workspace churn; eviction affects only parsing cost.
const MAX_JOB_ROLES_MEMO_WORKSPACES: usize = 128;
static JOB_ACTIVITY_ROLES_MEMO: Mutex<BTreeMap<PathBuf, JobRolesMemo>> =
    Mutex::new(BTreeMap::new());

/// One fleet request's reads of job definition directories and files.
///
/// Every workspace's catalog layers the same global and plugin definitions
/// over its own directory, so a fleet read that snapshots each workspace's
/// sources separately walks and reads the shared files once per workspace
/// (ORB-22422: most of each workspace's reliability read). Sharing one value
/// across a request reads each directory and file once and gives every
/// workspace the same snapshot of the shared files. Nothing outlives the
/// value, so the next request reads the disk again and an edit stays visible.
/// Create one per request; never keep one across requests.
#[derive(Debug, Default)]
pub struct JobDefinitionReads {
    listings: ReadOnce<DirectoryListing>,
    files: ReadOnce<Option<Arc<[u8]>>>,
    /// Staggers where each concurrent snapshot starts reading, so the first
    /// workspaces read different files in parallel instead of queueing behind
    /// one reader in loader order.
    snapshots_started: AtomicUsize,
}

#[derive(Debug, Clone)]
enum DirectoryListing {
    /// Not a directory: the layer contributes nothing.
    Absent,
    Files(Arc<[PathBuf]>),
    /// The walk failed, so no complete source snapshot exists.
    Unreadable,
}

impl JobDefinitionReads {
    fn listing(&self, dir: &Path) -> DirectoryListing {
        self.listings.get_or_read(dir, || {
            if !dir.is_dir() {
                return DirectoryListing::Absent;
            }
            match activity_job::job_definition_files(dir) {
                Ok(files) => DirectoryListing::Files(files.into()),
                Err(_) => DirectoryListing::Unreadable,
            }
        })
    }

    fn file(&self, path: &Path) -> Option<Arc<[u8]>> {
        self.files
            .get_or_read(path, || std::fs::read(path).ok().map(Arc::from))
    }

    /// Every file's bytes in `paths` order, or `None` when one is unreadable.
    fn files_in_order(&self, paths: Vec<PathBuf>) -> Option<JobDefinitionSources> {
        let count = paths.len();
        if count == 0 {
            return Some(Vec::new());
        }
        let stride = count.div_ceil(MAX_STAGGERED_SNAPSHOTS).max(1);
        let start = self
            .snapshots_started
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(stride)
            % count;
        let mut bytes = vec![None; count];
        for index in (0..count).map(|offset| (start + offset) % count) {
            bytes[index] = Some(self.file(&paths[index])?);
        }
        paths
            .into_iter()
            .zip(bytes)
            .map(|(path, bytes)| Some((path, bytes?)))
            .collect()
    }
}

/// Distinct read start points across one request's concurrent snapshots;
/// matches the fleet read's fan-out bound.
const MAX_STAGGERED_SNAPSHOTS: usize = 8;

/// Path-keyed reads that run once: workspaces resolved concurrently wait for
/// the first reader of a path instead of repeating its read, and all of them
/// see the value it read.
#[derive(Debug)]
struct ReadOnce<T> {
    slots: Mutex<HashMap<PathBuf, Arc<OnceLock<T>>>>,
}

impl<T> Default for ReadOnce<T> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: Clone> ReadOnce<T> {
    fn get_or_read(&self, path: &Path, read: impl FnOnce() -> T) -> T {
        // The map only hands out slots, so a panicked holder cannot leave it
        // worse than a missing entry; the read itself runs outside this lock.
        let slot = Arc::clone(
            self.slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(path.to_path_buf())
                .or_default(),
        );
        slot.get_or_init(read).clone()
    }
}

impl OrbitRuntime {
    /// Unions roles over the same catalog as the job listing, including disabled
    /// jobs. Definition bytes come from `reads`, which one request shares, so
    /// edits remain visible on the next request.
    pub(crate) fn job_activity_roles(
        &self,
        reads: &JobDefinitionReads,
    ) -> Result<JobActivityRoles, OrbitError> {
        let key = self.paths().jobs_dir.clone();
        let before = self.job_definition_sources(reads);
        let cached = before.as_ref().and_then(|sources| {
            let memo = JOB_ACTIVITY_ROLES_MEMO.lock().ok()?;
            memo.get(&key)
                .filter(|entry| &entry.sources == sources)
                .map(|entry| (entry.job_ids.clone(), entry.roles.clone()))
        });
        let (job_ids, roles) = if let Some(cached) = cached {
            cached
        } else {
            let (catalog, diagnostics) =
                self.load_v2_job_catalog_with_diagnostics(self.v2_job_asset_dirs())?;
            let mut roles = JobActivityRoles::default();
            let mut job_ids = Vec::new();
            for (job_id, _, spec) in catalog.iter() {
                job_ids.push(job_id.to_string());
                roles.merge(&spec.activity_roles());
            }
            let projection = JobRolesMemo {
                sources: before.clone().unwrap_or_default(),
                job_ids,
                roles,
            };
            // Memoize only when a fresh read after parsing still matches the
            // snapshot, so an edit racing the parse is never pinned.
            if diagnostics.is_empty()
                && before.is_some()
                && self.job_definition_sources(&JobDefinitionReads::default()) == before
                && let Ok(mut memo) = JOB_ACTIVITY_ROLES_MEMO.lock()
            {
                if memo.len() >= MAX_JOB_ROLES_MEMO_WORKSPACES && !memo.contains_key(&key) {
                    memo.pop_first();
                }
                memo.insert(key, projection.clone());
            }
            (projection.job_ids, projection.roles)
        };
        // The original listing read and decoded latest runs even though this
        // projection only uses roles. Preserve its store/decode errors on hits
        // as well as misses, rather than making a corrupt run disappear.
        self.stores().jobs().latest_job_runs(&job_ids)?;
        Ok(roles)
    }

    /// Snapshot the loader's inputs in precedence order, without sorting across
    /// layers: swapping two directories must invalidate a first-wins catalog.
    fn job_definition_sources(&self, reads: &JobDefinitionReads) -> Option<JobDefinitionSources> {
        let mut paths = Vec::new();
        for dir in self.v2_job_asset_dirs() {
            match reads.listing(dir.path()) {
                DirectoryListing::Absent => {}
                DirectoryListing::Files(files) => paths.extend(files.iter().cloned()),
                DirectoryListing::Unreadable => return None,
            }
        }
        for plugin in self.plugin_load().active() {
            paths.extend(plugin.definitions.jobs.iter().cloned());
        }
        reads.files_in_order(paths)
    }
}
