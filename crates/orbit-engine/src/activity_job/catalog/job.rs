use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_types::workflow::JobV2;

use super::super::asset_loader::load_job_asset;
use super::CatalogError;
#[cfg(doc)]
use super::V2ActivityCatalog;
use super::layered::{
    self, CatalogAdapter, ExistingNamePolicy, LayeredCatalog, LoadedCatalogAsset,
};

/// Typed job adapter over the same layered YAML catalog loader used by
/// [`V2ActivityCatalog`]. Directory precedence remains a caller policy.
#[derive(Debug, Default, Clone)]
pub struct V2JobCatalog {
    inner: LayeredCatalog<JobV2>,
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

/// Every `.yaml` or `.yml` file a job directory layer parses, recursively, in
/// walk order. A caller that memoizes a job catalog keys on these files, so it
/// lists exactly what the loader reads.
pub fn job_definition_files(dir: &Path) -> Result<Vec<PathBuf>, CatalogError> {
    let mut files = Vec::new();
    layered::walk_dir(dir, &mut |path| {
        files.push(path.to_path_buf());
        Ok(())
    })?;
    Ok(files)
}
