//! Reliability's catalog role projection, cached by definition bytes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::activity_job;
use orbit_types::workflow::JobActivityRoles;

use crate::OrbitRuntime;

#[derive(Clone)]
struct JobRolesMemo {
    sources: Vec<(PathBuf, Vec<u8>)>,
    job_ids: Vec<String>,
    roles: JobActivityRoles,
}

// Bound workspace churn; eviction affects only parsing cost.
const MAX_JOB_ROLES_MEMO_WORKSPACES: usize = 128;
static JOB_ACTIVITY_ROLES_MEMO: Mutex<BTreeMap<PathBuf, JobRolesMemo>> =
    Mutex::new(BTreeMap::new());

impl OrbitRuntime {
    /// Unions roles over the same catalog as the job listing, including disabled
    /// jobs. Re-read definition bytes on every request so edits remain visible.
    pub(crate) fn job_activity_roles(&self) -> Result<JobActivityRoles, OrbitError> {
        let key = self.paths().jobs_dir.clone();
        let before = self.job_definition_sources();
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
            if diagnostics.is_empty()
                && before.is_some()
                && self.job_definition_sources() == before
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
    fn job_definition_sources(&self) -> Option<Vec<(PathBuf, Vec<u8>)>> {
        let mut paths = Vec::new();
        for dir in self.v2_job_asset_dirs() {
            if dir.path().is_dir() {
                paths.extend(activity_job::job_definition_files(dir.path()).ok()?);
            }
        }
        for plugin in self.plugin_load().active() {
            paths.extend(plugin.definitions.jobs.iter().cloned());
        }
        paths
            .into_iter()
            .map(|path| {
                let bytes = std::fs::read(&path).ok()?;
                Some((path, bytes))
            })
            .collect()
    }
}
