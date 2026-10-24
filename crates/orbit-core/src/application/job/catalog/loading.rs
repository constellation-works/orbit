use super::*;

/// The job catalog one multi-run read parses once; see
/// [`OrbitRuntime::v2_job_by_name_cached`].
#[derive(Default)]
pub(crate) struct V2JobNameCache {
    catalog: OnceCell<Option<LoadedV2JobCatalog>>,
    resolved: RefCell<HashMap<String, Option<JobV2>>>,
}

/// The membership layering, which is named execution's layering for a custom
/// job name and that layering plus the workspace directory for a default one.
pub(super) struct LoadedV2JobCatalog {
    catalog: V2JobCatalog,
    diagnostics: Vec<V2JobCatalogDiagnostic>,
    dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    workspace_index: Option<usize>,
}

impl LoadedV2JobCatalog {
    /// `Some` with what named execution resolves for `job_id`, or `None` when
    /// this layering cannot answer for it.
    pub(super) fn resolve(&self, job_id: &str) -> Option<Option<JobV2>> {
        let selected = self.catalog.get(job_id);
        let selected_path = selected.map(|(path, _)| path);
        // A shipped default never resolves from the workspace directory.
        let skipped = is_default_job_name(job_id)
            .then_some(self.workspace_index)
            .flatten();
        if skipped.is_some()
            && selected_path.is_some_and(|path| {
                self.dirs
                    .iter()
                    .position(|dir| path.starts_with(dir.path()))
                    == skipped
            })
        {
            return None;
        }
        let refused = self.diagnostics.iter().any(|diagnostic| {
            Some(diagnostic.directory_index) != skipped
                && diagnostic.applies_to_job(job_id, selected_path, &self.dirs)
        });
        Some(match selected {
            Some((_, job)) if !refused => Some(job.clone()),
            _ => None,
        })
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
pub(super) struct V2JobCatalogDiagnostic {
    directory_index: usize,
    path: PathBuf,
    error: OrbitError,
}

impl V2JobCatalogDiagnostic {
    pub(super) fn applies_to_job(
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

    pub(super) fn ci_sweep_integration_branch(
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
        let v2_jobs = self.load_v2_job_assets()?;
        let mut result = Vec::new();

        for (job_id, path, spec) in v2_jobs.iter() {
            if !include_disabled && spec.state == JobScheduleState::Disabled {
                continue;
            }
            if !matches_job_filter(spec.kind, filter) {
                continue;
            }
            result.push((
                JobCatalogEntry {
                    job_id: job_id.to_string(),
                    path: path.to_path_buf(),
                    spec: spec.clone(),
                },
                None,
            ));
        }

        let job_ids = result
            .iter()
            .map(|(entry, _)| entry.job_id.clone())
            .collect::<Vec<_>>();
        let mut latest = self
            .stores()
            .jobs()
            .latest_job_runs(&job_ids)?
            .into_iter()
            .map(|run| (run.job_id.clone(), run))
            .collect::<std::collections::HashMap<_, _>>();
        for (entry, last_run) in &mut result {
            *last_run = latest.remove(&entry.job_id);
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
        let (mut v2_jobs, diagnostics) = self.load_v2_job_catalog_with_diagnostics(dirs.clone())?;
        let selected_path = v2_jobs.get(job_id).map(|(path, _)| path.to_path_buf());
        if let Some(diagnostic) = diagnostics
            .into_iter()
            .find(|diagnostic| diagnostic.applies_to_job(job_id, selected_path.as_deref(), &dirs))
        {
            return Err(diagnostic.error);
        }
        if let Some((path, spec)) = v2_jobs.take(job_id) {
            return Ok(JobCatalogEntry {
                job_id: job_id.to_string(),
                path,
                spec,
            });
        }
        Err(OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))
    }

    pub(crate) fn load_v2_job_assets(&self) -> Result<V2JobCatalog, OrbitError> {
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
    ///
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

    pub(super) fn load_v2_job_catalog(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> Result<V2JobCatalog, OrbitError> {
        self.load_v2_job_catalog_with_diagnostics(dirs)
            .map(|(catalog, _)| catalog)
    }

    pub(super) fn load_v2_job_catalog_with_diagnostics(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> Result<(V2JobCatalog, Vec<V2JobCatalogDiagnostic>), OrbitError> {
        tracing::debug!(job_catalog_load = true);
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

    pub(super) fn load_v2_job_catalog_best_effort(
        &self,
        dirs: Vec<CatalogDirectory<V2JobCatalogDirKind>>,
    ) -> (V2JobCatalog, Vec<OrbitError>) {
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
    pub(super) fn load_plugin_job_files(
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

    pub(super) fn v2_job_asset_dirs(&self) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        self.v2_job_asset_dirs_with_env(v2_job_env_dirs().as_deref())
    }

    pub(super) fn v2_job_asset_dirs_with_env(
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

    pub(super) fn v2_job_membership_dirs(&self) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
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
        let (mut catalog, diagnostics) = self.load_v2_job_catalog_with_diagnostics(dirs.clone())?;
        let selected_path = catalog.get(job_id).map(|(path, _)| path.to_path_buf());
        if let Some(diagnostic) = diagnostics
            .into_iter()
            .find(|diagnostic| diagnostic.applies_to_job(job_id, selected_path.as_deref(), &dirs))
        {
            return Err(diagnostic.error);
        }
        if let Some(resolved) = catalog.take(job_id) {
            return Ok(resolved);
        }
        Err(OrbitError::not_found(NotFoundKind::Job, job_id.to_string()))
    }

    /// [`Self::load_v2_job_asset_by_name`] for a read that resolves many
    /// runs by job name: every name resolves to the definition execution
    /// would load, from one parse of the catalog for all names. Only the
    /// definition matters to such a read, so a failed resolution is `None`.
    pub(crate) fn v2_job_by_name_cached(
        &self,
        cache: &V2JobNameCache,
        job_id: &str,
    ) -> Option<JobV2> {
        if let Some(resolved) = cache.resolved.borrow().get(job_id) {
            return resolved.clone();
        }
        let loaded = cache.catalog.get_or_init(|| {
            let dirs = self.v2_job_membership_dirs();
            let (catalog, diagnostics) = self
                .load_v2_job_catalog_with_diagnostics(dirs.clone())
                .ok()?;
            Some(LoadedV2JobCatalog {
                catalog,
                diagnostics,
                workspace_index: dirs
                    .iter()
                    .position(|dir| *dir.kind() == V2JobCatalogDirKind::Workspace),
                dirs,
            })
        });
        let resolved = match loaded.as_ref().map(|loaded| loaded.resolve(job_id)) {
            Some(Some(resolved)) => resolved,
            // The catalog did not load, or this default name resolves to a
            // layer named execution never reads for it: load the way
            // execution does.
            _ => self
                .load_v2_job_asset_by_name(job_id)
                .ok()
                .map(|(_, job)| job),
        };
        cache
            .resolved
            .borrow_mut()
            .insert(job_id.to_string(), resolved.clone());
        resolved
    }

    pub(super) fn v2_job_asset_dirs_for_execution(
        &self,
        job_id: &str,
    ) -> Vec<CatalogDirectory<V2JobCatalogDirKind>> {
        self.v2_job_asset_dirs_for_execution_with_env(job_id, v2_job_env_dirs().as_deref())
    }

    pub(super) fn v2_job_asset_dirs_for_execution_with_env(
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

pub(super) fn validate_ci_integration_branch(
    repo_root: &Path,
    branch: &str,
) -> Result<String, OrbitError> {
    // Match pilot admission's origin-prefix normalization once, so collection
    // and every child receive the identical literal branch, never @{-1} or
    // another Git revision expression.
    let branch = branch.trim();
    let branch = branch.strip_prefix("origin/").unwrap_or(branch).trim();
    // `check-ref-format` is outside `run_git`'s admitted subcommands.
    let mut command = std::process::Command::new("git");
    command
        .args(["check-ref-format", &format!("refs/heads/{branch}")])
        .current_dir(repo_root);
    let valid = run_bounded_capped(&mut command, GIT_LOCAL_TIMEOUT, GIT_OUTPUT_LIMIT)
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

pub(super) fn ci_branch_field(input: &Value, key: &str) -> Result<Option<String>, OrbitError> {
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

pub(super) fn v2_job_env_dirs() -> Option<String> {
    std::env::var("ORBIT_JOB_DIR")
        .ok()
        .or_else(|| std::env::var("ORBIT_V2_JOB_DIR").ok())
}

pub(super) fn push_v2_job_env_dirs(
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
pub(super) enum V2JobCatalogDirKind {
    Explicit,
    Workspace,
    Global,
}

pub(crate) fn is_default_job_name(job_id: &str) -> bool {
    DEFAULT_JOB_FILES
        .iter()
        .any(|(default_job_id, _)| *default_job_id == job_id)
}

pub(super) fn matches_job_filter(kind: JobKind, filter: JobCatalogFilter) -> bool {
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
pub(crate) fn shipped_job_spec(job_id: &str) -> Result<JobV2, OrbitError> {
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
