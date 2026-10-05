//! Shared helpers for the `orbit config` subcommand family: `--scope`
//! resolution and the two well-known config file paths (global/workspace).
//! Domain logic (validation, file I/O) stays in `orbit_config`; these
//! helpers only translate CLI flags into `orbit-core` calls.

use std::path::PathBuf;

use clap::ValueEnum;
use orbit_config::{ConfigRoots, ConfigScope, ConfigStore};
use orbit_core::{OrbitError, OrbitRuntime};

/// `--scope` value shared by `orbit config show` and `orbit config get`.
///
/// `Effective` asks callers to use the layered runtime view (see the
/// `orbit_config` module documentation). `Global`/`Workspace` always
/// read one specific file directly, without applying the other layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ConfigScopeArg {
    #[default]
    Effective,
    Global,
    Workspace,
}

/// The two roots `orbit config show`/`get` layer, taken from the open runtime.
/// `orbit-config` resolves no roots of its own, so the CLI states them.
pub(super) fn runtime_config_roots(runtime: &OrbitRuntime) -> ConfigRoots {
    ConfigRoots::new(runtime.global_root(), runtime.shared_root())
}

/// The global `config.toml` path, whether or not it exists on disk.
pub(super) fn global_config_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.global_root().join("config.toml")
}

/// The workspace `config.toml` path, whether or not it exists on disk.
pub(super) fn workspace_config_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.shared_root().join("config.toml")
}

/// Resolve `--scope` into a concrete file path. For `Effective`, this returns
/// the highest-precedence file path for display and compatibility only; show
/// and get load both layers through `orbit_config::load_effective_config`.
fn resolve_scope(runtime: &OrbitRuntime, scope: ConfigScopeArg) -> (ConfigScope, PathBuf) {
    match scope {
        ConfigScopeArg::Global => (ConfigScope::Global, global_config_path(runtime)),
        ConfigScopeArg::Workspace => (ConfigScope::Workspace, workspace_config_path(runtime)),
        ConfigScopeArg::Effective => {
            let workspace_path = workspace_config_path(runtime);
            let global_path = global_config_path(runtime);
            if workspace_path.exists() && runtime.shared_root() != runtime.global_root() {
                (ConfigScope::Workspace, workspace_path)
            } else {
                (ConfigScope::Global, global_path)
            }
        }
    }
}

/// Open a [`ConfigStore`] for read-only operations (`show`, `get`) at the
/// requested `--scope`.
pub(super) fn open_store_for_scope(
    runtime: &OrbitRuntime,
    scope: ConfigScopeArg,
) -> Result<ConfigStore, OrbitError> {
    let (resolved_scope, path) = resolve_scope(runtime, scope);
    ConfigStore::open(resolved_scope, path)
}
