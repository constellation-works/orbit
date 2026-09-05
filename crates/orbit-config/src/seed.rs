//! Rendering and writing a fresh default `config.toml`.
//!
//! Seeding freezes agent-dependent choices at `orbit init` time so runtime
//! config loading never probes `PATH` or the environment. The probing itself
//! is not done here: the CLI init adapter detects installed provider CLIs,
//! runs any interactive prompts, and hands the answers over as a
//! [`ConfigSeed`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::write_text_with_parent;

use crate::raw::{CrewSeed, RawCrewEntry};
use crate::registry::DEFAULT_WORKFLOW_SYSTEM_CREW;
use crate::resolved::default_crews;

pub(crate) const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../assets/default-config.toml");

/// Crew families Orbit ships crews for, in the order a seeded config prefers
/// them. `ollama` is deliberately absent: Orbit ships no `ollama` crew.
/// Antigravity occupies Gemini's previous default-provider slot so a host
/// with `agy` prefers the current Google terminal CLI. Legacy `gemini` stays
/// later so enterprise Gemini CLI-only hosts still seed. Copilot, Cursor, and
/// Pi remain appended after the original families. [ORB-10946] [ORB-10945]
/// [ORB-11296] [ORB-11299]
const CREW_FAMILY_PREFERENCE: &[&str] = &[
    "claude",
    "codex",
    "antigravity",
    "gemini",
    "grok",
    "copilot",
    "cursor",
    "pi",
];

/// Explicit, host-independent inputs for rendering a fresh `config.toml`.
///
/// A seed says which provider families this machine can actually dispatch to
/// and, optionally, which crew assignments an operator chose. Everything else
/// — the model tier per lane, the crew table layout, the default-crew key —
/// is config policy and stays in this crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSeed {
    /// Provider families available on this host. An empty set seeds an
    /// explicitly empty `[crews]` registry, which is how a host that can
    /// dispatch nothing avoids inheriting the built-in crews at load time.
    pub families: BTreeSet<String>,
    /// Crew assignments chosen by the caller, keyed by crew name. A `custom`
    /// entry becomes `workflow.default_crew`; `system` overrides the
    /// family-derived default for that lane.
    pub crews: BTreeMap<String, CrewSeed>,
}

impl ConfigSeed {
    /// Build a seed from the detected family names, keeping only families
    /// Orbit ships crews for.
    pub fn from_families<I, S>(families: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            families: families
                .into_iter()
                .map(|family| family.as_ref().to_string())
                .filter(|family| CREW_FAMILY_PREFERENCE.contains(&family.as_str()))
                .collect(),
            crews: BTreeMap::new(),
        }
    }

    /// Attach the crew assignments an operator chose interactively.
    pub fn with_crews(mut self, crews: BTreeMap<String, CrewSeed>) -> Self {
        self.crews = crews;
        self
    }

    /// Available families in Orbit's fixed preference order.
    fn available_families(&self) -> Vec<&'static str> {
        CREW_FAMILY_PREFERENCE
            .iter()
            .copied()
            .filter(|family| self.families.contains(*family))
            .collect()
    }

    fn has_family(&self, family: &str) -> bool {
        self.families.contains(family)
    }

    fn chosen_crew(&self, name: &str) -> Option<CrewSeed> {
        self.crews.get(name).cloned()
    }

    fn has_custom_crew(&self) -> bool {
        self.crews.contains_key("custom")
    }
}

/// Write a fresh `config.toml` at `config_path`, returning whether one was
/// created. An existing file is never overwritten, so `orbit init` stays
/// idempotent.
///
/// `seed` of `None` renders the static template alone: no `[crews]` table and
/// no `workflow.default_crew`, so config loading falls back to the built-in
/// crew registry. That is the shape used by implicit bootstrap, which has no
/// operator present to detect a host for.
pub fn seed_default_config(
    config_path: &Path,
    seed: Option<&ConfigSeed>,
) -> Result<bool, OrbitError> {
    if config_path.exists() {
        return Ok(false);
    }
    let body = render_seeded_config(DEFAULT_CONFIG_TEMPLATE, seed)?;
    write_text_with_parent(config_path, &body)?;
    Ok(true)
}

fn render_seeded_config(template: &str, seed: Option<&ConfigSeed>) -> Result<String, OrbitError> {
    if let Some(custom) = seed.and_then(|seed| seed.crews.get("custom")) {
        validate_complete_crew_setting(custom)?;
    }

    let mut body = template.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let Some(seed) = seed else {
        return Ok(body);
    };

    // Agent detection is frozen at init; runtime config loading never probes
    // PATH or the environment.
    let workflow_default = render_workflow_default_crew(seed);
    if !workflow_default.is_empty() {
        // L-0100: generated TOML keys must be inserted inside their intended table.
        let marker = "[workflow]\n";
        let insertion = body.find(marker).ok_or_else(|| {
            OrbitError::InvalidInput("default config template is missing [workflow]".to_string())
        })? + marker.len();
        body.insert_str(insertion, &workflow_default);
    }
    body.push('\n');
    body.push_str(&render_crews(seed)?);
    Ok(body)
}

fn render_workflow_default_crew(seed: &ConfigSeed) -> String {
    let default_crew = if seed.has_custom_crew() {
        Some("custom")
    } else {
        default_crew_name(seed)
    };
    default_crew.map_or_else(String::new, |name| format!("default_crew = \"{name}\"\n"))
}

/// Default crew name frozen into newly seeded config. The result always names
/// the first emitted crew for the preferred available family.
fn default_crew_name(seed: &ConfigSeed) -> Option<&'static str> {
    seed.available_families()
        .first()
        .map(|family| match *family {
            "claude" => "opus",
            "codex" => "astra",
            "antigravity" => "antigravity",
            "gemini" => "gemini",
            "grok" => "grok",
            "copilot" => "copilot",
            "cursor" => "cursor",
            "pi" => "pi",
            _ => unreachable!("available crew families are fixed"),
        })
}

fn render_crews(seed: &ConfigSeed) -> Result<String, OrbitError> {
    let available_families = seed.available_families();
    let mut crews: BTreeMap<String, RawCrewEntry> = default_crews()
        .into_iter()
        .filter(|(_, crew)| available_families.contains(&crew.assignment.provider.as_str()))
        .map(|(name, crew)| {
            (
                name,
                RawCrewEntry {
                    provider: Some(crew.assignment.provider),
                    model: Some(crew.assignment.model),
                    effort: crew.assignment.effort.map(|value| value.to_string()),
                    backend: None,
                    description: crew.description,
                    tags: crew.tags,
                    planner: None,
                    implementer: None,
                    reviewer: None,
                },
            )
        })
        .collect();

    if let Some(assignment) = seed.chosen_crew("custom") {
        crews.insert(
            "custom".to_string(),
            RawCrewEntry {
                provider: assignment.provider,
                model: assignment.model,
                effort: None,
                backend: None,
                description: None,
                tags: Vec::new(),
                planner: None,
                implementer: None,
                reviewer: None,
            },
        );
    }

    let name = DEFAULT_WORKFLOW_SYSTEM_CREW;
    if let Some(assignment) = seed.chosen_crew(name).or_else(|| default_system_crew(seed)) {
        crews.insert(
            name.to_string(),
            RawCrewEntry {
                provider: assignment.provider,
                model: assignment.model,
                effort: None,
                backend: None,
                description: None,
                tags: Vec::new(),
                planner: None,
                implementer: None,
                reviewer: None,
            },
        );
    }

    let mut rendered = String::new();
    for (name, entry) in crews {
        rendered.push_str(&render_crew_table(&name, &entry)?);
    }
    if rendered.is_empty() {
        // Preserve an explicitly empty registry so runtime loading does not
        // substitute built-in crews for a host where init detected none.
        rendered.push_str("[crews]\n");
    }
    Ok(rendered)
}

/// Seed the bounded system lane: step-failure recovery, failed-run triage, and
/// the read-only task pilot. That work is high-volume and low-judgment, so this
/// picks the cheapest tier each family offers rather than the family's default
/// model — seeding a mid-tier crew here multiplies the cost of every unattended
/// sweep for no gain.
///
/// The order below is a preference list, not a strict price sort. Gemini Flash
/// undercuts both Sonnet and Grok per token but sits last because observed runs
/// have failed outright on quota; a crew that does not finish costs more than a
/// pricier one that does. Adjust the order here rather than teaching callers to
/// special-case a family.
fn default_system_crew(seed: &ConfigSeed) -> Option<CrewSeed> {
    use orbit_common::model_defaults::{
        ANTIGRAVITY_CREW_MODEL, CLAUDE_DEFAULT_WEAK, CODEX_LUNA_MODEL, COPILOT_CREW_MODEL,
        CURSOR_CREW_MODEL, GEMINI_CREW_MODEL, GROK_DEFAULT_MODEL, PI_CREW_MODEL,
    };
    let (provider, model) = if seed.has_family("codex") {
        ("codex", CODEX_LUNA_MODEL)
    } else if seed.has_family("claude") {
        ("claude", CLAUDE_DEFAULT_WEAK)
    } else if seed.has_family("grok") {
        ("grok", GROK_DEFAULT_MODEL)
    } else if seed.has_family("antigravity") {
        ("antigravity", ANTIGRAVITY_CREW_MODEL)
    } else if seed.has_family("gemini") {
        ("gemini", GEMINI_CREW_MODEL)
    } else if seed.has_family("copilot") {
        ("copilot", COPILOT_CREW_MODEL)
    } else if seed.has_family("cursor") {
        ("cursor", CURSOR_CREW_MODEL)
    } else if seed.has_family("pi") {
        ("pi", PI_CREW_MODEL)
    } else {
        return None;
    };
    Some(CrewSeed {
        provider: Some(provider.to_string()),
        model: Some(model.to_string()),
    })
}

fn render_crew_table(name: &str, entry: &RawCrewEntry) -> Result<String, OrbitError> {
    let mut rendered = format!("[crews.{name}]\n");
    for (field, value) in [
        ("model", entry.model.as_deref()),
        ("provider", entry.provider.as_deref()),
    ] {
        let value = value.ok_or_else(|| {
            OrbitError::InvalidInput(format!("crew `{name}` is missing `{field}`"))
        })?;
        rendered.push_str(&format!(
            "{field} = {}\n",
            toml::Value::String(value.to_string())
        ));
    }
    if let Some(description) = entry
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        rendered.push_str(&format!(
            "description = {}\n",
            toml::Value::String(description.to_string())
        ));
    }
    if !entry.tags.is_empty() {
        let tags = entry
            .tags
            .iter()
            .map(|tag| toml::Value::String(tag.clone()))
            .collect::<Vec<_>>();
        rendered.push_str(&format!("tags = {}\n", toml::Value::Array(tags)));
    }
    rendered.push('\n');
    Ok(rendered)
}

fn validate_complete_crew_setting(config: &CrewSeed) -> Result<(), OrbitError> {
    for (field, value) in [
        ("provider", config.provider.as_deref()),
        ("model", config.model.as_deref()),
    ] {
        if value.map(str::trim).is_none_or(str::is_empty) {
            return Err(OrbitError::InvalidInput(format!(
                "custom crew is missing required `{field}`"
            )));
        }
    }
    Ok(())
}
