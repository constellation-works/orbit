//! Turn host detection and interactive answers into an `orbit_config::ConfigSeed`.
//!
//! This is the whole adapter between the terminal/host and the config crate:
//! detection and prompting happen here, and everything that crosses into
//! `orbit-config` is plain data — the detected families plus, interactively,
//! the names of the seeded crews the operator chose for the two workflow lanes.

use std::io;
use std::path::{Path, PathBuf};

use orbit_config::ConfigSeed;
use orbit_core::OrbitError;
use orbit_registry::workspace_registry::global_orbit_dir;

use super::agent_detect::{DetectedAgents, RealAgentEnvProbe, available_crew_families, detect};
use super::agent_prompt::{Prompter, StdinPrompter, collect_default_crew, collect_system_crew};

/// Probe the host and build the seed for `orbit init`.
///
/// Prompts run only when ALL of:
/// - `--non-interactive` is unset
/// - the target config.toml does not already exist (or `--force` is set, which
///   wipes it)
///
/// A non-interactive run still seeds from detected agent surfaces with the
/// seed's own recommendations; only the crew prompts are skipped.
pub(crate) fn collect_config_seed_for_init(
    root_override: Option<&Path>,
    force: bool,
    non_interactive: bool,
) -> Result<ConfigSeed, OrbitError> {
    let detected = detect(&RealAgentEnvProbe);
    let seed = config_seed_from_detection(&detected);
    if non_interactive || !config_would_be_written(root_override, force)? {
        return Ok(seed);
    }
    let mut prompter = StdinPrompter;
    collect_interactive_crew_choices(&detected, seed, &mut prompter)
        .map_err(|err| OrbitError::Io(format!("agent prompts failed: {err}")))
}

/// The host-blind projection of a detection snapshot: which crew families this
/// machine can actually dispatch to.
pub(crate) fn config_seed_from_detection(detected: &DetectedAgents) -> ConfigSeed {
    ConfigSeed::from_families(available_crew_families(detected))
}

/// Whether init will write a fresh config.toml, which is the only case worth
/// prompting for — `orbit init` is idempotent over an existing global root.
pub(crate) fn config_would_be_written(
    root_override: Option<&Path>,
    force: bool,
) -> Result<bool, OrbitError> {
    Ok(force || !resolve_config_path(root_override)?.exists())
}

/// Prompt for the default crew and, when more than one cheap-tier family is
/// detected, the system crew; both by seeded crew name. Does not prompt for QA.
pub(crate) fn collect_interactive_crew_choices(
    detected: &DetectedAgents,
    seed: ConfigSeed,
    prompter: &mut dyn Prompter,
) -> io::Result<ConfigSeed> {
    let mut seed = seed;
    if let Some(default_crew) = collect_default_crew(detected, &seed, prompter)? {
        seed = seed.with_default_crew(default_crew);
    }
    if let Some(system_crew) = collect_system_crew(&seed, prompter)? {
        seed = seed.with_system_crew(system_crew);
    }
    Ok(seed)
}

fn resolve_config_path(root_override: Option<&Path>) -> Result<PathBuf, OrbitError> {
    let root = match root_override {
        Some(root) => root.to_path_buf(),
        None => global_orbit_dir()?,
    };
    Ok(root.join("config.toml"))
}
