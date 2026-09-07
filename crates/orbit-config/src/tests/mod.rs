mod layering;
mod operation;
mod resolved;
mod seed;
mod store;

use std::path::Path;

use crate::ConfigRoots;

/// Write a `config.toml` into one of the two root directories a test set up.
fn write_config(dir: &Path, body: &str) {
    std::fs::write(dir.join("config.toml"), body).expect("write config");
}

/// Layer `workspace` over `global` for a test that owns both temp dirs.
fn roots(global: &Path, workspace: &Path) -> ConfigRoots {
    ConfigRoots::new(global, workspace)
}
