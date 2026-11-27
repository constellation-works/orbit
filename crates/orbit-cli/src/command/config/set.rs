use std::path::Path;

use clap::Args;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::Env;
use orbit_common::OrbitError;
use orbit_core::Core;
use orbit_core::app::config::{
    Command, ConfigScope, ConfigWriteInit, Outcome, exec,
    refuse_indirect_uninitialized_caller_orbit,
};

use crate::command::CommandOut;

#[derive(Args)]
pub struct ConfigSetArgs {
    /// Dotted config.toml key, e.g. workflow.base_branch
    pub key: String,
    /// Value to set, parsed as a TOML literal (bool/int/float/array/etc),
    /// falling back to a plain string when it doesn't parse as one
    pub value: String,
    /// Target the global config (~/.orbit/config.toml) instead of the
    /// workspace one
    #[arg(long, conflicts_with_all = ["seed_from_global", "fresh"])]
    pub global: bool,
    /// When the workspace config.toml doesn't exist yet, seed it from the
    /// current global config before applying this edit
    #[arg(long = "seed-from-global", conflicts_with = "fresh")]
    pub seed_from_global: bool,
    /// When the workspace config.toml doesn't exist yet, start from an
    /// empty document before applying this edit
    #[arg(long)]
    pub fresh: bool,
}

impl ConfigSetArgs {
    pub(super) fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        let scope = if self.global {
            ConfigScope::Global
        } else {
            ConfigScope::Workspace
        };
        let init = if self.seed_from_global {
            ConfigWriteInit::SeedFromGlobal
        } else if self.fresh {
            ConfigWriteInit::Fresh
        } else {
            ConfigWriteInit::RequireExisting
        };
        let Outcome::SetLiteral {
            outcome,
            global_root,
        } = exec(
            core,
            Command::SetLiteral {
                key: self.key.clone(),
                value: self.value,
                scope,
                init,
            },
        )?
        else {
            unreachable!("config set returned an unrelated outcome");
        };
        let warnings = orbit_cmd::service_environment::crew_env_config_write_warnings_for_root(
            &global_root,
            &self.key,
            &outcome.new_value,
            env,
        );
        Ok(
            crate::output::config::set_payload(&self.key, outcome.scope, &outcome.path, &warnings)
                .into(),
        )
    }
}

/// Refuse a workspace `config set` before runtime open when the working
/// directory's uninitialized `.orbit/` is not the workspace that resolution
/// selected. Opening the runtime would otherwise bootstrap that other root.
/// An explicit workspace selector names its target directly, so the working
/// directory's roots are never resolved in that case.
pub fn preflight_indirect_workspace_write(
    root_override: Option<&Path>,
    global: bool,
    explicit_workspace: bool,
    env: &Env,
) -> Result<(), OrbitError> {
    if global || root_override.is_some() || explicit_workspace {
        return Ok(());
    }
    let cwd = std::env::current_dir().map_err(|err| OrbitError::Io(err.to_string()))?;
    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&cwd, None)?;
    refuse_indirect_uninitialized_caller_orbit(env, &cwd, &roots.shared_root, false)
}
