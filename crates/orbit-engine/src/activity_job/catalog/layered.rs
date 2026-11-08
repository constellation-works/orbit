use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::CatalogError;

#[derive(Debug, Clone)]
pub(super) struct LayeredCatalog<T> {
    pub(super) entries: BTreeMap<String, T>,
    pub(super) sources: BTreeMap<String, PathBuf>,
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
    pub(super) fn load_dir<A, F>(
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

    pub(super) fn load_dir_best_effort<A, F>(
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
    pub(super) fn load_files<A>(
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

pub(super) struct LoadedCatalogAsset<T> {
    pub(super) name: String,
    pub(super) spec: T,
}

pub(super) trait CatalogAdapter {
    type Asset;

    fn duplicate_name(&self, name: String, first: PathBuf, second: PathBuf) -> CatalogError;

    fn load(
        &self,
        path: &Path,
        yaml: &str,
        skipped: &mut Vec<PathBuf>,
    ) -> Result<Option<LoadedCatalogAsset<Self::Asset>>, CatalogError>;
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ExistingNamePolicy {
    Reject,
    PreferExisting,
}

pub(super) fn walk_dir(
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
