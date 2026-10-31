//! The operator's human crew `orbit init` seeds: `[crews.<name>] kind =
//! "human"` in the global `config.toml`, named after the OS user unless the
//! operator answers or passes `--human-name`. `orbit-config` owns the write;
//! the terminal side lives here.

use std::io::{self, Write};
use std::path::Path;

use orbit_config::{
    AgentCrewNames, HumanCrewSeedOutcome, human_crew_seed_collides, normalize_human_crew_name,
    os_user_human_crew_name, seed_operator_human_crew,
};
use orbit_core::OrbitError;

use super::prompt_stdin;
use crate::output::sink;

const MAX_HUMAN_CREW_NAME_ATTEMPTS: usize = 4;

/// The human crew as `orbit init` reports it.
pub(super) struct HumanCrewReport {
    pub(super) outcome: &'static str,
    pub(super) name: String,
}

/// Refuse, before init writes anything, a human crew name it would refuse
/// later: a `--human-name` that normalizes to nothing, or one (or, with no
/// prompt to re-ask, the OS user) that is an agent crew. The seed re-checks
/// under the config lock.
pub(super) fn reject_unusable_human_name(
    config_path: &Path,
    force: bool,
    non_interactive: bool,
    requested: Option<&str>,
) -> Result<(), OrbitError> {
    let (name, flag) = match requested {
        Some(raw) => (
            Some(normalize_human_crew_name(raw).ok_or_else(|| unusable_name(raw))?),
            true,
        ),
        None if non_interactive || !sink::stdin_is_terminal() => (os_user_human_crew_name(), false),
        None => return Ok(()),
    };
    let Some(name) = name else {
        return Ok(());
    };
    if !human_crew_seed_collides(config_path, &name, force)? {
        return Ok(());
    }
    Err(if flag {
        flag_collision(&name)
    } else {
        os_user_collision(&name)
    })
}

/// Create the operator's human crew unless the config already has one. The
/// prompt runs only at a terminal; a pipe, `--non-interactive` or a CI runner
/// takes the OS user without asking.
pub(super) fn ensure_human_crew_for_init(
    config_path: &Path,
    non_interactive: bool,
    requested: Option<String>,
) -> Result<HumanCrewReport, OrbitError> {
    let flag = requested.clone();
    let outcome = seed_operator_human_crew(config_path, |agent_crews| {
        if let Some(raw) = flag {
            let name = normalize_human_crew_name(&raw).ok_or_else(|| unusable_name(&raw))?;
            if agent_crews.contains(&name) {
                return Err(flag_collision(&name));
            }
            return Ok(name);
        }
        let default = os_user_human_crew_name();
        if !non_interactive && sink::stdin_is_terminal() {
            let prompt = match default.as_deref() {
                Some(name) => format!("Human crew name for your own work [{name}]: "),
                None => "Human crew name for your own work: ".to_string(),
            };
            let stderr = io::stderr();
            let mut output = stderr.lock();
            return collect_human_crew_name(
                &prompt,
                default,
                agent_crews,
                &mut output,
                |prompt, output| {
                    prompt_stdin::read_trimmed_line(prompt, output)
                        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
                },
            );
        }
        let name = default.ok_or_else(|| {
            OrbitError::InvalidInput(
                "the OS user name is unavailable for the human crew; pass --human-name <NAME>"
                    .to_string(),
            )
        })?;
        if agent_crews.contains(&name) {
            return Err(os_user_collision(&name));
        }
        Ok(name)
    })?;
    if let HumanCrewSeedOutcome::Unchanged(existing) = &outcome
        && let Some(raw) = requested
            .filter(|raw| normalize_human_crew_name(raw).as_deref() != Some(existing.as_str()))
    {
        eprintln!(
            "warning: --human-name '{raw}' ignored: the human crew '{existing}' already exists"
        );
    }
    Ok(HumanCrewReport {
        outcome: match outcome {
            HumanCrewSeedOutcome::Created(_) => "created",
            HumanCrewSeedOutcome::Unchanged(_) => "unchanged",
        },
        name: outcome.name().to_string(),
    })
}

fn collect_human_crew_name<W, F>(
    prompt: &str,
    default: Option<String>,
    agent_crews: &AgentCrewNames,
    output: &mut W,
    mut read_answer: F,
) -> Result<String, OrbitError>
where
    W: Write,
    F: FnMut(&str, &mut W) -> Result<String, OrbitError>,
{
    for _ in 0..MAX_HUMAN_CREW_NAME_ATTEMPTS {
        let answer = read_answer(prompt, output)?;
        let name = if answer.is_empty() {
            default.clone()
        } else {
            normalize_human_crew_name(&answer)
        };
        let complaint = match name {
            Some(name) if !agent_crews.contains(&name) => return Ok(name),
            Some(name) => format!("'{name}' is already an agent crew; choose another name"),
            None if answer.is_empty() => {
                "no OS user name is available; enter a name for your human crew".to_string()
            }
            None => format!("'{answer}' has no usable characters; use letters, digits, '_' or '-'"),
        };
        writeln!(output, "{complaint}").map_err(|error| OrbitError::Io(error.to_string()))?;
    }
    Err(OrbitError::InvalidInput(format!(
        "human crew name remained unusable after {MAX_HUMAN_CREW_NAME_ATTEMPTS} attempts; pass \
         --human-name"
    )))
}

fn flag_collision(name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "--human-name '{name}' is already an agent crew; pass a different --human-name"
    ))
}

fn os_user_collision(name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "the OS user '{name}' is already an agent crew name, so it cannot name your human crew; \
         pass --human-name <NAME>"
    ))
}

fn unusable_name(raw: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "--human-name '{raw}' has no usable characters; use letters, digits, '_' or '-'"
    ))
}
