//! Crew admission, optional-property diagnostics and workflow lane aliases.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::model_defaults::{
    ANTIGRAVITY_DEFAULT_MODEL, CLAUDE_DEFAULT_STRONG, CLAUDE_DEFAULT_WEAK, CLAUDE_FABLE_MODEL,
    CLAUDE_HAIKU_MODEL, CODEX_ASTRA_MODEL, CODEX_LUNA_MODEL, CODEX_SOL_MODEL, CODEX_TERRA_MODEL,
    COPILOT_DEFAULT_MODEL, CURSOR_DEFAULT_MODEL, GEMINI_CREW_MODEL, GROK_DEFAULT_MODEL,
    OPENCODE_DEFAULT_MODEL, PI_DEFAULT_MODEL,
};
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{Crew, CrewAssignment, ReasoningEffort, validate_antigravity_model};
use orbit_types::workflow::activity_job::Provider;

use super::compatibility::reject_retired_crew_backend;
use super::config::ResolvedConfig;
use crate::crew_pools::reject_unpoolable_crew_name_in_config;
use crate::raw::RawCrewEntry;
use crate::registry::{DEFAULT_WORKFLOW_SYSTEM_CREW, LEGACY_WORKFLOW_SYSTEM_CREW};

/// An optional per-crew setting that was present in `config.toml` but dropped
/// at admission because it had no safe typed value.
///
/// Required crew fields still fail closed. Optional tunables such as `effort`
/// are ignored so one mistyped key cannot take the workspace down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredCrewProperty {
    /// Home-redacted path of the config file that supplied the value.
    pub config: String,
    /// Crew table name (`[crews.<name>]`).
    pub crew: String,
    /// Property that was ignored (`effort`).
    pub property: String,
    /// Offending raw value.
    pub value: String,
    /// Accepted values, or guidance to omit the key.
    pub accepted: String,
    /// Former hard-error text, reused when `orbit config set` refuses to
    /// persist a value that admission would ignore.
    pub error_message: String,
}

impl IgnoredCrewProperty {
    /// Dotted `crews.<name>.<field>` key this warning belongs to.
    pub fn config_key(&self) -> String {
        format!("crews.{}.{}", self.crew, self.property)
    }

    /// Operator-facing warning line for doctor / config surfaces.
    pub fn warning_message(&self) -> String {
        format!(
            "ignoring [crews.{}].{} = '{}' in {}; accepted: {}",
            self.crew, self.property, self.value, self.config, self.accepted
        )
    }

    /// Corrective edit naming the file, key, and accepted values.
    pub fn remediation(&self) -> String {
        format!(
            "Edit {}: set [crews.{}].{} to one of {}, or remove the key.",
            self.config, self.crew, self.property, self.accepted
        )
    }
}

/// A workflow lane key (`workflow.default_crew` / `workflow.system_crew`)
/// that names a disabled crew. Loading still succeeds — unrelated commands
/// keep working — but dispatch on that lane refuses, so `orbit doctor`
/// reports it ahead of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledLaneCrew {
    /// The `workflow.*` key that selects the crew.
    pub key: &'static str,
    /// Crew name the key resolves to.
    pub crew: String,
    /// Crew table whose `enabled` flag turns the lane back on. Differs from
    /// `crew` only when `system` mirrors another crew.
    pub enable_target: String,
}

impl DisabledLaneCrew {
    /// Operator-facing warning line for doctor.
    pub fn warning_message(&self) -> String {
        format!(
            "{} names {}; dispatch on that lane is refused",
            self.key,
            disabled_crew_subject(&self.crew, &self.enable_target)
        )
    }

    /// Corrective command.
    pub fn remediation(&self) -> String {
        format!(
            "orbit config set crews.{}.enabled true (or point {} at an enabled crew)",
            self.enable_target, self.key
        )
    }
}

/// Refusal text for dispatching a disabled crew: names the crew, the crew
/// table that disables it, and the one-line edit that enables it. `mirrors` is
/// the crew the synthesized `system` entry copies, when it is one.
pub fn disabled_crew_message(crew: &str, mirrors: Option<&str>) -> String {
    let target = mirrors.unwrap_or(crew);
    format!(
        "{}; enable it with `orbit config set crews.{target}.enabled true` (or set \
         `enabled = true` in [crews.{target}]), or select an enabled crew",
        disabled_crew_subject(crew, target)
    )
}

fn disabled_crew_subject(crew: &str, target: &str) -> String {
    if crew == target {
        format!("crew `{crew}`, which is disabled ([crews.{crew}] enabled = false)")
    } else {
        format!(
            "crew `{crew}`, which mirrors crew `{target}`; `{target}` is disabled \
             ([crews.{target}] enabled = false)"
        )
    }
}

impl ResolvedConfig {
    /// Lane keys whose crew is disabled. Only a crew this registry defines can
    /// be reported; an unknown name already fails elsewhere.
    pub fn disabled_lane_crews(&self) -> Vec<DisabledLaneCrew> {
        [
            ("workflow.default_crew", self.default_crew.as_deref()),
            ("workflow.system_crew", Some(self.system_crew.as_str())),
        ]
        .into_iter()
        .filter_map(|(key, name)| {
            let crew = self.crews.get(name?)?;
            if crew.enabled {
                return None;
            }
            let enable_target = if crew.name == DEFAULT_WORKFLOW_SYSTEM_CREW {
                self.system_crew_alias.clone()
            } else {
                None
            };
            Some(DisabledLaneCrew {
                key,
                crew: crew.name.clone(),
                enable_target: enable_target.unwrap_or_else(|| crew.name.clone()),
            })
        })
        .collect()
    }
}

pub(crate) fn default_crews() -> BTreeMap<String, Crew> {
    let mut crews = BTreeMap::new();
    for (name, model, provider) in [
        ("opus", CLAUDE_DEFAULT_STRONG, "claude"),
        ("sonnet", CLAUDE_DEFAULT_WEAK, "claude"),
        ("haiku", CLAUDE_HAIKU_MODEL, "claude"),
        ("fable", CLAUDE_FABLE_MODEL, "claude"),
        ("sol", CODEX_SOL_MODEL, "codex"),
        ("terra", CODEX_TERRA_MODEL, "codex"),
        ("luna", CODEX_LUNA_MODEL, "codex"),
        ("astra", CODEX_ASTRA_MODEL, "codex"),
        ("gemini", GEMINI_CREW_MODEL, "gemini"),
        ("antigravity", ANTIGRAVITY_DEFAULT_MODEL, "antigravity"),
        ("grok", GROK_DEFAULT_MODEL, "grok"),
        ("copilot", COPILOT_DEFAULT_MODEL, "copilot"),
        ("cursor", CURSOR_DEFAULT_MODEL, "cursor"),
        ("pi", PI_DEFAULT_MODEL, "pi"),
        ("opencode", OPENCODE_DEFAULT_MODEL, "opencode"),
        // [ORB-10877] Shipped job steps name `system` directly, so the
        // built-in set used by a config with no `[crews]` table must define it
        // or those pipelines fail validation. A seeded config omits this table
        // and names a real cheap-tier crew in `workflow.system_crew` instead;
        // the claude tier here matches the family the built-in `default_crew`
        // already assumes.
        (DEFAULT_WORKFLOW_SYSTEM_CREW, CLAUDE_DEFAULT_WEAK, "claude"),
    ] {
        crews.insert(
            name.to_string(),
            Crew {
                name: name.to_string(),
                assignment: crew_assignment(model, provider),
                description: None,
                tags: Vec::new(),
                enabled: true,
            },
        );
    }
    crews
}

fn crew_assignment(model: &str, provider: &str) -> CrewAssignment {
    CrewAssignment {
        model: model.to_string(),
        provider: provider.to_string(),
        effort: None,
    }
}
pub(super) fn crews_from_raw(
    raw: Option<&BTreeMap<String, RawCrewEntry>>,
    config_path: &Path,
) -> Result<(BTreeMap<String, Crew>, Vec<IgnoredCrewProperty>), OrbitError> {
    let Some(raw_crews) = raw else {
        return Ok((default_crews(), Vec::new()));
    };
    let mut crews = BTreeMap::new();
    let mut ignored = Vec::new();
    for (name, entry) in raw_crews {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(OrbitError::InvalidInput(
                "[crews] names must not be empty".to_string(),
            ));
        }
        reject_unpoolable_crew_name_in_config(trimmed, "[crews]", config_path)?;
        let crew = Crew {
            name: trimmed.to_string(),
            assignment: crew_assignment_from_raw(trimmed, entry, config_path, &mut ignored)?,
            description: normalized_crew_description(entry.description.as_deref()),
            tags: normalized_crew_tags(&entry.tags),
            enabled: entry.enabled.unwrap_or(true),
        };
        if crews.insert(trimmed.to_string(), crew).is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "[crews] contains duplicate name '{trimmed}' after whitespace normalization"
            )));
        }
    }
    Ok((crews, ignored))
}

/// [ORB-10877] Shipped job steps name the `system` crew directly so the
/// definition says which crew does the work. A seeded config has no
/// `[crews.system]` table — `orbit init` names a real crew in
/// `workflow.system_crew` instead — and neither does a config written before
/// that key existed, so resolve the name rather than failing those hosts at
/// dispatch.
///
/// `configured` is `workflow.system_crew`, which is how such a config already
/// says where system work belongs. A defined configured crew wins. For the two
/// names Orbit itself has used for this lane (`system` and legacy `qa`), fall
/// back to `qa` and then the already-validated default crew. That final fallback
/// keeps pre-system Gemini- and Grok-only configs portable: those versions never
/// seeded `qa`, but they did seed their family default. Unknown custom names do
/// not receive this compatibility fallback, so a typo still fails closed at
/// dispatch. An explicit `[crews.system]` always wins.
///
/// Returns the name of the mirrored crew. The alias copies that crew's
/// `enabled` flag, so a refusal can name the table that actually disables it.
pub(super) fn alias_system_crew(
    crews: &mut BTreeMap<String, Crew>,
    configured: &str,
    default_crew: Option<&str>,
) -> Option<String> {
    if crews.contains_key(DEFAULT_WORKFLOW_SYSTEM_CREW) {
        return None;
    }
    let source = crews.get(configured).cloned().or_else(|| {
        if !matches!(
            configured,
            DEFAULT_WORKFLOW_SYSTEM_CREW | LEGACY_WORKFLOW_SYSTEM_CREW
        ) {
            return None;
        }
        crews
            .get(LEGACY_WORKFLOW_SYSTEM_CREW)
            .or_else(|| default_crew.and_then(|name| crews.get(name)))
            .cloned()
    });
    let source = source?;
    let mirrored = source.name.clone();
    crews.insert(
        DEFAULT_WORKFLOW_SYSTEM_CREW.to_string(),
        Crew {
            name: DEFAULT_WORKFLOW_SYSTEM_CREW.to_string(),
            ..source
        },
    );
    Some(mirrored)
}

fn normalized_crew_description(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalized_crew_tags(raw: &[String]) -> Vec<String> {
    let mut tags = raw
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

fn crew_assignment_from_raw(
    crew: &str,
    raw: &RawCrewEntry,
    config_path: &Path,
    ignored: &mut Vec<IgnoredCrewProperty>,
) -> Result<CrewAssignment, OrbitError> {
    let has_legacy = raw.planner.is_some() || raw.implementer.is_some() || raw.reviewer.is_some();
    if has_legacy {
        return Err(OrbitError::InvalidInput(format!(
            "[crews.{crew}] uses retired planner/implementer/reviewer role tables; rewrite it with flat `model` and `provider` fields only"
        )));
    }
    reject_retired_crew_backend(crew, raw.backend.as_deref())?;
    let model = required_crew_field(crew, "model", raw.model.as_deref())?;
    let provider = required_crew_field(crew, "provider", raw.provider.as_deref())?;
    if Provider::parse(&provider).ok() == Some(Provider::Antigravity) {
        validate_antigravity_model(Some(model.as_str()))
            .map_err(|error| OrbitError::InvalidInput(format!("[crews.{crew}].model {error}")))?;
    }
    Ok(CrewAssignment {
        model,
        provider: provider.clone(),
        effort: crew_effort_from_raw(
            crew,
            raw.effort.as_deref(),
            &provider,
            raw.model.as_deref(),
            config_path,
            ignored,
        ),
    })
}

/// Optional crew effort: invalid or provider-unsupported values are ignored
/// so a mistyped optional key cannot fail every command. [ORB-12720]
fn crew_effort_from_raw(
    crew: &str,
    raw_effort: Option<&str>,
    provider: &str,
    raw_model: Option<&str>,
    config_path: &Path,
    ignored: &mut Vec<IgnoredCrewProperty>,
) -> Option<ReasoningEffort> {
    let raw_effort = raw_effort?;
    let effort = match raw_effort.parse::<ReasoningEffort>() {
        Ok(effort) => effort,
        Err(error) => {
            ignore_optional_crew_property(
                ignored,
                config_path,
                crew,
                "effort",
                raw_effort,
                ReasoningEffort::VALUES,
                format!("[crews.{crew}].{error}"),
            );
            return None;
        }
    };
    let provider_id = match Provider::resolve_name(provider) {
        Ok(identity) => identity,
        Err(_) => {
            ignore_optional_crew_property(
                ignored,
                config_path,
                crew,
                "effort",
                raw_effort,
                "omit the key",
                format!(
                    "[crews.{crew}].effort requires a supported effort provider; provider '{provider}' is unsupported"
                ),
            );
            return None;
        }
    };
    if let Err(error) = effort.validate_for_provider_model(provider_id.provider.as_str(), raw_model)
    {
        ignore_optional_crew_property(
            ignored,
            config_path,
            crew,
            "effort",
            raw_effort,
            effort_accepted_values(provider_id.provider.as_str(), raw_model),
            format!("[crews.{crew}].effort {error}"),
        );
        return None;
    }
    Some(effort)
}

fn effort_accepted_values(provider: &str, model: Option<&str>) -> &'static str {
    match provider {
        "antigravity" => "low, medium, high",
        "opencode" => "high, max",
        "grok" => match model.map(str::trim).filter(|model| !model.is_empty()) {
            Some("grok-4.7" | "grok-4.6") => "low, medium, high, xhigh",
            Some("grok-4.5") => "low, medium, high",
            _ => "omit the key",
        },
        "claude" | "codex" | "pi" => ReasoningEffort::VALUES,
        _ => "omit the key",
    }
}

fn ignore_optional_crew_property(
    ignored: &mut Vec<IgnoredCrewProperty>,
    config_path: &Path,
    crew: &str,
    property: &str,
    value: &str,
    accepted: &str,
    error_message: String,
) {
    let config = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %config,
        crew = %crew,
        property = %property,
        value = %value,
        accepted = %accepted,
        "ignoring [crews.{crew}].{property}"
    );
    ignored.push(IgnoredCrewProperty {
        config,
        crew: crew.to_string(),
        property: property.to_string(),
        value: value.to_string(),
        accepted: accepted.to_string(),
        error_message,
    });
}
fn required_crew_field(crew: &str, field: &str, value: Option<&str>) -> Result<String, OrbitError> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    value.map(ToOwned::to_owned).ok_or_else(|| {
        OrbitError::InvalidInput(format!("[crews.{crew}].{field} must not be empty"))
    })
}
