use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::identity::{Crew, CrewAssignment, ReasoningEffort};
use tempfile::tempdir;

use crate::registry::resolve_default_crew;
use crate::resolved::default_crews;
use crate::tests::{roots, write_config};
use crate::{ConfigSnapshot, ExecutionEnvPolicy, PersistenceConfig, ResolvedConfig};

fn single_family_crew(name: &str) -> Crew {
    let assignment = CrewAssignment {
        model: format!("{name}-model"),
        provider: name.to_string(),
        effort: None,
    };
    Crew {
        name: name.to_string(),
        assignment,
        description: None,
        tags: Vec::new(),
        enabled: true,
    }
}

fn load_config(body: &str) -> Result<ResolvedConfig, OrbitError> {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(workspace.path(), body);
    ResolvedConfig::load(&roots(global.path(), workspace.path()))
}

mod compatibility;
mod config;
mod crew;
mod execution_env;
