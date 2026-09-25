use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::activity_job::{
    CatalogDirectory, CatalogDirectoryList, CatalogError, V2JobCatalog, catalog_error_to_orbit,
};
use orbit_types::workflow::{JobKind, JobRun, JobScheduleState, JobV2};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::managed_assets::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};

#[cfg(test)]
thread_local! {
    static V2_JOB_CATALOG_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_v2_job_catalog_loads() {
    V2_JOB_CATALOG_LOADS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn v2_job_catalog_loads() -> usize {
    V2_JOB_CATALOG_LOADS.with(std::cell::Cell::get)
}

/// Shippable default workflow assets. The list lives beside the shipped
/// activities in `runtime::assets` so the runtime kernel can answer "is this a
/// job Orbit ships" — a plugin routine may target one — without reaching up
/// into the application layer.
pub(crate) use crate::runtime::assets::DEFAULT_JOB_FILES;

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

/// Names and diagnostics collected while building the membership index used by
/// routine discovery. A broken lower-precedence layer must not erase names
/// from a layer that still resolves for execution, but its error must remain
/// available to the caller for operator-visible reporting.
#[derive(Debug)]
pub(crate) struct V2JobExecutionMembership {
    pub(crate) names: BTreeSet<String>,
    pub(crate) errors: Vec<OrbitError>,
}

#[derive(Debug)]
struct V2JobCatalogDiagnostic {
    directory_index: usize,
    path: PathBuf,
    error: OrbitError,
}

impl V2JobCatalogDiagnostic {
    fn applies_to_job(
        &self,
        job_id: &str,
        selected_path: Option<&Path>,
        dirs: &[CatalogDirectory<V2JobCatalogDirKind>],
    ) -> bool {
        if self.path.file_stem().and_then(|stem| stem.to_str()) != Some(job_id) {
            return false;
        }
        let Some(selected_path) = selected_path else {
            return true;
        };
        let selected_directory_index = dirs
            .iter()
            .position(|dir| selected_path.starts_with(dir.path()))
            .unwrap_or(usize::MAX);
        self.directory_index <= selected_directory_index
    }
}

impl OrbitRuntime {
    /// Capture integration identity at named submission, before the worker
    /// merges job defaults. Both collection and pilot admission then consume
    /// the same durable input, independent of GitHub's release default.
    pub(crate) fn resolve_ci_sweep_input(
        &self,
        spec: &JobV2,
        input: &mut Value,
    ) -> Result<(), OrbitError> {
        if input.is_null() {
            *input = serde_json::json!({});
        }
        if !input.is_object() {
            return Err(OrbitError::InvalidInput(
                "CI sweep run input must be an object".to_string(),
            ));
        }

        let branch = self.ci_sweep_integration_branch(spec, input)?;
        let branch = validate_ci_integration_branch(&self.paths().repo_root, &branch)?;
        input["integration_branch"] = Value::String(branch);
        Ok(())
    }

    fn ci_sweep_integration_branch(
        &self,
        spec: &JobV2,
        input: &Value,
    ) -> Result<String, OrbitError> {
        for key in ["integration_branch", "base_branch"] {
            if let Some(branch) = ci_branch_field(input, key)? {
                return Ok(branch);
            }
        }
        if let Some(defaults) = spec.default_input.as_ref()
            && let Some(branch) = ci_branch_field(defaults, "integration_branch")?
        {
            return Ok(branch);
        }
        if let Some(branch) = self
            .workspace_runtime_binding()
            .and_then(|binding| binding.base_branch.as_ref())
        {
            return Ok(branch.clone());
        }

        // Standalone workspaces can explicitly configure integration identity.
        // The built-in "main" fallback is not evidence of that identity.
        let config = orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
            self.global_root(),
            self.shared_root(),
        ))?;
        config
            .values()
            .iter()
            .find(|entry| {
                entry.key == "workflow.base_branch"
                    && entry.source.kind() != orbit_config::ConfigValueSourceKind::BuiltIn
            })
            .and_then(|entry| entry.value.as_str())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "CI sweep integration branch is unavailable; set the registered workspace \
                     base branch, explicit workflow.base_branch config, or integration_branch \
                     run input"
                        .to_string(),
                )
            })
    }

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
                })?
                .into_iter()
                .next();
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
        let dirs = self.v2_job_asset_dirs();
        let (v2_jobs, diagnostics) = self.load_v2_job_catalog_with_diagnostics(dirs.clone())?;
        let selected_path = v2_jobs.get(job_id).map(|(path, _)| path.to_path_buf());
        if let Some(diagnostic) = diagnostics
            .into_iter()
            .find(|diagnostic| diagnostic.applies_to_job(job_id, selected_path.as_deref(), &dirs))
        {
            return Err(diagnostic.error);
        }
        if let Some((path, spec)) = v2_jobs.get(job_id) {
            return Ok(JobCatalogEntry {
                job_id: job_id.to_string(),
                path: path.to_path_buf(),
                spec: spec.clone(),
            });
        }
        Err(OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))
    }

    fn load_v2_job_assets(&self) -> Result<V2JobCatalog, OrbitError> {
        self.load_v2_job_catalog(self.v2_job_asset_dirs())
    }

    /// Job names [`Self::load_v2_job_asset_by_name`] would resolve, parsed once
    /// so routine collection can check membership without re-reading every YAML
    /// per definition.
    ///
    /// Default job names stay bound to env/global layers (L-0060): a workspace
    /// copy of a shipped default does not make the name resolvable for
    /// execution, so it must not pass load-time target checks either.
    ///
    /// That distinction only means anything when the workspace jobs
    /// directory is actually separate from the global one. In the common
    /// single-root layout (`orbit init` and `workspace init` sharing one
    /// `--root`, as most CLI-driven workspaces do) `jobs_dir` and
    /// `global_dir/resources/jobs` are the very same path, so every entry's
    /// on-disk path trivially starts with `jobs_dir` — the path alone can't
    /// tell "came from the workspace copy" apart from "came from the shared
    /// global directory". Skip the exclusion entirely when the two paths
    /// coincide, rather than filtering out every default job name.
    #[allow(dead_code)]
    pub(crate) fn load_v2_job_execution_names(&self) -> Result<BTreeSet<String>, OrbitError> {
        Ok(self.load_v2_job_execution_membership().names)
    }

    /// Build the execution-name index once while retaining errors from any
    /// layer that could not be loaded. The named execution path remains strict
    /// and will re-read its eligible directories before dispatch.
    pub(crate) fn load_v2_job_execution_membership(&self) -> V2JobExecutionMembership {
        let (catalog, errors) = self.load_v2_job_catalog_best_effort(self.v2_job_membership_dirs());
        let jobs_dir = &self.paths().jobs_dir;
        let global_jobs_dir = self.paths().global_dir.join("resources/jobs");
        let workspace_dir_is_distinct = *jobs_dir != global_jobs_dir;
        let names = catalog
            .iter()
            .filter_map(|(name, path, _)| {
                if workspace_dir_is_distinct
                    && is_default_job_name(name)
                    && path.starts_with(jobs_dir)
                {
                    None
                } else {
                    Some(name.to_string())
                }
            })
            .collect();
        V2JobExecutionMembership { names, errors }
    }

    fn load_v2_job_catalog(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> Result<V2JobCatalog, OrbitError> {
        self.load_v2_job_catalog_with_diagnostics(dirs)
            .map(|(catalog, _)| catalog)
    }

    fn load_v2_job_catalog_with_diagnostics(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> Result<(V2JobCatalog, Vec<V2JobCatalogDiagnostic>), OrbitError> {
        #[cfg(test)]
        V2_JOB_CATALOG_LOADS.with(|count| count.set(count.get() + 1));
        let mut catalog = V2JobCatalog::new();
        let mut diagnostics = Vec::new();
        for (directory_index, dir) in dirs.into_iter().enumerate() {
            if dir.path().is_dir() {
                match catalog.load_dir_prefer_existing_best_effort(dir.path()) {
                    Ok(parse_errors) => {
                        diagnostics.extend(
                            parse_errors
                                .into_iter()
                                .map(|error| {
                                    let path = match &error {
                                        CatalogError::Parse { path, .. } => path.clone(),
                                        _ => return Err(catalog_error_to_orbit(error)),
                                    };
                                    Ok(V2JobCatalogDiagnostic {
                                        directory_index,
                                        path,
                                        error: catalog_error_to_orbit(error),
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        );
                    }
                    Err(error) => return Err(catalog_error_to_orbit(error)),
                }
            }
        }
        self.load_plugin_job_files(&mut catalog, &mut |error| {
            diagnostics.push(V2JobCatalogDiagnostic {
                directory_index: usize::MAX,
                path: PathBuf::new(),
                error,
            });
        });
        for diagnostic in &diagnostics {
            tracing::warn!(
                target: "orbit.core.jobs",
                path = %diagnostic.path.display(),
                error = %diagnostic.error,
                "skipping malformed job catalog file"
            );
        }
        Ok((catalog, diagnostics))
    }

    fn load_v2_job_catalog_best_effort(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> (V2JobCatalog, Vec<OrbitError>) {
        #[cfg(test)]
        V2_JOB_CATALOG_LOADS.with(|count| count.set(count.get() + 1));
        let mut catalog = V2JobCatalog::new();
        let mut errors = Vec::new();
        for dir in dirs {
            if dir.path().is_dir() {
                match catalog.load_dir_prefer_existing_best_effort(dir.path()) {
                    Ok(parse_errors) => {
                        errors.extend(parse_errors.into_iter().map(catalog_error_to_orbit));
                    }
                    Err(error) => errors.push(catalog_error_to_orbit(error)),
                }
            }
        }
        self.load_plugin_job_files(&mut catalog, &mut |error| errors.push(error));
        (catalog, errors)
    }

    /// Load the `plugin:<ns>` job layer.
    ///
    /// It loads after every directory layer, so a workspace job and a shipped
    /// default both keep their name against a plugin that ships the same one
    /// (design §3: `workspace > plugin:<ns> > shipped`, with L-0060's rule that
    /// a shipped default is never displaced). A plugin whose job files no
    /// longer parse yields a diagnostic, never a failed catalog: every other
    /// job must remain dispatchable.
    fn load_plugin_job_files(
        &self,
        catalog: &mut V2JobCatalog,
        report: &mut dyn FnMut(OrbitError),
    ) {
        for plugin in self.plugin_load().active() {
            if plugin.definitions.jobs.is_empty() {
                continue;
            }
            if let Err(error) = catalog.load_files_prefer_existing(&plugin.definitions.jobs) {
                report(OrbitError::InvalidInput(format!(
                    "plugin '{}' job catalog layer: {}",
                    plugin.namespace(),
                    catalog_error_to_orbit(error)
                )));
            }
        }
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

    fn v2_job_membership_dirs(&self) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        let mut dirs = CatalogDirectoryList::default();
        // Same layering as named execution, with the workspace dir always
        // present so one parse covers both default and custom job names.
        push_v2_job_env_dirs(&mut dirs, v2_job_env_dirs().as_deref());
        dirs.push(
            self.paths().global_dir.join("resources/jobs"),
            V2JobCatalogDirKind::Global,
        );
        dirs.push(
            self.paths().jobs_dir.clone(),
            V2JobCatalogDirKind::Workspace,
        );
        dirs.into_vec()
    }

    pub(crate) fn load_v2_job_asset_by_name(
        &self,
        job_id: &str,
    ) -> Result<(PathBuf, JobV2), OrbitError> {
        let dirs = self.v2_job_asset_dirs_for_execution(job_id);
        let (catalog, diagnostics) = self.load_v2_job_catalog_with_diagnostics(dirs.clone())?;
        let selected_path = catalog.get(job_id).map(|(path, _)| path.to_path_buf());
        if let Some(diagnostic) = diagnostics
            .into_iter()
            .find(|diagnostic| diagnostic.applies_to_job(job_id, selected_path.as_deref(), &dirs))
        {
            return Err(diagnostic.error);
        }
        if let Some((path, spec)) = catalog.get(job_id) {
            return Ok((path.to_path_buf(), spec.clone()));
        }
        Err(OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))
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

fn validate_ci_integration_branch(repo_root: &Path, branch: &str) -> Result<String, OrbitError> {
    // Match pilot admission's origin-prefix normalization once, so collection
    // and every child receive the identical literal branch, never @{-1} or
    // another Git revision expression.
    let branch = branch.trim();
    let branch = branch.strip_prefix("origin/").unwrap_or(branch).trim();
    let valid = std::process::Command::new("git")
        .args(["check-ref-format", &format!("refs/heads/{branch}")])
        .current_dir(repo_root)
        .output()
        .map_err(|error| {
            OrbitError::Execution(format!("validate CI sweep integration branch: {error}"))
        })?
        .status
        .success();
    if !valid || branch == "HEAD" || branch.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "CI sweep integration branch {branch:?} is not a valid branch name"
        )));
    }
    Ok(branch.to_string())
}

fn ci_branch_field(input: &Value, key: &str) -> Result<Option<String>, OrbitError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(branch)) => {
            let branch = branch.trim();
            Ok((!branch.is_empty()).then(|| branch.to_string()))
        }
        Some(_) => Err(OrbitError::InvalidInput(format!(
            "CI sweep integration branch input.{key} must be a string"
        ))),
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
