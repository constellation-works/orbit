use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_config::{
    ConfigRoots, ConfigScope, ConfigSnapshot, ConfigStore, EffectiveConfigValue, admit_config_key,
    load_effective_config,
};
use serde_json::Value;

use crate::{Core, ShipMode};

/// Which layer view a read command inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigReadScope {
    /// Layered effective configuration.
    Effective,
    /// Global `~/.orbit/config.toml` only.
    Global,
    /// Workspace `.orbit/config.toml` only.
    Workspace,
}

/// Read result of `orbit config get`.
pub enum ConfigGetResult {
    /// Layered effective value.
    Effective {
        /// Effective JSON value, or null when unset.
        value: Value,
    },
    /// Value resolved from one physical config file in isolation.
    Scoped {
        /// Scope label (`global` or `workspace`).
        scope: &'static str,
        /// Physical file path when it exists on disk.
        path: Option<PathBuf>,
        /// Effective value for the key in this file.
        value: Value,
        /// Whether the key is explicitly set in this file.
        exists: bool,
    },
}

/// Workspace-registry delivery metadata rendered by `orbit config show`.
pub struct ConfigShowWorkspaceBinding {
    /// Registered base branch, if any.
    pub base_branch: Option<String>,
    /// Registered ship mode.
    pub ship_mode: ShipMode,
    /// Registered repository root.
    pub repo_root: PathBuf,
}

/// Resolved roots, paths and persistence details for `orbit config show`.
pub struct ConfigShowRoots {
    /// Host global Orbit root.
    pub global_root: PathBuf,
    /// Workspace shared Orbit root.
    pub shared_root: PathBuf,
    /// Worktree-local Orbit root.
    pub local_root: PathBuf,
    /// Whether a separate workspace layer exists distinct from global.
    pub has_workspace_layer: bool,
    /// Bound workspace delivery metadata, if registered.
    pub workspace_binding: Option<ConfigShowWorkspaceBinding>,
    /// Persistence location metadata JSON.
    pub persistence: Value,
    /// Pre-formatted `(label, value)` rows for the `Paths` block.
    pub path_rows: Vec<(String, String)>,
}

/// Read model for `orbit config show`.
pub enum ConfigShowData {
    /// Layered effective configuration view.
    Effective {
        /// Effective values with provenance and shadowed-layer info.
        values: Vec<EffectiveConfigValue>,
        /// Automatic review switches summary JSON.
        review: Value,
        /// Crew purposes projection JSON.
        crew_purposes: Value,
        /// Effective `[workflow.review_skip_tags]` entries as `(tag, paths, guard)`.
        review_skip_tags: Vec<(String, Vec<String>, Option<String>)>,
        /// Resolved roots and binding metadata.
        roots: ConfigShowRoots,
    },
    /// Single physical file resolved in isolation.
    Scoped {
        /// Opened config store for the requested file.
        store: ConfigStore,
        /// Resolved single-file snapshot.
        snapshot: Box<ConfigSnapshot>,
        /// Resolved roots and binding metadata.
        roots: ConfigShowRoots,
    },
}

pub(super) fn config_path(core: &Core, global: bool) -> Result<PathBuf, OrbitError> {
    let runtime = core.runtime();
    if global {
        Ok(runtime.global_root().join("config.toml"))
    } else {
        runtime.config_path()
    }
}

pub(super) fn get(
    core: &Core,
    key: &str,
    scope: ConfigReadScope,
) -> Result<ConfigGetResult, OrbitError> {
    let runtime = core.runtime();
    if scope == ConfigReadScope::Effective {
        admit_config_key(key)?;
        let effective = load_effective_config(runtime.env(), &config_roots(core))?;
        let value = effective.value_for(key).unwrap_or(Value::Null);
        return Ok(ConfigGetResult::Effective { value });
    }

    let store = open_store_for_scope(core, scope)?;
    admit_config_key(key)?;
    let file_exists = store.exists_on_disk();
    let value = store.effective_value(key)?;
    let exists = store.is_key_set(key);
    let path = file_exists.then(|| store.path().to_path_buf());
    Ok(ConfigGetResult::Scoped {
        scope: store.scope().label(),
        path,
        value,
        exists,
    })
}

pub(super) fn show(core: &Core, scope: ConfigReadScope) -> Result<ConfigShowData, OrbitError> {
    let runtime = core.runtime();
    if scope == ConfigReadScope::Effective {
        let effective = load_effective_config(runtime.env(), &config_roots(core))?;
        let values = effective.values().to_vec();
        let review = crate::application::review::review_switches_view(
            runtime,
            runtime.operation_policy(),
            chrono::Utc::now(),
        );
        let crew_purposes = super::purpose::crew_purposes_view(&values);
        let review_skip_tags = runtime
            .review_skip_settings()
            .entries()
            .map(|(tag, profile)| {
                (
                    tag.to_string(),
                    profile.paths.clone(),
                    profile.guard.clone(),
                )
            })
            .collect();
        let roots = show_roots(core, None);
        return Ok(ConfigShowData::Effective {
            values,
            review,
            crew_purposes,
            review_skip_tags,
            roots,
        });
    }

    let store = open_store_for_scope(core, scope)?;
    let snapshot = Box::new(store.snapshot_with_global(&runtime.global_root())?);
    let roots = show_roots(core, Some(store.path()));
    Ok(ConfigShowData::Scoped {
        store,
        snapshot,
        roots,
    })
}

fn config_roots(core: &Core) -> ConfigRoots {
    let runtime = core.runtime();
    ConfigRoots::new(runtime.global_root(), runtime.shared_root())
}

fn open_store_for_scope(core: &Core, scope: ConfigReadScope) -> Result<ConfigStore, OrbitError> {
    let runtime = core.runtime();
    let global_path = runtime.global_root().join("config.toml");
    let workspace_path = runtime.shared_root().join("config.toml");
    let (resolved_scope, path) = match scope {
        ConfigReadScope::Global => (ConfigScope::Global, global_path),
        ConfigReadScope::Workspace => (ConfigScope::Workspace, workspace_path),
        ConfigReadScope::Effective => {
            if workspace_path.exists() && config_roots(core).has_workspace_layer() {
                (ConfigScope::Workspace, workspace_path)
            } else {
                (ConfigScope::Global, global_path)
            }
        }
    };
    ConfigStore::open(runtime.env(), resolved_scope, path)
}

fn show_roots(core: &Core, config_path: Option<&Path>) -> ConfigShowRoots {
    let runtime = core.runtime();
    ConfigShowRoots {
        global_root: runtime.global_root(),
        shared_root: runtime.shared_root(),
        local_root: runtime.local_root(),
        has_workspace_layer: config_roots(core).has_workspace_layer(),
        workspace_binding: runtime.workspace_runtime_binding().map(|binding| {
            ConfigShowWorkspaceBinding {
                base_branch: binding.base_branch.clone(),
                ship_mode: binding.ship_mode,
                repo_root: binding.repo_root.clone(),
            }
        }),
        persistence: runtime.persistence_config_json(),
        path_rows: super::path::path_rows(runtime, config_path),
    }
}
