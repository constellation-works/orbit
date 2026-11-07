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
use orbit_types::identity::{Crew, crew_reference_base, resolve_crew};
use orbit_types::workflow::ShipMode;

use crate::crew_defaults::builtin_default_crews;
use crate::layering::WORKSPACE_REPLACE_ONLY_KEYS;
use crate::plugin_enablement::workspace_config_sets_policy;
use crate::{ConfigRoots, ConfigScope, ConfigStore, ResolvedConfig};

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
            &["terra", "sonnet"],
            &["sol", "opus"],
            &["opus", "astra"],
        ],
    ),
    (true, false, [&["luna"], &["terra"], &["sol"], &["astra"]]),
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
        builtin_default_crews()
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

/// Machine-scope settings a newly created machine identity starts with, as
/// `(key, TOML value, comment)`. They are written beside the identity, in the
/// global `config.toml` that alone may hold `[machine]`, never into the
/// config template: a template seeded under an explicit `--root` can later
/// load as a workspace config, which refuses `[machine]`.
const NEW_MACHINE_DEFAULTS: &[(&str, &str, &str)] = &[(
    "machine.auto_concurrency.mode",
    "\"enforce\"",
    "Admit delivery work only while its forecast fits this host.",
)];

/// Seed the machine-scope defaults a new machine starts with
/// (`machine.auto_concurrency.mode = "enforce"`) that `store`, the global
/// config of a machine whose identity is being created, does not already set.
/// The caller saves.
pub fn seed_new_machine_defaults(store: &mut ConfigStore) -> Result<bool, OrbitError> {
    if store.scope() != ConfigScope::Global {
        return Err(OrbitError::InvalidInput(
            "machine defaults are written only to the global config.toml".to_string(),
        ));
    }
    let mut changed = false;
    for (key, value, comment) in NEW_MACHINE_DEFAULTS {
        changed |= store.seed_missing_value(key, value, comment)?;
    }
    Ok(changed)
}

/// Seed missing workspace review choices for its registered delivery route.
///
/// Uses enabled crews from the effective config after global bootstrap, never
/// provider detection alone. Existing keys and comments survive, including
/// deprecated review switches; repeated initialization writes nothing. The
/// global config is never changed, even when both roots name the same file.
pub fn seed_workspace_review(roots: &ConfigRoots, ship_mode: ShipMode) -> Result<bool, OrbitError> {
    if roots.global() == roots.workspace() {
        return Ok(false);
    }
    let path = roots.workspace().join("config.toml");
    ConfigStore::with_write_lock(&path, || {
        let mut store = ConfigStore::open(ConfigScope::Workspace, &path)?;
        let resolved = ResolvedConfig::load(roots)?;
        let cheap: Vec<&str> = ["luna", "haiku", "muse-spark"]
            .into_iter()
            .filter(|name| resolved.crews.get(*name).is_some_and(|crew| crew.enabled))
            .collect();
        let (timing, reason) = match ship_mode {
            ShipMode::Local => ("off", "Before-landing review needs the PR route."),
            ShipMode::Pr if cheap.is_empty() => (
                "off",
                "No cheap reviewer enabled (luna, haiku or muse-spark).",
            ),
            ShipMode::Pr => (
                "before_landing",
                "Review the PR before landing with enabled cheap crews.",
            ),
        };

        // Creating the first policy layer must not silently change the sandbox,
        // approval policy or environment allowlist that this workspace used.
        let mut changed = false;
        if !workspace_config_sets_policy(&path) {
            for key in WORKSPACE_REPLACE_ONLY_KEYS {
                if let Some(value) = resolved
                    .snapshot
                    .value_for(key)
                    .filter(|value| !value.is_null())
                {
                    let value = toml::Value::try_from(value)
                        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
                    changed |= store.seed_missing_value(
                        key,
                        &value.to_string(),
                        "Keep the security policy in effect before workspace init.",
                    )?;
                }
            }
        }
        if ![
            "review.before_pr",
            "review.before_landing",
            "operation.review_policy",
        ]
        .iter()
        .any(|key| store.is_key_set(key))
        {
            changed |= store.seed_missing_value(
                "review.timing",
                &toml::Value::String(timing.to_string()).to_string(),
                reason,
            )?;
        }
        if !store.is_key_set("operation.review_minutes") {
            changed |= store.seed_missing_value(
                "review.minutes",
                "30",
                "Bound review to the built-in 30-minute budget.",
            )?;
        }
        if ship_mode == ShipMode::Pr {
            changed |= store.seed_missing_value(
                "workflow.purpose_crews.pre_landing_review",
                &toml_array(&cheap).to_string(),
                "Only enabled luna, haiku and muse-spark crews; no costlier fallback is seeded.",
            )?;
        }
        if changed {
            store.validate_workspace_with_global(roots.global())?;
            store.save()?;
        }
        Ok(changed)
    })
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

/// Seeded `workflow.purpose_crews` pools, in order. Each is filtered to the
/// crews this host seeds enabled (a `crew@effort` member by its base crew),
/// and a pool left empty is omitted, so it takes its built-in route. The one
/// exception is `final_recovery`, whose built-in default names crews this host
/// may not enable: an empty one is written as `[]`, which turns it off.
const PURPOSE_POOLS: &[(&str, &[&str])] = &[
    (
        "review",
        &["sol", "sonnet", "muse-wonder", "grok", "opus@medium"],
    ),
    (
        "task_pilot",
        &["luna@high", "haiku@high", "muse-spark@high"],
    ),
    ("failure_recovery", &["luna", "haiku", "muse-spark", "grok"]),
    ("auto_fix", &["luna", "haiku", "muse-spark"]),
    ("final_recovery", &["sol", "muse-wonder", "opus@medium"]),
    (
        "baseline_red_repair",
        &["terra", "sonnet", "muse-wonder", "grok"],
    ),
    ("pre_landing_review", &["luna", "haiku", "muse-spark"]),
];

/// The `[workflow]` keys a seed owns, written explicitly so no crew route is
/// an invisible default: the default crew, the four complexity pools seeded
/// from the detected families, and the [`PURPOSE_POOLS`] this host can run.
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
    rendered.push_str(&render_purpose_crews(crews)?);
    Ok(rendered)
}

/// The [`PURPOSE_POOLS`] filtered to the crews this host seeds enabled.
fn render_purpose_crews(crews: &BTreeMap<String, Crew>) -> Result<String, OrbitError> {
    let mut rendered = String::from(
        "# Crew pools by purpose of work, written like the pools above; `crew@effort`\n\
         # runs a crew at that reasoning effort. An omitted purpose takes its built-in route.\n",
    );
    for (key, members) in PURPOSE_POOLS {
        let mut pool = Vec::new();
        for member in *members {
            // A member this host's crews cannot run (an effort its model
            // refuses) is a template defect, not a host to seed around.
            resolve_crew(member, crews).map_err(|error| {
                OrbitError::InvalidInput(format!("workflow.purpose_crews.{key}: {error}"))
            })?;
            if crews
                .get(crew_reference_base(member))
                .is_some_and(|base| base.enabled)
            {
                pool.push(*member);
            }
        }
        if pool.is_empty() && *key != "final_recovery" {
            continue;
        }
        let note = if pool.is_empty() {
            "  # [] turns final recovery off"
        } else {
            ""
        };
        rendered.push_str(&format!(
            "purpose_crews.{key} = {}{note}\n",
            toml_array(&pool)
        ));
    }
    Ok(rendered)
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
