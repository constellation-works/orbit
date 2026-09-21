//! Rendering and writing a fresh default `config.toml`.
//!
//! Seeding freezes agent-dependent choices at `orbit init` time so runtime
//! config loading never probes `PATH` or the environment. The probing itself
//! is not done here: the CLI init adapter detects installed provider CLIs,
//! runs any interactive prompts, and hands the answers over as a
//! [`ConfigSeed`].
//!
//! A seeded file names real crews. `workflow.default_crew` and
//! `workflow.system_crew` each point at one of the built-in crews the seed
//! writes for the detected families; init never invents a `custom` or
//! `system` crew table whose only purpose is to be pointed at.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::write_text_with_parent;
use orbit_types::identity::Crew;

use crate::registry::DEFAULT_WORKFLOW_SYSTEM_CREW;
use crate::resolved::default_crews;

pub(crate) const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../assets/default-config.toml");

/// Crew families Orbit ships crews for, in the order a seeded config prefers
/// them. `ollama` is deliberately absent: Orbit ships no `ollama` crew.
/// Antigravity occupies Gemini's previous default-provider slot so a host
/// with `agy` prefers the current Google terminal CLI. Legacy `gemini` stays
/// later so enterprise Gemini CLI-only hosts still seed. Copilot, Cursor, and
/// Pi remain appended after the original families, and OpenCode after those,
/// so adding a lane never reorders an existing host's preference.
/// [ORB-10946] [ORB-10945] [ORB-11296] [ORB-11299] [ORB-11295]
const CREW_FAMILY_PREFERENCE: &[&str] = &[
    "claude",
    "codex",
    "antigravity",
    "gemini",
    "grok",
    "copilot",
    "cursor",
    "pi",
    "opencode",
];

/// The crew each family's default lane runs on, in [`CREW_FAMILY_PREFERENCE`]
/// order. The first available family's entry is the seeded
/// `workflow.default_crew`.
const DEFAULT_CREW_BY_FAMILY: &[(&str, &str)] = &[
    ("claude", "opus"),
    ("codex", "astra"),
    ("antigravity", "antigravity"),
    ("gemini", "gemini"),
    ("grok", "grok"),
    ("copilot", "copilot"),
    ("cursor", "cursor"),
    ("pi", "pi"),
    ("opencode", "opencode"),
];

/// The cheapest built-in crew each family offers, in the order a seeded
/// config prefers them for the bounded system lane: step-failure recovery,
/// PR conflict recovery, and the read-only task pilot. That work is
/// high-volume and low-judgment, so the first available entry becomes
/// `workflow.system_crew` rather than the family's default crew — seeding a
/// mid-tier crew there multiplies the cost of every unattended sweep for no
/// gain.
///
/// The order is a preference list, not a strict price sort. Gemini Flash
/// undercuts both Sonnet and Grok per token but sits later because observed
/// runs have failed outright on quota; a crew that does not finish costs more
/// than a pricier one that does. Adjust the order here rather than teaching
/// callers to special-case a family.
const SYSTEM_CREW_BY_FAMILY: &[(&str, &str)] = &[
    ("codex", "luna"),
    ("claude", "sonnet"),
    ("grok", "grok"),
    ("antigravity", "antigravity"),
    ("gemini", "gemini"),
    ("copilot", "copilot"),
    ("cursor", "cursor"),
    ("pi", "pi"),
    ("opencode", "opencode"),
];

/// Explicit, host-independent inputs for rendering a fresh `config.toml`.
///
/// A seed says which provider families this machine can actually dispatch to
/// and, optionally, which of the resulting crews an operator chose for the
/// two workflow lanes. Everything else — the model tier per lane, the crew
/// table layout, the recommended crew per lane — is config policy and stays
/// in this crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSeed {
    /// Provider families available on this host. An empty set seeds an
    /// explicitly empty `[crews]` registry, which is how a host that can
    /// dispatch nothing avoids inheriting the built-in crews at load time.
    pub families: BTreeSet<String>,
    /// Operator-chosen `workflow.default_crew`, by seeded crew name. `None`
    /// takes [`Self::recommended_default_crew`].
    pub default_crew: Option<String>,
    /// Operator-chosen `workflow.system_crew`, by seeded crew name. `None`
    /// takes [`Self::recommended_system_crew`].
    pub system_crew: Option<String>,
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
            default_crew: None,
            system_crew: None,
        }
    }

    /// Record the crew an operator chose as `workflow.default_crew`.
    pub fn with_default_crew(mut self, name: impl Into<String>) -> Self {
        self.default_crew = Some(name.into());
        self
    }

    /// Record the crew an operator chose as `workflow.system_crew`.
    pub fn with_system_crew(mut self, name: impl Into<String>) -> Self {
        self.system_crew = Some(name.into());
        self
    }

    /// The built-in crews this seed writes: the registry filtered to the
    /// available families, keyed by crew name. The built-in `system` alias is
    /// excluded because a seeded file names its system crew through
    /// `workflow.system_crew` instead.
    pub fn seeded_crews(&self) -> BTreeMap<String, Crew> {
        let available_families = self.available_families();
        default_crews()
            .into_iter()
            .filter(|(name, crew)| {
                name != DEFAULT_WORKFLOW_SYSTEM_CREW
                    && available_families.contains(&crew.assignment.provider.as_str())
            })
            .collect()
    }

    /// Default crew name frozen into a newly seeded config when the operator
    /// chose none: the default crew of the preferred available family.
    pub fn recommended_default_crew(&self) -> Option<&'static str> {
        self.available_families().first().and_then(|family| {
            DEFAULT_CREW_BY_FAMILY
                .iter()
                .find(|(candidate, _)| candidate == family)
                .map(|(_, crew)| *crew)
        })
    }

    /// Cheap-tier crews an operator may pick as `workflow.system_crew`, in
    /// preference order; the first entry is the non-interactive choice.
    pub fn system_crew_options(&self) -> Vec<&'static str> {
        SYSTEM_CREW_BY_FAMILY
            .iter()
            .filter(|(family, _)| self.has_family(family))
            .map(|(_, crew)| *crew)
            .collect()
    }

    /// System crew name frozen into a newly seeded config when the operator
    /// chose none.
    pub fn recommended_system_crew(&self) -> Option<&'static str> {
        self.system_crew_options().first().copied()
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

    fn effective_default_crew(&self) -> Option<String> {
        self.default_crew
            .clone()
            .or_else(|| self.recommended_default_crew().map(str::to_string))
    }

    fn effective_system_crew(&self) -> Option<String> {
        self.system_crew
            .clone()
            .or_else(|| self.recommended_system_crew().map(str::to_string))
    }
}

/// Write a fresh `config.toml` at `config_path`, returning whether one was
/// created. An existing file is never overwritten, so `orbit init` stays
/// idempotent.
///
/// `seed` of `None` renders the static template alone: no `[crews]` table and
/// no `[workflow]` crew keys, so config loading falls back to the built-in
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
    let mut body = template.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let Some(seed) = seed else {
        return Ok(body);
    };

    // Agent detection is frozen at init; runtime config loading never probes
    // PATH or the environment.
    let crews = seed.seeded_crews();
    let workflow = render_workflow_crew_keys(seed, &crews)?;
    // L-0100: generated TOML keys must be inserted inside their intended table.
    let marker = "[workflow]\n";
    let insertion = body.find(marker).ok_or_else(|| {
        OrbitError::InvalidInput("default config template is missing [workflow]".to_string())
    })? + marker.len();
    body.insert_str(insertion, &workflow);
    body.push('\n');
    body.push_str(&render_crews(&crews));
    Ok(body)
}

/// The `[workflow]` keys a seed owns: the two lane crews, each naming a crew
/// the same file defines, and the four complexity pools scaffolded empty so an
/// operator finds them without reading the docs.
fn render_workflow_crew_keys(
    seed: &ConfigSeed,
    crews: &BTreeMap<String, Crew>,
) -> Result<String, OrbitError> {
    let mut rendered = String::new();
    let lanes = [
        ("default_crew", seed.effective_default_crew()),
        ("system_crew", seed.effective_system_crew()),
    ];
    if lanes.iter().any(|(_, crew)| crew.is_some()) {
        rendered.push_str(
            "# `default_crew` runs every task that declares no crew of its own.\n\
             # `system_crew` runs bounded system work (step-failure recovery, the task\n\
             # pilot); shipped job steps that name `crew: system` resolve onto it unless\n\
             # this file defines a crew table literally named `system`. Both name a\n\
             # crew from the `[crews.<name>]` tables below.\n",
        );
    }
    for (key, crew) in lanes {
        let Some(name) = crew else {
            continue;
        };
        if !crews.contains_key(&name) {
            return Err(OrbitError::InvalidInput(format!(
                "workflow.{key} names crew `{name}`, which this host does not seed"
            )));
        }
        rendered.push_str(&format!("{key} = {}\n", toml::Value::String(name)));
    }
    rendered.push_str(
        "# Automatic crew pools by task complexity. Entries are crew names, written\n\
         # `name` or `name:weight` (all bare or all weighted). A task created without\n\
         # a crew draws from the pool for its complexity; an empty pool routes that\n\
         # complexity to `default_crew`.\n\
         low_complexity_crews = []\n\
         medium_complexity_crews = []\n\
         hard_complexity_crews = []\n\
         xhard_complexity_crews = []\n",
    );
    Ok(rendered)
}

fn render_crews(crews: &BTreeMap<String, Crew>) -> String {
    let mut rendered = String::new();
    for (name, crew) in crews {
        rendered.push_str(&render_crew_table(name, crew));
    }
    if rendered.is_empty() {
        // Preserve an explicitly empty registry so runtime loading does not
        // substitute built-in crews for a host where init detected none.
        rendered.push_str("[crews]\n");
    }
    rendered
}

fn render_crew_table(name: &str, crew: &Crew) -> String {
    let mut rendered = format!("[crews.{name}]\n");
    for (field, value) in [
        ("model", &crew.assignment.model),
        ("provider", &crew.assignment.provider),
    ] {
        rendered.push_str(&format!(
            "{field} = {}\n",
            toml::Value::String(value.clone())
        ));
    }
    if let Some(description) = crew
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
    if !crew.tags.is_empty() {
        let tags = crew
            .tags
            .iter()
            .map(|tag| toml::Value::String(tag.clone()))
            .collect::<Vec<_>>();
        rendered.push_str(&format!("tags = {}\n", toml::Value::Array(tags)));
    }
    rendered.push('\n');
    rendered
}
