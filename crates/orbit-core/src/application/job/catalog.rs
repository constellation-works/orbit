use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::activity_job::{
    CatalogDirectory, CatalogDirectoryList, V2JobCatalog, catalog_error_to_orbit,
};
use orbit_types::workflow::{JobKind, JobRun, JobScheduleState, JobV2};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};

/// Shippable default workflow assets, seeded under
/// `<orbit_root>/resources/jobs/<name>.yaml` on `orbit init`. The entries
/// here are the admission-controlled task shipment workflows
/// (auto / gate / local / pr) and the failed-run triage workflow [ORB-10129].
/// Example and smoke fixtures live
/// under `crates/orbit-core/assets/jobs/examples/` and are NOT seeded —
/// they exist for `crates/orbit-engine/examples/v2_job_runtime_smoke.rs`
/// only.
pub(crate) const DEFAULT_JOB_FILES: &[(&str, &str)] = &[
    (
        "agent_invoke_pipeline",
        include_str!("../../../assets/jobs/agent_invoke_pipeline.yaml"),
    ),
    (
        "auto_task_scheduler_pipeline",
        include_str!("../../../assets/jobs/auto_task_scheduler_pipeline.yaml"),
    ),
    (
        "ci_failure_sweep_pipeline",
        include_str!("../../../assets/jobs/ci_failure_sweep_pipeline.yaml"),
    ),
    (
        "dependabot_alert_sweep_pipeline",
        include_str!("../../../assets/jobs/dependabot_alert_sweep_pipeline.yaml"),
    ),
    (
        "epic_pipeline",
        include_str!("../../../assets/jobs/epic_pipeline.yaml"),
    ),
    (
        "task_auto_pipeline",
        include_str!("../../../assets/jobs/task_auto_pipeline.yaml"),
    ),
    (
        "task_gate_pipeline",
        include_str!("../../../assets/jobs/task_gate_pipeline.yaml"),
    ),
    (
        "task_local_pipeline",
        include_str!("../../../assets/jobs/task_local_pipeline.yaml"),
    ),
    (
        "task_pilot_pipeline",
        include_str!("../../../assets/jobs/task_pilot_pipeline.yaml"),
    ),
    (
        "task_pr_pipeline",
        include_str!("../../../assets/jobs/task_pr_pipeline.yaml"),
    ),
    (
        "task_triage_pipeline",
        include_str!("../../../assets/jobs/task_triage_pipeline.yaml"),
    ),
    (
        "workspace_ship_pipeline",
        include_str!("../../../assets/jobs/workspace_ship_pipeline.yaml"),
    ),
    (
        "workspace_auto_pipeline",
        include_str!("../../../assets/jobs/workspace_auto_pipeline.yaml"),
    ),
    (
        "worktree_gc_pipeline",
        include_str!("../../../assets/jobs/worktree_gc_pipeline.yaml"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobCatalogFilter {
    WorkflowsOnly,
    All,
    Kind(JobKind),
}

#[derive(Debug, Clone)]
pub struct JobCatalogEntry {
    pub job_id: String,
    pub path: PathBuf,
    pub spec: JobV2,
}

impl JobCatalogEntry {
    pub fn kind(&self) -> JobKind {
        self.spec.kind
    }

    pub fn state(&self) -> JobScheduleState {
        self.spec.state
    }

    pub fn max_active_runs(&self) -> u32 {
        self.spec.max_active_runs
    }

    pub fn default_input(&self) -> Option<&Value> {
        self.spec.default_input.as_ref()
    }
}

impl OrbitRuntime {
    pub fn list_job_catalog_with_last_run(
        &self,
        include_disabled: bool,
        filter: JobCatalogFilter,
    ) -> Result<Vec<(JobCatalogEntry, Option<JobRun>)>, OrbitError> {
        use orbit_store::contracts::JobRunQuery;

        let v2_jobs = self.load_v2_job_assets()?;
        let mut result = Vec::new();

        for (job_id, path, spec) in v2_jobs.iter() {
            if !include_disabled && spec.state == JobScheduleState::Disabled {
                continue;
            }
            if !matches_job_filter(spec.kind, filter) {
                continue;
            }
            let last_run = self
                .stores()
                .jobs()
                .list_job_runs_filtered(&JobRunQuery {
                    job_id: Some(job_id.to_string()),
                    state: None,
                    terminal_only: false,
                    created_since: None,
                    limit: Some(1),
                    ..Default::default()
                })
                .ok()
                .and_then(|runs| runs.into_iter().next());
            result.push((
                JobCatalogEntry {
                    job_id: job_id.to_string(),
                    path: path.to_path_buf(),
                    spec: spec.clone(),
                },
                last_run,
            ));
        }

        result.sort_by(|left, right| left.0.job_id.cmp(&right.0.job_id));
        Ok(result)
    }

    /// The spec a job would run under, falling back to the asset this binary
    /// ships when the workspace catalog has not been seeded [ORB-11253].
    ///
    /// A control that validates against a job's declared limits must be able to
    /// answer even in a workspace whose `resources/jobs` directory is empty; the
    /// shipped asset is the same document seeding would have written there.
    pub(crate) fn resolved_job_spec(&self, job_id: &str) -> Result<JobV2, OrbitError> {
        match self.show_job_catalog_entry(job_id) {
            Ok(entry) => Ok(entry.spec),
            Err(OrbitError::NotFound { .. }) => shipped_job_spec(job_id),
            Err(error) => Err(error),
        }
    }

    pub fn show_job_catalog_entry(&self, job_id: &str) -> Result<JobCatalogEntry, OrbitError> {
        let v2_jobs = self.load_v2_job_assets()?;
        v2_jobs
            .get(job_id)
            .map(|(path, spec)| JobCatalogEntry {
                job_id: job_id.to_string(),
                path: path.to_path_buf(),
                spec: spec.clone(),
            })
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))
    }

    fn load_v2_job_assets(&self) -> Result<V2JobCatalog, OrbitError> {
        self.load_v2_job_catalog(self.v2_job_asset_dirs())
    }

    fn load_v2_job_catalog(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> Result<V2JobCatalog, OrbitError> {
        let mut catalog = V2JobCatalog::new();
        for dir in dirs {
            if dir.path().is_dir() {
                catalog
                    .load_dir_prefer_existing(dir.path())
                    .map_err(catalog_error_to_orbit)?;
            }
        }
        Ok(catalog)
    }

    fn v2_job_asset_dirs(&self) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        self.v2_job_asset_dirs_with_env(v2_job_env_dirs().as_deref())
    }

    fn v2_job_asset_dirs_with_env(
        &self,
        env_dirs: Option<&str>,
    ) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        let mut dirs = CatalogDirectoryList::default();

        push_v2_job_env_dirs(&mut dirs, env_dirs);
        dirs.push(
            self.paths().jobs_dir.clone(),
            V2JobCatalogDirKind::Workspace,
        );
        dirs.push(
            self.paths().global_dir.join("resources/jobs"),
            V2JobCatalogDirKind::Global,
        );
        dirs.into_vec()
    }

    pub(crate) fn load_v2_job_asset_by_name(
        &self,
        job_id: &str,
    ) -> Result<(PathBuf, JobV2), OrbitError> {
        let catalog = self.load_v2_job_catalog(self.v2_job_asset_dirs_for_execution(job_id))?;
        catalog
            .get(job_id)
            .map(|(path, spec)| (path.to_path_buf(), spec.clone()))
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))
    }

    fn v2_job_asset_dirs_for_execution(
        &self,
        job_id: &str,
    ) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        self.v2_job_asset_dirs_for_execution_with_env(job_id, v2_job_env_dirs().as_deref())
    }

    fn v2_job_asset_dirs_for_execution_with_env(
        &self,
        job_id: &str,
        env_dirs: Option<&str>,
    ) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        let mut dirs = CatalogDirectoryList::default();

        // L-0060 / ORB-00356: name-based execution keeps shipped defaults
        // authoritative over workspace catalogs.
        push_v2_job_env_dirs(&mut dirs, env_dirs);
        dirs.push(
            self.paths().global_dir.join("resources/jobs"),
            V2JobCatalogDirKind::Global,
        );
        if !is_default_job_name(job_id) {
            dirs.push(
                self.paths().jobs_dir.clone(),
                V2JobCatalogDirKind::Workspace,
            );
        }
        dirs.into_vec()
    }
}

fn v2_job_env_dirs() -> Option<String> {
    std::env::var("ORBIT_JOB_DIR")
        .ok()
        .or_else(|| std::env::var("ORBIT_V2_JOB_DIR").ok())
}

fn push_v2_job_env_dirs(
    dirs: &mut CatalogDirectoryList<V2JobCatalogDirKind>,
    env_dirs: Option<&str>,
) {
    if let Some(raw) = env_dirs {
        for entry in raw.split(':').filter(|value| !value.is_empty()) {
            dirs.push(PathBuf::from(entry), V2JobCatalogDirKind::Explicit);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum V2JobCatalogDirKind {
    Explicit,
    Workspace,
    Global,
}

fn is_default_job_name(job_id: &str) -> bool {
    DEFAULT_JOB_FILES
        .iter()
        .any(|(default_job_id, _)| *default_job_id == job_id)
}

fn matches_job_filter(kind: JobKind, filter: JobCatalogFilter) -> bool {
    match filter {
        JobCatalogFilter::WorkflowsOnly => kind == JobKind::Workflow,
        JobCatalogFilter::All => true,
        JobCatalogFilter::Kind(expected) => kind == expected,
    }
}

/// Seed every entry in [`DEFAULT_JOB_FILES`] as a YAML file under
/// `jobs_dir`. Mirrors the activity / skill / policy seeding pattern:
/// the workflow YAML is embedded in the binary via `include_str!` and
/// copied out on `orbit init` so the job loader can discover it without
/// depending on a git checkout of this repo.
///
/// When `overwrite` is false, existing files are preserved — users who've
/// edited a previously-seeded workflow won't lose their changes on re-init.
/// Parse the job asset compiled into this binary.
fn shipped_job_spec(job_id: &str) -> Result<JobV2, OrbitError> {
    let (_, yaml) = DEFAULT_JOB_FILES
        .iter()
        .find(|(name, _)| *name == job_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))?;
    Ok(orbit_engine::activity_job::load_job_asset(yaml)
        .map_err(|error| {
            OrbitError::JobValidation(format!("shipped job asset `{job_id}` is invalid: {error}"))
        })?
        .spec)
}

pub(crate) fn seed_default_jobs(
    jobs_dir: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        jobs_dir,
        "job",
        ManagedAssetLayout::YamlStem,
        DEFAULT_JOB_FILES,
        overwrite,
        |_, content| Ok(Cow::Borrowed(content)),
    )
}
