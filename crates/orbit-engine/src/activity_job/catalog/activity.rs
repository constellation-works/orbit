use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_types::workflow::activity_job::ActivityV2;
use orbit_types::workflow::validate_activity_tool_allowlist_against_registered_tools;

use super::super::asset_loader::{
    ActivityAsset, AssetLoadError, declared_activity_name_in_yaml, load_activity_asset,
};
use super::layered::{CatalogAdapter, ExistingNamePolicy, LayeredCatalog, LoadedCatalogAsset};
use super::{CatalogError, ResolveError};

#[derive(Debug, Default, Clone)]
pub struct V2ActivityCatalog {
    inner: LayeredCatalog<ActivityV2>,
    /// Activities withdrawn from resolution, keyed by name, with the reason
    /// a job referencing one is refused. One unusable activity must not
    /// take the rest of the catalog down with it.
    unavailable: BTreeMap<String, String>,
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
    /// name — the activity's declared `metadata.name` when it is
    /// recoverable from the refusal or the document, otherwise the file
    /// stem — is recorded in `unavailable` with the parse error, which
    /// names the file and the specific remedy it carries, so a job
    /// referencing it is refused with
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
            if let CatalogError::Parse { path, source } = error {
                let name = withdrawn_activity_name(path, source);
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
    pub(super) fn resolve_activity(
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

/// The name a parse refusal is withdrawn under: the declared
/// `metadata.name` when the refusal or the file itself still yields it,
/// otherwise the conservative file-stem fallback for identities that
/// cannot be recovered.
fn withdrawn_activity_name(path: &Path, source: &AssetLoadError) -> String {
    if let Some(name) = source.declared_activity_name() {
        return name.to_string();
    }
    if let Some(name) = std::fs::read_to_string(path)
        .ok()
        .and_then(|yaml| declared_activity_name_in_yaml(&yaml))
    {
        return name;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
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
