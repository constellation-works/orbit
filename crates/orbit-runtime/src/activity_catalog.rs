//! The activity catalog a runtime resolves `target: activity:<name>` against.

use std::path::{Path, PathBuf};

use orbit_engine::activity_job::{CatalogDirectory, CatalogDirectoryList};

use super::RuntimeKernel;
use crate::assets::DEFAULT_ACTIVITY_FILES;

impl RuntimeKernel {
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
    /// (`CatalogError::DuplicateName`). An activity whose tool allowlist does
    /// not validate against the registered tools is withdrawn alone and
    /// refused when a job references it. A workspace-local file that fails
    /// to parse (a stale copy of a retired activity, or one declaring a
    /// removed key) is likewise withdrawn alone with a warning naming the
    /// file, so unrelated jobs still run; only a job referencing that name
    /// is refused, with the same parse error.
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
                    // A stale workspace-local copy (e.g. of a retired bundled
                    // activity, or one declaring a removed key) must not fail
                    // every job in the workspace: withdraw that file alone,
                    // warn with its path and remedy, and let unrelated jobs
                    // build. A job referencing the withdrawn name is refused
                    // with the same named error. Global and explicit layers
                    // stay fail-closed above.
                    let (skipped, failures) = catalog
                        .load_dir_skipping_retired_prefer_existing_where_best_effort(
                            dir.path(),
                            |name| !is_default_activity_name(name),
                        )?;
                    warn_skipped_retired_activity_assets(dir.path(), skipped);
                    for failure in &failures {
                        tracing::warn!(
                            target: "orbit.core.assets",
                            error = %failure,
                            "skipping invalid workspace-local activity file; fix or remove the stale file it names (a removed activity or key will not return — migrate as the error directs, for example to a task workflow), then rerun the job; jobs that reference this activity will be refused with the same error"
                        );
                    }
                }
            }
        }
        // The `plugin:<ns>` layer loads last, so a workspace file of the same
        // name shadows a plugin's activity and a shipped default stays
        // authoritative (L-0060). A name withdrawn for a stale workspace-local
        // file stays withdrawn here: the plugin layer must not revive it.
        // `orbit run show` prints which layer answered
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
        // An activity whose allowlist names a tool this workspace does not
        // register (say, one from a plugin that is not installed) is withdrawn
        // on its own: jobs that reference it are refused with the reason, and
        // every other job keeps running. Catalog loading logs each withdrawal.
        let registered_tools = self.allowlist_known_tool_names();
        for failure in
            catalog.withdraw_invalid_tool_allowlists(registered_tools.iter().map(String::as_str))
        {
            tracing::warn!(
                target: "orbit.core.assets",
                error = %failure,
                "activity withdrawn from the catalog"
            );
        }

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
    pub fn v2_activity_catalog_paths(&self) -> Vec<PathBuf> {
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
