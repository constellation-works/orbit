//! Plugin build planning and installation through exported helpers.
use orbit_types::plugin::{PluginBuildOutput, PluginBuildSpec};

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

mod env;
mod install;
mod plan;
