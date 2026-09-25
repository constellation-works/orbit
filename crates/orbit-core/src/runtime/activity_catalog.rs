//! The activity catalog a runtime resolves `target: activity:<name>` against.

use std::path::{Path, PathBuf};

use orbit_engine::activity_job::{CatalogDirectory, CatalogDirectoryList};

use super::OrbitRuntime;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

impl OrbitRuntime {
    /// Build the activity catalog for `target: activity:<name>` resolution
    /// (Phase 4). Execution keeps shipped global activities authoritative:
    /// workspace-local assets can add new names, but cannot shadow binary
    /// defaults.
    ///
    /// The lookup order:
    /// 1. `ORBIT_ACTIVITY_DIR` env var (or legacy `ORBIT_V2_CATALOG_DIR`) as
    ///    a colon-separated list of dirs, highest precedence for smokes/tests.
    /// 2. `<global_root>/resources/activities/` — global defaults (seeded by
    ///    `orbit init` from the YAMLs embedded in the binary).
    /// 3. `<workspace_root>/.orbit/resources/activities/` — workspace-local
    ///    additions. Names matching shipped defaults are ignored unless an
    ///    explicit env catalog already supplied that name.
    ///
    /// Missing directories are skipped silently. Directories are loaded from
    /// highest to lowest precedence; the first activity for each name wins,
    /// and workspace-local shipped names are skipped even when the global file
    /// is missing.
    /// Duplicate names inside one directory tree are still a hard error
    /// (`CatalogError::DuplicateName`).
    pub fn v2_activity_catalog(
        &self,
    ) -> Result<
        orbit_engine::activity_job::V2ActivityCatalog,
        orbit_engine::activity_job::CatalogError,
    > {
        use orbit_engine::activity_job::V2ActivityCatalog;

        let mut catalog = V2ActivityCatalog::new();
        for dir in self.v2_activity_catalog_dirs() {
            if !dir.path().is_dir() {
                continue;
            }
            // L-0060 / ORB-00356: name-based execution keeps shipped defaults
            // authoritative over workspace catalogs.
            match dir.kind() {
                V2ActivityCatalogDirKind::Explicit | V2ActivityCatalogDirKind::Global => {
                    warn_skipped_retired_activity_assets(
                        dir.path(),
                        catalog.load_dir_skipping_retired_prefer_existing(dir.path())?,
                    );
                }
                V2ActivityCatalogDirKind::WorkspaceLocal => {
                    warn_skipped_retired_activity_assets(
                        dir.path(),
                        catalog.load_dir_skipping_retired_prefer_existing_where(
                            dir.path(),
                            |name| !is_default_activity_name(name),
                        )?,
                    );
                }
            }
        }
        // The `plugin:<ns>` layer loads last, so a workspace file of the same
        // name shadows a plugin's activity and a shipped default stays
        // authoritative (L-0060). `orbit run show` prints which layer answered
        // for each reference, which is what makes the shadowing legible.
        for plugin in self.plugin_load().active() {
            let files: Vec<PathBuf> = plugin.definitions.activities.clone();
            if files.is_empty() {
                continue;
            }
            warn_skipped_retired_activity_assets(
                &plugin.root,
                catalog.load_files_prefer_existing(&files)?,
            );
        }
        let registered_tools = self.allowlist_known_tool_names();
        catalog.validate_tool_allowlists(registered_tools.iter().map(String::as_str))?;

        Ok(catalog)
    }

    /// Tool names valid as activity allowlist targets.
    ///
    /// This is the registry's builtin schema set.
    /// `pub` for the direct v2 activity runner in `orbit-cmd` [ORB-10016].
    pub fn allowlist_known_tool_names(&self) -> Vec<String> {
        self.tool_registry()
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect()
    }

    /// Production activity-catalog directories in load order. Doctor reuses
    /// this list so a workspace file that fails catalog construction cannot
    /// be reported healthy.
    pub(crate) fn v2_activity_catalog_paths(&self) -> Vec<PathBuf> {
        self.v2_activity_catalog_dirs()
            .into_iter()
            .map(|dir| dir.path().to_path_buf())
            .collect()
    }

    fn v2_activity_catalog_dirs(&self) -> Vec<CatalogDirectory<V2ActivityCatalogDirKind>> {
        let mut dirs = CatalogDirectoryList::default();

        let env_dirs = std::env::var("ORBIT_ACTIVITY_DIR")
            .ok()
            .or_else(|| std::env::var("ORBIT_V2_CATALOG_DIR").ok());
        if let Some(raw) = env_dirs {
            for entry in raw.split(':').filter(|value| !value.is_empty()) {
                dirs.push(
                    std::path::PathBuf::from(entry),
                    V2ActivityCatalogDirKind::Explicit,
                );
            }
        }

        dirs.push(
            self.context.paths().global_dir.join("resources/activities"),
            V2ActivityCatalogDirKind::Global,
        );
        dirs.push(
            self.context.paths().activities_dir.clone(),
            V2ActivityCatalogDirKind::WorkspaceLocal,
        );
        dirs.into_vec()
    }
}

fn warn_skipped_retired_activity_assets(dir: &Path, skipped: Vec<PathBuf>) {
    if skipped.is_empty() {
        return;
    }
    tracing::warn!(
        target: "orbit.core.assets",
        count = skipped.len(),
        dir = %dir.display(),
        "skipped retired schemaVersion 1 activity assets while loading",
    );
}

#[derive(Clone, Copy)]
enum V2ActivityCatalogDirKind {
    Explicit,
    Global,
    WorkspaceLocal,
}

fn is_default_activity_name(name: &str) -> bool {
    DEFAULT_ACTIVITY_FILES
        .iter()
        .any(|(default_name, _)| *default_name == name)
}
