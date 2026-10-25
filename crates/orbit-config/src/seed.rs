//! Rendering and writing a fresh default `config.toml`.
//!
//! Seeding freezes agent-dependent choices at `orbit init` time so runtime
//! config loading never probes `PATH` or the environment. The probing itself
//! is not done here: the CLI init adapter detects installed provider CLIs,
//! runs any interactive prompts, and hands the answers over as a
//! [`ConfigSeed`].
//!
//! A seeded file writes every built-in crew. Crews of a detected family get
//! `enabled = true`; the rest get `enabled = false`, so turning a provider on
//! later is a one-line edit rather than a re-seed. `workflow.default_crew`
//! points at an enabled built-in crew, and the complexity pools name enabled
//! built-in crews; init never invents a `custom` crew table whose only
//! purpose is to be pointed at.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::write_text_with_parent;
use orbit_types::identity::Crew;

use crate::resolved::default_crews;

pub(crate) const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../assets/default-config.toml");

/// Crew families Orbit ships crews for, in the order a seeded config prefers
/// them. Families with no built-in crew (`ollama`, the legacy `gemini` CLI)
/// are not seeded; an operator defines their crews by hand.
/// [ORB-10946] [ORB-11299] [ORB-16577]
const CREW_FAMILY_PREFERENCE: &[&str] =
    &["claude", "codex", "antigravity", "grok", "copilot", "muse"];

/// The crew each family's default lane runs on, in [`CREW_FAMILY_PREFERENCE`]
/// order. The first available family's entry is the seeded
/// `workflow.default_crew`.
const DEFAULT_CREW_BY_FAMILY: &[(&str, &str)] = &[
    ("claude", "opus"),
    ("codex", "astra"),
    ("antigravity", "gemini-flash"),
    ("grok", "grok"),
    ("copilot", "copilot"),
    ("muse", "muse-spark"),
];

/// Seeded `workflow.*_complexity_crews` pools, keyed by detected codex/claude
/// presence: `(codex, claude, [low, medium, hard, xhard])`. A host with
/// neither starts from empty pools; grok and the Google CLIs are layered on top
/// by [`ConfigSeed::complexity_pools`].
const BASE_POOLS: &[(bool, bool, [&[&str]; 4])] = &[
    (
        true,
        true,
        [
            &["haiku", "luna"],
            &["sol", "sonnet"],
            &["opus"],
            &["opus", "astra"],
        ],
    ),
    (true, false, [&["luna"], &["sol"], &["sol"], &["astra"]]),
    (false, true, [&["haiku"], &["sonnet"], &["opus"], &["opus"]]),
];

/// The four `workflow.*_complexity_crews` keys, in pool order.
const POOL_KEYS: [&str; 4] = [
    "low_complexity_crews",
    "medium_complexity_crews",
    "hard_complexity_crews",
    "xhard_complexity_crews",
];

/// Explicit, host-independent inputs for rendering a fresh `config.toml`.
///
/// A seed says which provider families this machine can actually dispatch to
/// and, optionally, which of the resulting crews an operator chose as the
/// default crew. Everything else — the crew table layout, the recommended
/// default crew, the complexity pools — is config policy and stays in this
/// crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSeed {
    /// Provider families available on this host. An empty set seeds every
    /// crew disabled and no lane keys, which is how a host that can dispatch
    /// nothing avoids silently running built-in crews: dispatch refuses the
    /// disabled default until an operator enables a crew.
    pub families: BTreeSet<String>,
    /// Operator-chosen `workflow.default_crew`, by seeded crew name. `None`
    /// takes [`Self::recommended_default_crew`].
    pub default_crew: Option<String>,
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
        }
    }

    /// Record the crew an operator chose as `workflow.default_crew`.
    pub fn with_default_crew(mut self, name: impl Into<String>) -> Self {
        self.default_crew = Some(name.into());
        self
    }

    /// The built-in crews this seed writes, keyed by crew name: every one of
    /// them, enabled exactly when its provider family is available.
    pub fn seeded_crews(&self) -> BTreeMap<String, Crew> {
        let available_families = self.available_families();
        default_crews()
            .into_iter()
            .map(|(name, mut crew)| {
                crew.enabled = available_families.contains(&crew.assignment.provider.as_str());
                (name, crew)
            })
            .collect()
    }

    /// The seeded crews an operator may choose for a workflow lane: only
    /// those enabled on this host.
    pub fn enabled_crews(&self) -> BTreeMap<String, Crew> {
        self.seeded_crews()
            .into_iter()
            .filter(|(_, crew)| crew.enabled)
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

    /// The seeded complexity pools `[low, medium, hard, xhard]`, derived from
    /// the detected families. Codex and claude pick a base row; grok is
    /// appended to medium; `agy` appends `gemini-flash` to low and `argon` to
    /// hard; `muse` appends `muse-spark`, `muse-wonder` and `muse-wonder-full`
    /// to low, medium and hard. Copilot has no pool rule, and an empty pool
    /// routes to `default_crew`.
    pub fn complexity_pools(&self) -> [Vec<&'static str>; 4] {
        let (codex, claude) = (self.has_family("codex"), self.has_family("claude"));
        let mut pools: [Vec<&'static str>; 4] = BASE_POOLS
            .iter()
            .find(|(has_codex, has_claude, _)| *has_codex == codex && *has_claude == claude)
            .map(|(_, _, rows)| rows.map(|row| row.to_vec()))
            .unwrap_or_default();
        if self.has_family("grok") {
            pools[1].push("grok");
        }
        if self.has_family("antigravity") {
            pools[0].push("gemini-flash");
            pools[2].push("argon");
        }
        if self.has_family("muse") {
            pools[0].push("muse-spark");
            pools[1].push("muse-wonder");
            pools[2].push("muse-wonder-full");
        }
        pools
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

/// Crews final recovery is seeded with, in order, when this host seeds them
/// enabled. Without any of them it falls back to [`FINAL_RECOVERY_FALLBACK`].
const FINAL_RECOVERY_PREFERENCE: &[&str] = &["sol", "grok", "muse-wonder"];

/// Final recovery's seeded crew on a host with none of
/// [`FINAL_RECOVERY_PREFERENCE`].
const FINAL_RECOVERY_FALLBACK: &str = "opus";

/// The `[workflow]` keys a seed owns, written explicitly so no crew route is
/// an invisible default: the default crew, the four complexity pools seeded
/// from the detected families, and every `purpose_crews` pool. A purpose whose
/// route no fixed list can express (the task's own complexity pool, the
/// review pool) is written as `[]` with a comment naming that route.
pub(crate) fn render_workflow_crew_keys(
    seed: &ConfigSeed,
    crews: &BTreeMap<String, Crew>,
) -> Result<String, OrbitError> {
    let mut rendered = String::new();
    if let Some(name) = seed.effective_default_crew() {
        if !crews.get(&name).is_some_and(|crew| crew.enabled) {
            return Err(OrbitError::InvalidInput(format!(
                "workflow.default_crew names crew `{name}`, which this host does not seed enabled"
            )));
        }
        rendered
            .push_str("# Runs a task with no crew of its own whose complexity pool is empty.\n");
        rendered.push_str(&format!("default_crew = {}\n", toml::Value::String(name)));
    }
    rendered.push_str(
        "# Crew pools by task complexity: `name` or `name:weight` entries, all bare or\n\
         # all weighted.\n",
    );
    let pools = seed.complexity_pools();
    for (key, pool) in POOL_KEYS.into_iter().zip(&pools) {
        if let Some(name) = pool
            .iter()
            .find(|name| !crews.get(**name).is_some_and(|crew| crew.enabled))
        {
            return Err(OrbitError::InvalidInput(format!(
                "workflow.{key} names crew `{name}`, which this host does not seed enabled"
            )));
        }
        rendered.push_str(&format!("{key} = {}\n", toml_array(pool)));
    }
    rendered.push_str(&render_purpose_crews(&pools, crews));
    Ok(rendered)
}

/// Every `workflow.purpose_crews` key, each with the crews its route resolves
/// to on this host.
fn render_purpose_crews(pools: &[Vec<&'static str>; 4], crews: &BTreeMap<String, Crew>) -> String {
    let [low, medium, ..] = pools;
    let enabled = |name: &&str| crews.get(*name).is_some_and(|crew| crew.enabled);
    let mut final_recovery: Vec<&str> = FINAL_RECOVERY_PREFERENCE
        .iter()
        .copied()
        .filter(enabled)
        .collect();
    if final_recovery.is_empty() && enabled(&FINAL_RECOVERY_FALLBACK) {
        final_recovery.push(FINAL_RECOVERY_FALLBACK);
    }
    let mut rendered =
        String::from("# Crew pools by purpose of work, written like the pools above.\n");
    let keys: [(&str, &[&str], &str); 9] = [
        ("task_pilot", low, ""),
        ("auto_fix", low, ""),
        ("failure_recovery", medium, ""),
        ("review", medium, ""),
        ("pre_landing_review", &[], "  # [] uses the review pool"),
        (
            "final_recovery",
            &final_recovery,
            "  # [] turns final recovery off",
        ),
        (
            "baseline_red_repair",
            &[],
            "  # [] uses the task's complexity pool",
        ),
        ("sweep_tasks", &[], "  # [] uses the task's complexity pool"),
        ("explorer", &[], "  # [] offers no read-only explorers"),
    ];
    for (key, pool, note) in keys {
        rendered.push_str(&format!(
            "purpose_crews.{key} = {}{note}\n",
            toml_array(pool)
        ));
    }
    rendered
}

fn toml_array(names: &[&str]) -> toml::Value {
    toml::Value::Array(
        names
            .iter()
            .map(|name| toml::Value::String((*name).to_string()))
            .collect(),
    )
}

fn render_crews(crews: &BTreeMap<String, Crew>) -> String {
    let mut rendered = String::from(
        "# Every built-in crew is listed. `enabled = false` marks a crew whose provider\n\
         # CLI was not detected at `orbit init`; dispatch refuses it. Enable one with\n\
         # `orbit config set crews.<name>.enabled true`.\n",
    );
    for (name, crew) in crews {
        rendered.push_str(&render_crew_table(name, crew));
    }
    rendered
}

fn render_crew_table(name: &str, crew: &Crew) -> String {
    let mut rendered = format!("[crews.{name}]\nenabled = {}\n", crew.enabled);
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
