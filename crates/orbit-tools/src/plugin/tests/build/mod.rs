//! The two security invariants of a build the install boundary cannot reach
//! without a working build sandbox: what environment a phase receives, and
//! what a hostile build directory can make Orbit install.

use orbit_types::plugin::{PluginBuildOutput, PluginBuildSpec};

mod env;
mod install;
mod plan;

fn spec(outputs: &[(&str, &str)]) -> PluginBuildSpec {
    PluginBuildSpec {
        programs: vec!["sh".to_string()],
        fetch: None,
        command: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
        outputs: outputs
            .iter()
            .map(|(from, to)| PluginBuildOutput {
                from: (*from).to_string(),
                to: (*to).to_string(),
            })
            .collect(),
        timeout_ms: None,
    }
}
