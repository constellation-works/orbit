//! v2 activity catalog + `target: activity:<name>` resolution (Phase 4).
//!
//! A catalog is a name → `ActivityV2` map built from one or more directory
//! trees of v2 YAML assets. [`resolve_job_target_refs`] walks a [`JobV2`]
//! DAG and rewrites every [`JobV2StepBody::TargetRef`] into
//! [`JobV2StepBody::Target`] by looking up the named activity in the
//! catalog. Resolution runs after `resolve_job_backends`
//! (so the `Auto` → concrete rewrite also applies to the newly-inlined
//! specs) and before `validate_job_loop_session_backends`.
//!
//! Scope resolution (§9 `MergeByKey`) remains caller policy: orbit-core
//! adapters supply precedence-ordered directory lists and choose whether a
//! lower layer is eligible. This module owns the shared recursive walk,
//! canonical directory deduplication, first-wins layering, and duplicate-name
//! detection for the typed activity and job catalogs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use orbit_common::OrbitError;

use super::asset_loader::{ActivityAsset, AssetLoadError, load_activity_asset, load_job_asset};
use orbit_types::workflow::activity_job::ActivityV2;
use orbit_types::workflow::{JobV2, JobV2Step, JobV2StepBody, LoopBlock, TargetRef, TargetStep};
use orbit_types::workflow::{
    ToolAllowlistError, validate_activity_tool_allowlist_against_registered_tools,
};

/// `activity:<name>` prefix for the `target:` field on a [`TargetRef`].
pub const ACTIVITY_REF_PREFIX: &str = "activity:";

/// `script:<name>` prefix for the `target:` field on a [`TargetRef`].
pub const SCRIPT_REF_PREFIX: &str = orbit_types::workflow::activity_job::SCRIPT_REF_PREFIX;

/// `auto-task:<name>` prefix for the `target:` field on a [`TargetRef`].
pub const AUTO_TASK_REF_PREFIX: &str = orbit_types::workflow::activity_job::AUTO_TASK_REF_PREFIX;

#[derive(Debug, Default, Clone)]
pub struct V2ActivityCatalog {
    inner: LayeredCatalog<ActivityV2>,
    /// Activities withdrawn from resolution, keyed by name, with the reason
    /// a job referencing one is refused. One unusable activity must not
    /// take the rest of the catalog down with it.
    unavailable: BTreeMap<String, String>,
}

/// Typed job adapter over the same layered YAML catalog loader used by
/// [`V2ActivityCatalog`]. Directory precedence remains a caller policy.
#[derive(Debug, Default, Clone)]
pub struct V2JobCatalog {
    inner: LayeredCatalog<JobV2>,
}

/// A precedence-ordered, canonical-path-deduplicated catalog directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDirectory<K> {
    path: PathBuf,
    kind: K,
}

impl<K> CatalogDirectory<K> {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> &K {
        &self.kind
    }
}

/// Builds a catalog directory list while preserving the first occurrence of
/// each canonical path. Adapters push directories in their policy order.
#[derive(Debug, Clone)]
pub struct CatalogDirectoryList<K> {
    dirs: Vec<CatalogDirectory<K>>,
    seen: std::collections::BTreeSet<PathBuf>,
}

impl<K> Default for CatalogDirectoryList<K> {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            seen: std::collections::BTreeSet::new(),
        }
    }
}

impl<K> CatalogDirectoryList<K> {
    pub fn push(&mut self, path: PathBuf, kind: K) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if self.seen.insert(canonical) {
            self.dirs.push(CatalogDirectory { path, kind });
        }
    }

    pub fn into_vec(self) -> Vec<CatalogDirectory<K>> {
        self.dirs
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("read dir {path}: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("read file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: AssetLoadError,
    },
    #[error("duplicate activity name `{name}` — defined in both {first} and {second}")]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("duplicate v2 job name '{name}' — defined in both {first} and {second}")]
    DuplicateJobName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("activity `{name}` tool allowlist invalid: {source}")]
    ToolAllowlist {
        name: String,
        source: ToolAllowlistError,
    },
}

/// Translate catalog-loading failures at the orbit-engine crate boundary.
pub fn catalog_error_to_orbit(error: CatalogError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ResolveError {
    #[error(
        "step `{step_id}`: target `{target}` does not start with `activity:`, `script:` or `auto-task:`"
    )]
    UnknownRefKind { step_id: String, target: String },
    #[error("step `{step_id}`: activity `{name}` not found in catalog")]
    ActivityNotInCatalog { step_id: String, name: String },
    #[error("{referrer}activity `{name}` is unavailable: {reason}")]
    ActivityUnavailable {
        /// Prefix naming where the reference sits: a step id or a job field.
        referrer: String,
        name: String,
        reason: String,
    },
    #[error("job recovery_activity `{name}` not found in catalog")]
    RecoveryActivityNotInCatalog { name: String },
    #[error("job failure_activity `{name}` not found in catalog")]
    FailureActivityNotInCatalog { name: String },
    #[error("job final_recovery_activity `{name}` not found in catalog")]
    FinalRecoveryActivityNotInCatalog { name: String },
    #[error("step `{step_id}`: recovery_activity `{name}` not found in catalog")]
    StepRecoveryActivityNotInCatalog { step_id: String, name: String },
    #[error(
        "step `{step_id}`: script `{name}` manifest not found at {manifest}; a `script:` step names a manifest in the workspace's scripts directory"
    )]
    ScriptNotInCatalog {
        step_id: String,
        name: String,
        manifest: PathBuf,
    },
}

impl V2ActivityCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every `*.yaml` / `*.yml` file under `dir` (recursively) as a
    /// schemaVersion 2 activity asset. Duplicate names across files are a
    /// hard error; merge semantics belong to the caller.
    pub fn load_dir(&mut self, dir: &Path) -> Result<(), CatalogError> {
        self.inner
            .load_dir(
                dir,
                &ActivityCatalogAdapter {
                    skip_retired: false,
                },
                ExistingNamePolicy::Reject,
                |_| true,
                &BTreeSet::new(),
            )
            .map(|_| ())
    }

    fn withdrawn_names(&self) -> BTreeSet<String> {
        self.unavailable.keys().cloned().collect()
    }

    /// Load `dir` as a lower-precedence layer, skipping retired schemaVersion 1
    /// assets and returning the file paths that were ignored. Duplicate names
    /// inside `dir` are still invalid, but names that already exist in the
    /// catalog are left untouched so callers can load directories from highest
    /// to lowest precedence.
    pub fn load_dir_skipping_retired_prefer_existing(
        &mut self,
        dir: &Path,
    ) -> Result<Vec<PathBuf>, CatalogError> {
        let withdrawn = self.withdrawn_names();
        self.inner.load_dir(
            dir,
            &ActivityCatalogAdapter { skip_retired: true },
            ExistingNamePolicy::PreferExisting,
            |_| true,
            &withdrawn,
        )
    }

    /// Policy-filtered layered load. Duplicate names within `dir` are checked
    /// before `include_name` is applied, then admitted names follow the same
    /// first-wins behavior as [`Self::load_dir_skipping_retired_prefer_existing`].
    pub fn load_dir_skipping_retired_prefer_existing_where<F>(
        &mut self,
        dir: &Path,
        include_name: F,
    ) -> Result<Vec<PathBuf>, CatalogError>
    where
        F: FnMut(&str) -> bool,
    {
        let withdrawn = self.withdrawn_names();
        self.inner.load_dir(
            dir,
            &ActivityCatalogAdapter { skip_retired: true },
            ExistingNamePolicy::PreferExisting,
            include_name,
            &withdrawn,
        )
    }

    /// Best-effort variant of
    /// [`Self::load_dir_skipping_retired_prefer_existing_where`]: a file that
    /// fails to parse (for example a stale copy declaring a removed key) is
    /// withdrawn alone instead of failing the whole catalog. The withdrawn
    /// name (the file stem, matching the doctor scan) is recorded in
    /// `unavailable` with the parse error — which names the file and the
    /// specific remedy it carries — so a job referencing it is refused with
    /// [`ResolveError::ActivityUnavailable`] while unrelated jobs keep
    /// resolving. Non-parse failures (unreadable directories, duplicate
    /// names inside `dir`) remain hard errors. A name already withdrawn by
    /// an earlier layer is not revived by this layer. Returns the retired
    /// files skipped and the parse failures withdrawn, for the caller to
    /// report.
    pub fn load_dir_skipping_retired_prefer_existing_where_best_effort<F>(
        &mut self,
        dir: &Path,
        include_name: F,
    ) -> Result<(Vec<PathBuf>, Vec<CatalogError>), CatalogError>
    where
        F: FnMut(&str) -> bool,
    {
        let mut errors = Vec::new();
        let withdrawn = self.withdrawn_names();
        let skipped = self.inner.load_dir_best_effort(
            dir,
            &ActivityCatalogAdapter { skip_retired: true },
            ExistingNamePolicy::PreferExisting,
            include_name,
            &mut errors,
            &withdrawn,
        )?;
        for error in &errors {
            if let CatalogError::Parse { path, .. } = error {
                let name = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default()
                    .to_string();
                if !name.is_empty() && self.get(&name).is_none() {
                    self.unavailable.insert(name, error.to_string());
                }
            }
        }
        Ok((skipped, errors))
    }

    /// Load an explicit list of activity files as one lower-precedence layer.
    ///
    /// A plugin ships files rather than a directory it owns outright, so this
    /// is the file-shaped counterpart of
    /// [`Self::load_dir_skipping_retired_prefer_existing`]: names already in
    /// the catalog stay, names withdrawn by an earlier layer (recorded in
    /// `unavailable`) are not revived by this lower-precedence layer, and
    /// the returned paths are the retired assets that were skipped.
    pub fn load_files_prefer_existing(
        &mut self,
        files: &[PathBuf],
    ) -> Result<Vec<PathBuf>, CatalogError> {
        let withdrawn = self.withdrawn_names();
        self.inner.load_files(
            files,
            &ActivityCatalogAdapter { skip_retired: true },
            ExistingNamePolicy::PreferExisting,
            &withdrawn,
        )
    }

    /// The file one catalog name resolved from, for layer attribution.
    pub fn source(&self, name: &str) -> Option<&Path> {
        self.inner.sources.get(name).map(PathBuf::as_path)
    }

    /// Insert an explicit `(name, spec)` pair — used by smokes and in-memory
    /// composition. Returns the displaced entry if the name was already set.
    pub fn insert(&mut self, name: impl Into<String>, spec: ActivityV2) -> Option<ActivityV2> {
        let name = name.into();
        self.inner
            .sources
            .insert(name.clone(), PathBuf::from("<explicit>"));
        self.inner.entries.insert(name, spec)
    }

    pub fn get(&self, name: &str) -> Option<&ActivityV2> {
        self.inner.entries.get(name)
    }

    pub fn len(&self) -> usize {
        self.inner.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.entries.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.inner.entries.keys().map(String::as_str)
    }

    /// Validate every agent-facing activity tool allowlist against a caller
    /// supplied registry snapshot (keeping `orbit-common` registry-agnostic),
    /// one activity at a time: every activity whose allowlist fails against `registered_tools` is withdrawn from
    /// resolution and recorded as unavailable, and the rest of the catalog
    /// stays usable. A job that references a withdrawn activity is refused
    /// with [`ResolveError::ActivityUnavailable`], naming the activity and
    /// the reason. Returns the failures so the caller can report them.
    pub fn withdraw_invalid_tool_allowlists<'a, I>(
        &mut self,
        registered_tools: I,
    ) -> Vec<CatalogError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let registered_tools: Vec<&str> = registered_tools.into_iter().collect();
        let mut failures = Vec::new();
        for (name, activity) in &self.inner.entries {
            if let Err(source) = validate_activity_tool_allowlist_against_registered_tools(
                activity,
                registered_tools.iter().copied(),
            ) {
                failures.push(CatalogError::ToolAllowlist {
                    name: name.clone(),
                    source,
                });
            }
        }
        for failure in &failures {
            if let CatalogError::ToolAllowlist { name, source } = failure {
                // The source path stays recorded so layer attribution still
                // explains where the withdrawn activity came from.
                self.inner.entries.remove(name);
                self.unavailable
                    .insert(name.clone(), format!("tool allowlist invalid: {source}"));
            }
        }
        failures
    }

    /// Why `name` was withdrawn from the catalog, when it was.
    pub fn unavailable_reason(&self, name: &str) -> Option<&str> {
        self.unavailable.get(name).map(String::as_str)
    }

    /// Look up `name` for resolution: a withdrawn activity is refused with
    /// its reason, a missing one with `not_found`.
    fn resolve_activity(
        &self,
        name: &str,
        referrer: impl FnOnce() -> String,
        not_found: impl FnOnce() -> ResolveError,
    ) -> Result<&ActivityV2, ResolveError> {
        if let Some(activity) = self.get(name) {
            return Ok(activity);
        }
        match self.unavailable_reason(name) {
            Some(reason) => Err(ResolveError::ActivityUnavailable {
                referrer: referrer(),
                name: name.to_string(),
                reason: reason.to_string(),
            }),
            None => Err(not_found()),
        }
    }
}

impl V2JobCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load one job directory while retaining valid siblings when a file has
    /// an invalid schema or document. Non-parse failures and duplicate names
    /// remain hard errors. The returned diagnostics are the parse failures
    /// encountered in this directory.
    pub fn load_dir_prefer_existing_best_effort(
        &mut self,
        dir: &Path,
    ) -> Result<Vec<CatalogError>, CatalogError> {
        let mut errors = Vec::new();
        self.inner
            .load_dir_best_effort(
                dir,
                &JobCatalogAdapter,
                ExistingNamePolicy::PreferExisting,
                |_| true,
                &mut errors,
                &BTreeSet::new(),
            )
            .map(|_| errors)
    }

    /// Load an explicit list of job files as one lower-precedence layer, for
    /// the same reason [`V2ActivityCatalog::load_files_prefer_existing`]
    /// exists: a plugin contributes files, not a directory.
    pub fn load_files_prefer_existing(&mut self, files: &[PathBuf]) -> Result<(), CatalogError> {
        self.inner
            .load_files(
                files,
                &JobCatalogAdapter,
                ExistingNamePolicy::PreferExisting,
                &BTreeSet::new(),
            )
            .map(|_| ())
    }

    pub fn get(&self, name: &str) -> Option<(&Path, &JobV2)> {
        Some((
            self.inner.sources.get(name)?.as_path(),
            self.inner.entries.get(name)?,
        ))
    }

    /// Consume one entry, returning its source path and spec without cloning.
    pub fn take(&mut self, name: &str) -> Option<(PathBuf, JobV2)> {
        let spec = self.inner.entries.remove(name)?;
        let path = self.inner.sources.remove(name)?;
        Some((path, spec))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path, &JobV2)> {
        self.inner.entries.iter().filter_map(|(name, spec)| {
            self.inner
                .sources
                .get(name)
                .map(|path| (name.as_str(), path.as_path(), spec))
        })
    }
}

#[derive(Debug, Clone)]
struct LayeredCatalog<T> {
    entries: BTreeMap<String, T>,
    sources: BTreeMap<String, PathBuf>,
}

impl<T> Default for LayeredCatalog<T> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            sources: BTreeMap::new(),
        }
    }
}

impl<T> LayeredCatalog<T> {
    fn load_dir<A, F>(
        &mut self,
        dir: &Path,
        adapter: &A,
        existing_name_policy: ExistingNamePolicy,
        include_name: F,
        withdrawn_names: &BTreeSet<String>,
    ) -> Result<Vec<PathBuf>, CatalogError>
    where
        A: CatalogAdapter<Asset = T>,
        F: FnMut(&str) -> bool,
    {
        self.load_dir_inner(
            dir,
            adapter,
            existing_name_policy,
            include_name,
            None,
            withdrawn_names,
        )
    }

    fn load_dir_best_effort<A, F>(
        &mut self,
        dir: &Path,
        adapter: &A,
        existing_name_policy: ExistingNamePolicy,
        include_name: F,
        errors: &mut Vec<CatalogError>,
        withdrawn_names: &BTreeSet<String>,
    ) -> Result<Vec<PathBuf>, CatalogError>
    where
        A: CatalogAdapter<Asset = T>,
        F: FnMut(&str) -> bool,
    {
        self.load_dir_inner(
            dir,
            adapter,
            existing_name_policy,
            include_name,
            Some(errors),
            withdrawn_names,
        )
    }

    /// Load an explicit file list as one layer. Duplicate names inside the
    /// list are rejected; names already in the catalog keep their entry when
    /// the policy prefers existing.
    fn load_files<A>(
        &mut self,
        files: &[PathBuf],
        adapter: &A,
        existing_name_policy: ExistingNamePolicy,
        withdrawn_names: &BTreeSet<String>,
    ) -> Result<Vec<PathBuf>, CatalogError>
    where
        A: CatalogAdapter<Asset = T>,
    {
        let mut local_entries: BTreeMap<String, (T, PathBuf)> = BTreeMap::new();
        let mut skipped = Vec::new();
        for path in files {
            let yaml = std::fs::read_to_string(path).map_err(|source| CatalogError::ReadFile {
                path: path.clone(),
                source,
            })?;
            let Some(asset) = adapter.load(path, &yaml, &mut skipped)? else {
                continue;
            };
            if let Some((_, prev)) = local_entries.get(&asset.name) {
                return Err(adapter.duplicate_name(asset.name, prev.clone(), path.clone()));
            }
            local_entries.insert(asset.name, (asset.spec, path.clone()));
        }
        for (name, (spec, path)) in local_entries {
            if withdrawn_names.contains(&name) {
                continue;
            }
            if let Some(prev) = self.sources.get(&name) {
                match existing_name_policy {
                    ExistingNamePolicy::Reject => {
                        return Err(adapter.duplicate_name(name, prev.clone(), path));
                    }
                    ExistingNamePolicy::PreferExisting => continue,
                }
            }
            self.sources.insert(name.clone(), path);
            self.entries.insert(name, spec);
        }
        Ok(skipped)
    }

    fn load_dir_inner<A, F>(
        &mut self,
        dir: &Path,
        adapter: &A,
        existing_name_policy: ExistingNamePolicy,
        mut include_name: F,
        mut parse_errors: Option<&mut Vec<CatalogError>>,
        withdrawn_names: &BTreeSet<String>,
    ) -> Result<Vec<PathBuf>, CatalogError>
    where
        A: CatalogAdapter<Asset = T>,
        F: FnMut(&str) -> bool,
    {
        let mut local_entries: BTreeMap<String, (T, PathBuf)> = BTreeMap::new();
        let mut skipped = Vec::new();
        walk_dir(dir, &mut |path| {
            let yaml = std::fs::read_to_string(path).map_err(|source| CatalogError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
            let asset = match adapter.load(path, &yaml, &mut skipped) {
                Ok(asset) => asset,
                Err(error @ CatalogError::Parse { .. }) => {
                    if let Some(errors) = parse_errors.as_deref_mut() {
                        errors.push(error);
                        return Ok(());
                    }
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            let Some(asset) = asset else {
                return Ok(());
            };
            if let Some((_, prev)) = local_entries.get(&asset.name) {
                return Err(adapter.duplicate_name(asset.name, prev.clone(), path.to_path_buf()));
            }
            local_entries.insert(asset.name, (asset.spec, path.to_path_buf()));
            Ok(())
        })?;

        for (name, (spec, path)) in local_entries {
            if withdrawn_names.contains(&name) {
                continue;
            }
            if !include_name(&name) {
                continue;
            }
            if let Some(prev) = self.sources.get(&name) {
                match existing_name_policy {
                    ExistingNamePolicy::Reject => {
                        return Err(adapter.duplicate_name(name, prev.clone(), path));
                    }
                    ExistingNamePolicy::PreferExisting => continue,
                }
            }
            self.sources.insert(name.clone(), path);
            self.entries.insert(name, spec);
        }

        Ok(skipped)
    }
}

struct LoadedCatalogAsset<T> {
    name: String,
    spec: T,
}

trait CatalogAdapter {
    type Asset;

    fn duplicate_name(&self, name: String, first: PathBuf, second: PathBuf) -> CatalogError;

    fn load(
        &self,
        path: &Path,
        yaml: &str,
        skipped: &mut Vec<PathBuf>,
    ) -> Result<Option<LoadedCatalogAsset<Self::Asset>>, CatalogError>;
}

struct ActivityCatalogAdapter {
    skip_retired: bool,
}

impl CatalogAdapter for ActivityCatalogAdapter {
    type Asset = ActivityV2;

    fn duplicate_name(&self, name: String, first: PathBuf, second: PathBuf) -> CatalogError {
        CatalogError::DuplicateName {
            name,
            first,
            second,
        }
    }

    fn load(
        &self,
        path: &Path,
        yaml: &str,
        skipped: &mut Vec<PathBuf>,
    ) -> Result<Option<LoadedCatalogAsset<Self::Asset>>, CatalogError> {
        match load_activity_catalog_asset(path, yaml, self.skip_retired)? {
            Some(asset) => Ok(Some(LoadedCatalogAsset {
                name: asset.name,
                spec: asset.spec,
            })),
            None => {
                skipped.push(path.to_path_buf());
                Ok(None)
            }
        }
    }
}

/// Load one activity YAML with the same parse and retired-schema rules the
/// catalog uses while walking a directory. `Ok(None)` is the skipped
/// schemaVersion 1 case when `skip_retired` is set; every other load
/// failure is [`CatalogError::Parse`].
pub fn load_activity_catalog_asset(
    path: &Path,
    yaml: &str,
    skip_retired: bool,
) -> Result<Option<ActivityAsset>, CatalogError> {
    match load_activity_asset(yaml) {
        Ok(asset) => Ok(Some(asset)),
        Err(AssetLoadError::RetiredVersion(_)) if skip_retired => Ok(None),
        Err(source) => Err(CatalogError::Parse {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Validate one loaded activity's tool allowlist the same way
/// [`V2ActivityCatalog::withdraw_invalid_tool_allowlists`] does for a full
/// catalog.
pub fn validate_catalog_activity_tools<'a, I>(
    name: &str,
    activity: &ActivityV2,
    registered_tools: I,
) -> Result<(), CatalogError>
where
    I: IntoIterator<Item = &'a str>,
{
    validate_activity_tool_allowlist_against_registered_tools(activity, registered_tools).map_err(
        |source| CatalogError::ToolAllowlist {
            name: name.to_string(),
            source,
        },
    )
}

struct JobCatalogAdapter;

impl CatalogAdapter for JobCatalogAdapter {
    type Asset = JobV2;

    fn duplicate_name(&self, name: String, first: PathBuf, second: PathBuf) -> CatalogError {
        CatalogError::DuplicateJobName {
            name,
            first,
            second,
        }
    }

    fn load(
        &self,
        path: &Path,
        yaml: &str,
        _skipped: &mut Vec<PathBuf>,
    ) -> Result<Option<LoadedCatalogAsset<Self::Asset>>, CatalogError> {
        load_job_asset(yaml)
            .map(|asset| {
                Some(LoadedCatalogAsset {
                    name: asset.name,
                    spec: asset.spec,
                })
            })
            .map_err(|source| CatalogError::Parse {
                path: path.to_path_buf(),
                source,
            })
    }
}

#[derive(Debug, Clone, Copy)]
enum ExistingNamePolicy {
    Reject,
    PreferExisting,
}

fn walk_dir(
    dir: &Path,
    cb: &mut dyn FnMut(&Path) -> Result<(), CatalogError>,
) -> Result<(), CatalogError> {
    let iter = std::fs::read_dir(dir).map_err(|source| CatalogError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in iter {
        let entry = entry.map_err(|source| CatalogError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, cb)?;
            continue;
        }
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");
        if is_yaml {
            cb(&path)?;
        }
    }
    Ok(())
}

/// Every `.yaml` or `.yml` file a job directory layer parses, recursively, in
/// walk order. A caller that memoizes a job catalog keys on these files, so it
/// lists exactly what the loader reads.
pub fn job_definition_files(dir: &Path) -> Result<Vec<PathBuf>, CatalogError> {
    let mut files = Vec::new();
    walk_dir(dir, &mut |path| {
        files.push(path.to_path_buf());
        Ok(())
    })?;
    Ok(files)
}

/// Walk `job` and rewrite every `activity:` [`JobV2StepBody::TargetRef`]
/// into a [`JobV2StepBody::Target`] using the named [`ActivityV2`] from
/// `catalog`. A ref with an unknown name is a hard error; silently
/// succeeding would leave a `TargetRef` lurking past dispatch where the
/// executor would ignore it.
///
/// A `script:` ref is a load error here: resolving one needs the
/// workspace's script catalog ([`resolve_job_target_refs_with_scripts`]),
/// and a caller without one must not silently admit a script step.
pub fn resolve_job_target_refs(
    job: &mut JobV2,
    catalog: &V2ActivityCatalog,
) -> Result<(), ResolveError> {
    let scripts = super::script_catalog::V2ScriptCatalog::new(PathBuf::new(), PathBuf::new());
    resolve_job_target_refs_with_scripts(job, catalog, &scripts)
}

/// [`resolve_job_target_refs`], plus `script:` refs: each one is checked
/// against the workspace's script catalog (whose loader has already
/// validated the manifest and resolved its entry inside the workspace),
/// and a missing script is a load error naming the manifest file it
/// would live at. A resolved script ref is *retained* as a `TargetRef`:
/// the host re-reads and snapshots the script, checks its grant and runs
/// it at dispatch, so a script edited after load is still caught.
pub fn resolve_job_target_refs_with_scripts(
    job: &mut JobV2,
    catalog: &V2ActivityCatalog,
    scripts: &super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    job.resolved_recovery_activity = match job.recovery_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || "job recovery_activity: ".to_string(),
                    || ResolveError::RecoveryActivityNotInCatalog {
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };
    job.resolved_failure_activity = match job.failure_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || "job failure_activity: ".to_string(),
                    || ResolveError::FailureActivityNotInCatalog {
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };
    job.resolved_final_recovery_activity = match job.final_recovery_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || "job final_recovery_activity: ".to_string(),
                    || ResolveError::FinalRecoveryActivityNotInCatalog {
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };

    for step in &mut job.steps {
        resolve_step(step, catalog, scripts)?;
    }
    Ok(())
}

fn resolve_step(
    step: &mut JobV2Step,
    catalog: &V2ActivityCatalog,
    scripts: &super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    step.resolved_recovery_activity = match step.recovery_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || format!("step `{}`: recovery_activity: ", step.id),
                    || ResolveError::StepRecoveryActivityNotInCatalog {
                        step_id: step.id.clone(),
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };

    match &mut step.body {
        JobV2StepBody::Target(_) => Ok(()),
        JobV2StepBody::TargetRef(reference) if reference.target.starts_with(SCRIPT_REF_PREFIX) => {
            resolve_script_ref(&step.id, &reference.target, scripts)
        }
        // An auto-task ref is retained: Core resolves the named definition
        // for inspection, and `validate_job` refuses to run it.
        JobV2StepBody::TargetRef(reference)
            if reference.target.starts_with(AUTO_TASK_REF_PREFIX) =>
        {
            Ok(())
        }
        JobV2StepBody::TargetRef(_) => {
            // Swap the body out so we can own the ref without cloning; the
            // replacement is a throwaway `Target` that we immediately
            // overwrite with the resolved one.
            let old = std::mem::replace(
                &mut step.body,
                JobV2StepBody::TargetRef(TargetRef {
                    target: String::new(),
                    default_input: None,
                    timeout_seconds: 0,
                    session: None,
                }),
            );
            let JobV2StepBody::TargetRef(r) = old else {
                unreachable!("checked above");
            };
            let resolved = resolve_ref(&step.id, r, catalog)?;
            step.body = JobV2StepBody::Target(resolved);
            Ok(())
        }
        JobV2StepBody::Parallel { parallel } => {
            for branch in &mut parallel.branches {
                resolve_step(branch, catalog, scripts)?;
            }
            Ok(())
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            resolve_step(&mut fan_out.worker, catalog, scripts)
        }
        JobV2StepBody::Loop { loop_ } => resolve_loop(loop_, catalog, scripts),
    }
}

fn resolve_loop(
    block: &mut LoopBlock,
    catalog: &V2ActivityCatalog,
    scripts: &super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    for step in &mut block.steps {
        resolve_step(step, catalog, scripts)?;
    }
    Ok(())
}

/// Check one `script:<name>` ref against the script catalog. The catalog
/// loader has already refused a manifest whose entry escapes the
/// workspace or whose interpreter carries inline code, naming that
/// manifest file; the only refusal left here is a name with no manifest.
fn resolve_script_ref(
    step_id: &str,
    target: &str,
    scripts: &super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    let name = target.strip_prefix(SCRIPT_REF_PREFIX).unwrap_or(target);
    if scripts.get(name).is_some() {
        return Ok(());
    }
    Err(ResolveError::ScriptNotInCatalog {
        step_id: step_id.to_string(),
        name: name.to_string(),
        manifest: scripts.manifest_path_for(name),
    })
}

fn resolve_ref(
    step_id: &str,
    r: TargetRef,
    catalog: &V2ActivityCatalog,
) -> Result<TargetStep, ResolveError> {
    let name =
        r.target
            .strip_prefix(ACTIVITY_REF_PREFIX)
            .ok_or_else(|| ResolveError::UnknownRefKind {
                step_id: step_id.to_string(),
                target: r.target.clone(),
            })?;
    let activity = catalog.resolve_activity(
        name,
        || format!("step `{step_id}`: "),
        || ResolveError::ActivityNotInCatalog {
            step_id: step_id.to_string(),
            name: name.to_string(),
        },
    )?;
    Ok(TargetStep {
        spec: activity.spec.clone(),
        activity_name: Some(name.to_string()),
        input_schema_json: Some(activity.input_schema_json.clone()),
        fs_profile: activity.fs_profile.clone(),
        default_input: r.default_input,
        timeout_seconds: r.timeout_seconds,
        session: r.session,
    })
}
