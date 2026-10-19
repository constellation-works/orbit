use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::config::{ConfigScope, ConfigWriteInit, set_key_literal};

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

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

impl Execute for ConfigSetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
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
        let outcome = set_key_literal(runtime, &self.key, &self.value, scope, init)?;
        let scope = outcome.scope;
        let path = outcome.path.to_string_lossy().into_owned();
        Ok(Payload::detail(
            json!({ "key": self.key, "scope": scope, "path": path }),
            format!("set {} ({scope} config: {path})", self.key),
        )
        .into())
    }
}
