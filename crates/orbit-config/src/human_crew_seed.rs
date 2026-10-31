//! `orbit init` seeding of the operator's human crew in the global
//! `config.toml`: `[crews.<name>] kind = "human"`.
//!
//! The terminal side (prompting, the OS user default) lives in the CLI; this
//! module owns the document edit, so the write takes the config write lock and
//! never replaces a human crew an earlier init or the operator wrote.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::identity::HUMAN_CREW_KIND;

use crate::raw::RawRuntimeConfig;
use crate::resolved::{crews_from_raw, human_crews_from_raw};
use crate::store::{ConfigScope, ConfigStore};

/// What [`seed_operator_human_crew`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HumanCrewSeedOutcome {
    /// No human crew existed; this one was written.
    Created(String),
    /// A human crew already existed and was left unchanged.
    Unchanged(String),
}

impl HumanCrewSeedOutcome {
    /// The human crew's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Created(name) | Self::Unchanged(name) => name,
        }
    }
}

/// The agent crew names a new human crew must not take: the document's agent
/// crews, or the built-in ones when it declares none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentCrewNames(BTreeSet<String>);

impl AgentCrewNames {
    /// Whether `name` is an agent crew.
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// Normalize an operator name into a crew name: lowercase ASCII letters,
/// digits, `_` and `-`, with every other run of characters folded into one
/// `-`. `None` when nothing usable is left.
pub fn normalize_human_crew_name(raw: &str) -> Option<String> {
    let mut name = String::new();
    for character in raw.trim().chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() || character == '_' {
            name.push(character);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-');
    (!name.is_empty()).then(|| name.to_string())
}

/// The OS user as a crew name, the default `orbit init` offers for the
/// operator's human crew: `$USER` (or `USERNAME`, `LOGNAME`), else the passwd
/// entry, normalized. `None` when none yields a usable name.
pub fn os_user_human_crew_name() -> Option<String> {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(orbit_common::security::child_env::os_login_name)
        .and_then(|name| normalize_human_crew_name(&name))
}

/// Whether seeding `name` as the operator's human crew would collide with an
/// agent crew, judged before init writes anything so a refusal leaves no
/// partial root. `false` when `config_path` already declares a human crew
/// (seeding leaves it unchanged). `fresh` judges the config as `--force`
/// will rewrite it: built-in agent crews only.
pub fn human_crew_seed_collides(
    config_path: &Path,
    name: &str,
    fresh: bool,
) -> Result<bool, OrbitError> {
    let builtin = crate::resolved::default_crews();
    if fresh || !config_path.exists() {
        return Ok(builtin.contains_key(name));
    }
    let text = std::fs::read_to_string(config_path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", config_path.display())))?;
    let raw = toml::from_str::<RawRuntimeConfig>(&text)
        .map_err(|error| OrbitError::InvalidInput(format!("invalid global config: {error}")))?;
    if !human_crews_from_raw(raw.crews.as_ref(), config_path)?.is_empty() {
        return Ok(false);
    }
    let (agent_crews, _) =
        crews_from_raw(raw.crews.as_ref(), config_path, &|_, _| config_path.into())?;
    // Init refreshes the built-in crews into the document, so they count too.
    Ok(agent_crews.contains_key(name) || builtin.contains_key(name))
}

/// Create the operator's human crew in the global `config.toml` at
/// `config_path` unless one exists. `choose` runs only when one must be
/// created: it receives the agent crew names and returns the normalized name
/// to write, or refuses. Runs under the config write lock, so a concurrent
/// init cannot write a second human crew.
pub fn seed_operator_human_crew(
    config_path: &Path,
    choose: impl FnOnce(&AgentCrewNames) -> Result<String, OrbitError>,
) -> Result<HumanCrewSeedOutcome, OrbitError> {
    ConfigStore::with_write_lock(config_path, || {
        let mut store = ConfigStore::open(ConfigScope::Global, config_path)?;
        let raw = toml::from_str::<RawRuntimeConfig>(store.raw_text())
            .map_err(|error| OrbitError::InvalidInput(format!("invalid global config: {error}")))?;
        let human_crews = human_crews_from_raw(raw.crews.as_ref(), config_path)?;
        if let Some(name) = human_crews.keys().next() {
            return Ok(HumanCrewSeedOutcome::Unchanged(name.clone()));
        }
        let (agent_crews, _) =
            crews_from_raw(raw.crews.as_ref(), config_path, &|_, _| config_path.into())?;
        let agent_names = AgentCrewNames(agent_crews.into_keys().collect());
        let name = choose(&agent_names)?;
        if normalize_human_crew_name(&name).as_deref() != Some(name.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "human crew name '{name}' is not normalized; use lowercase letters, digits, '_' \
                 and '-'"
            )));
        }
        if agent_names.contains(&name) {
            return Err(OrbitError::InvalidInput(format!(
                "human crew name '{name}' is already an agent crew; choose another name"
            )));
        }
        store.set_document_value(
            &format!("crews.{name}.kind"),
            &toml_edit::Value::from(HUMAN_CREW_KIND).to_string(),
        )?;
        store.set_document_value(
            &format!("crews.{name}.description"),
            &toml_edit::Value::from("Operator work for this person; never dispatched to an agent")
                .to_string(),
        )?;
        // Narrower than `validate`, as the machine identity write is: an
        // unrelated admission problem elsewhere must not stop init, but the
        // crew it stages must read back as human.
        let staged = toml::from_str::<RawRuntimeConfig>(&store.staged_text()).map_err(|error| {
            OrbitError::InvalidInput(format!("staged human crew is not valid TOML: {error}"))
        })?;
        if !human_crews_from_raw(staged.crews.as_ref(), config_path)?.contains_key(&name) {
            return Err(OrbitError::InvalidInput(format!(
                "staged human crew '{name}' does not read back as human; nothing was written"
            )));
        }
        store.save()?;
        Ok(HumanCrewSeedOutcome::Created(name))
    })
}
