mod agent_stall;
mod crew_pools;
mod machine;
mod seed;

use crate::ConfigRoots;
use std::path::Path;

/// Write a `config.toml` into one of the two root directories a test set up.
pub(super) fn write_config(dir: &Path, body: &str) {
    std::fs::write(dir.join("config.toml"), body).expect("write config");
}

/// Layer `workspace` over `global` for a test that owns both temp dirs.
pub(super) fn roots(global: &Path, workspace: &Path) -> ConfigRoots {
    ConfigRoots::new(global, workspace)
}
