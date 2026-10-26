//! The typed documents an experiment definition is made of: arms and their
//! overlays, the population filter and the guardrails. Each is parsed with
//! unknown fields refused, normalized, and validated on write; an overlay is
//! stored as its canonical JSON beside that JSON's digest.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Metric names an experiment may read its verdict or guardrails on.
pub(crate) const EXPERIMENT_METRICS: &[&str] = &[
    "implement_tokens",
    "total_tokens",
    "cost_usd",
    "lead_time",
    "implement_duration",
    "turns_before_first_edit",
    "result_bytes_before_first_edit",
    "first_pass_acceptance",
    "review_repairs",
    "step_recoveries",
    "final_recoveries",
    "failed_checks",
    "merge_conflicts",
    "delivery_done_rate",
    "escaped_regressions",
    "lines_changed",
];

const COMPLEXITIES: &[&str] = &["low", "medium", "hard", "xhard"];

/// The activity role an overlay entry applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayRole {
    Implement,
    Review,
    Recovery,
}

fn implement_only() -> Vec<OverlayRole> {
    vec![OverlayRole::Implement]
}

/// One role's instruction overlay: a repository file, or `false` to
/// suppress the shipped default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InstructionOverlay {
    File(String),
    Suppress(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsOverlay {
    /// Skill name to the repository bundle directory that replaces it.
    pub replace: BTreeMap<String, String>,
    #[serde(default = "implement_only")]
    pub roles: Vec<OverlayRole>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsOverlay {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default = "implement_only")]
    pub roles: Vec<OverlayRole>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrewWeight {
    pub crew: String,
    #[serde(default = "one")]
    pub weight: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrewOverlay {
    pub pool: Vec<CrewWeight>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigOverlay {
    pub values: BTreeMap<String, Value>,
    #[serde(default = "implement_only")]
    pub roles: Vec<OverlayRole>,
}

/// What an arm changes, per surface. The control's is usually empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentOverlay {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instructions: BTreeMap<OverlayRole, InstructionOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillsOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<CrewOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigOverlay>,
}

impl ExperimentOverlay {
    /// Parse a stored or submitted overlay, refusing unknown fields, and
    /// normalize it.
    pub(crate) fn parse(arm: &str, value: Value) -> Result<Self, OrbitError> {
        let mut overlay: Self = serde_json::from_value(value).map_err(|error| {
            OrbitError::InvalidInput(format!("arm `{arm}` has an invalid overlay: {error}"))
        })?;
        overlay.normalize(arm)?;
        Ok(overlay)
    }

    fn normalize(&mut self, arm: &str) -> Result<(), OrbitError> {
        let invalid = |detail: String| {
            OrbitError::InvalidInput(format!("arm `{arm}` has an invalid overlay: {detail}"))
        };
        for (role, instruction) in &self.instructions {
            match instruction {
                InstructionOverlay::File(path) => repository_path(path).map_err(|detail| {
                    invalid(format!("instructions.{}: {detail}", role_name(*role)))
                })?,
                InstructionOverlay::Suppress(false) => {}
                InstructionOverlay::Suppress(true) => {
                    return Err(invalid(format!(
                        "instructions.{} must be a repository file path, or false to suppress the shipped default",
                        role_name(*role)
                    )));
                }
            }
        }
        if let Some(skills) = &mut self.skills {
            if skills.replace.is_empty() {
                return Err(invalid("skills.replace names no skill".into()));
            }
            for (skill, bundle) in &skills.replace {
                if skill.trim().is_empty() {
                    return Err(invalid("skills.replace has a blank skill name".into()));
                }
                repository_path(bundle)
                    .map_err(|detail| invalid(format!("skills.replace.{skill}: {detail}")))?;
            }
            normalize_roles(&mut skills.roles)
                .map_err(|detail| invalid(format!("skills.{detail}")))?;
        }
        if let Some(tools) = &mut self.tools {
            normalize_names(&mut tools.add)
                .map_err(|detail| invalid(format!("tools.add {detail}")))?;
            normalize_names(&mut tools.remove)
                .map_err(|detail| invalid(format!("tools.remove {detail}")))?;
            if tools.add.is_empty() && tools.remove.is_empty() {
                return Err(invalid("tools adds and removes no tool".into()));
            }
            if let Some(both) = tools.add.iter().find(|tool| tools.remove.contains(tool)) {
                return Err(invalid(format!("tools both adds and removes `{both}`")));
            }
            normalize_roles(&mut tools.roles)
                .map_err(|detail| invalid(format!("tools.{detail}")))?;
        }
        if let Some(crew) = &mut self.crew {
            if crew.pool.is_empty() {
                return Err(invalid("crew.pool names no crew".into()));
            }
            let mut seen = BTreeSet::new();
            for member in &mut crew.pool {
                member.crew = member.crew.trim().to_string();
                if member.crew.is_empty() {
                    return Err(invalid("crew.pool has a blank crew".into()));
                }
                if member.weight == 0 {
                    return Err(invalid(format!(
                        "crew.pool weight for `{}` must be greater than 0",
                        member.crew
                    )));
                }
                if !seen.insert(member.crew.clone()) {
                    return Err(invalid(format!("crew.pool names `{}` twice", member.crew)));
                }
            }
        }
        if let Some(config) = &mut self.config {
            if config.values.is_empty() {
                return Err(invalid("config.values sets no key".into()));
            }
            if config.values.keys().any(|key| key.trim().is_empty()) {
                return Err(invalid("config.values has a blank key".into()));
            }
            normalize_roles(&mut config.roles)
                .map_err(|detail| invalid(format!("config.{detail}")))?;
        }
        Ok(())
    }

    /// The canonical JSON stored for this overlay.
    pub(crate) fn canonical_json(&self) -> Result<String, OrbitError> {
        serde_json::to_string(self).map_err(|error| {
            OrbitError::Execution(format!("serialize experiment overlay: {error}"))
        })
    }

    /// The config keys this overlay sets.
    pub(crate) fn config_keys(&self) -> impl Iterator<Item = &str> {
        self.config
            .iter()
            .flat_map(|config| config.values.keys().map(String::as_str))
    }
}

/// `sha256:<hex>` of a stored overlay's canonical JSON.
pub(crate) fn overlay_digest(canonical_json: &str) -> String {
    let hex: String = Sha256::digest(canonical_json.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn role_name(role: OverlayRole) -> &'static str {
    match role {
        OverlayRole::Implement => "implement",
        OverlayRole::Review => "review",
        OverlayRole::Recovery => "recovery",
    }
}

fn normalize_roles(roles: &mut Vec<OverlayRole>) -> Result<(), String> {
    roles.sort_unstable();
    roles.dedup();
    if roles.is_empty() {
        return Err("roles names no role".into());
    }
    Ok(())
}

fn normalize_names(names: &mut Vec<String>) -> Result<(), String> {
    for name in names.iter_mut() {
        *name = name.trim().to_string();
        if name.is_empty() {
            return Err("has a blank name".into());
        }
    }
    names.sort_unstable();
    names.dedup();
    Ok(())
}

/// A relative repository path that stays inside the repository.
fn repository_path(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("is a blank path".into());
    }
    if trimmed != path {
        return Err(format!("`{path}` has surrounding whitespace"));
    }
    let inside = Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(format!(
            "`{path}` must be a path relative to the repository root, without `..`"
        ));
    }
    Ok(())
}

/// One arm as submitted on create or update.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmInput {
    name: String,
    #[serde(default)]
    control: bool,
    #[serde(default = "one")]
    weight: u32,
    #[serde(default)]
    overlay: Option<Value>,
}

/// One validated arm, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ArmDefinition {
    pub(crate) name: String,
    pub(crate) control: bool,
    pub(crate) weight: u32,
    pub(crate) overlay_json: String,
    pub(crate) overlay_digest: String,
}

/// Parse and validate the submitted arm list. A draft may be incomplete
/// (fewer than two arms, no control yet); start checks completeness.
pub(crate) fn parse_arms(value: Value) -> Result<Vec<ArmDefinition>, OrbitError> {
    let inputs: Vec<ArmInput> = serde_json::from_value(value).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "`arms` must be a JSON array of {{name, control, weight, overlay}}: {error}"
        ))
    })?;
    let mut names = BTreeSet::new();
    let mut controls = 0;
    let mut arms = Vec::with_capacity(inputs.len());
    for input in inputs {
        let name = input.name.trim().to_string();
        validate_handle("arm name", &name)?;
        if !names.insert(name.clone()) {
            return Err(OrbitError::InvalidInput(format!(
                "arm name `{name}` is used twice"
            )));
        }
        if input.weight == 0 {
            return Err(OrbitError::InvalidInput(format!(
                "arm `{name}` weight must be greater than 0"
            )));
        }
        if input.control {
            controls += 1;
        }
        let overlay = ExperimentOverlay::parse(
            &name,
            input
                .overlay
                .unwrap_or_else(|| Value::Object(Default::default())),
        )?;
        let overlay_json = overlay.canonical_json()?;
        arms.push(ArmDefinition {
            overlay_digest: overlay_digest(&overlay_json),
            overlay_json,
            name,
            control: input.control,
            weight: input.weight,
        });
    }
    if controls > 1 {
        return Err(OrbitError::InvalidInput(
            "exactly one arm may be the control; more than one sets `control: true`".into(),
        ));
    }
    Ok(arms)
}

/// Which tasks an experiment enrolls. Empty lists place no restriction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PopulationFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub complexity: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_type: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_exclude: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin: Vec<String>,
}

/// Parse, normalize and validate a population filter into its stored JSON.
pub(crate) fn parse_population(value: Value) -> Result<String, OrbitError> {
    let mut population: PopulationFilter = serde_json::from_value(value)
        .map_err(|error| OrbitError::InvalidInput(format!("`population` is invalid: {error}")))?;
    for (field, values) in [
        ("complexity", &mut population.complexity),
        ("task_type", &mut population.task_type),
        ("tags_include", &mut population.tags_include),
        ("tags_exclude", &mut population.tags_exclude),
        ("origin", &mut population.origin),
    ] {
        normalize_names(values)
            .map_err(|detail| OrbitError::InvalidInput(format!("`population.{field}` {detail}")))?;
    }
    if let Some(unknown) = population
        .complexity
        .iter()
        .find(|complexity| !COMPLEXITIES.contains(&complexity.as_str()))
    {
        return Err(OrbitError::InvalidInput(format!(
            "`population.complexity` value `{unknown}` is not one of {}",
            COMPLEXITIES.join(", ")
        )));
    }
    if let Some(both) = population
        .tags_include
        .iter()
        .find(|tag| population.tags_exclude.contains(tag))
    {
        return Err(OrbitError::InvalidInput(format!(
            "`population` both includes and excludes tag `{both}`"
        )));
    }
    serde_json::to_string(&population)
        .map_err(|error| OrbitError::Execution(format!("serialize population: {error}")))
}

/// Parse and validate guardrails, metric name to threshold, into their
/// stored JSON.
pub(crate) fn parse_guardrails(value: Value) -> Result<String, OrbitError> {
    let guardrails: BTreeMap<String, String> = serde_json::from_value(value).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "`guardrails` must map metric names to threshold strings: {error}"
        ))
    })?;
    for (metric, threshold) in &guardrails {
        validate_metric("guardrails", metric)?;
        if !valid_threshold(threshold) {
            return Err(OrbitError::InvalidInput(format!(
                "guardrail threshold `{threshold}` for `{metric}` must be a signed number with an optional `pt` or `%` unit, such as `-5pt` or `+0`"
            )));
        }
    }
    serde_json::to_string(&guardrails)
        .map_err(|error| OrbitError::Execution(format!("serialize guardrails: {error}")))
}

fn valid_threshold(threshold: &str) -> bool {
    let Some(rest) = threshold
        .strip_prefix('+')
        .or_else(|| threshold.strip_prefix('-'))
    else {
        return false;
    };
    let number = rest
        .strip_suffix("pt")
        .or_else(|| rest.strip_suffix('%'))
        .unwrap_or(rest);
    !number.is_empty() && number.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Refuse a metric name Orbit does not measure.
pub(crate) fn validate_metric(field: &str, metric: &str) -> Result<(), OrbitError> {
    if EXPERIMENT_METRICS.contains(&metric) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`{field}` names unknown metric `{metric}`; expected one of {}",
        EXPERIMENT_METRICS.join(", ")
    )))
}

/// An experiment or arm name: a short handle a person types.
pub(crate) fn validate_handle(what: &str, name: &str) -> Result<(), OrbitError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "{what} `{name}` must be 1-64 characters of letters, digits, `-`, `_` or `.`, starting with a letter or digit"
    )))
}
