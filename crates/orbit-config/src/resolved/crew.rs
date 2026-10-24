//! Crew admission, optional-property diagnostics and the workflow default lane.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::model_defaults::{
    ANTIGRAVITY_ARGON_MODEL, ANTIGRAVITY_DEFAULT_MODEL, CLAUDE_DEFAULT_STRONG, CLAUDE_DEFAULT_WEAK,
    CLAUDE_FABLE_MODEL, CLAUDE_HAIKU_MODEL, CODEX_ASTRA_MODEL, CODEX_LUNA_MODEL, CODEX_SOL_MODEL,
    COPILOT_DEFAULT_MODEL, CURSOR_DEFAULT_MODEL, GEMINI_CREW_MODEL, GROK_DEFAULT_MODEL,
    MUSE_DEFAULT_MODEL, OPENCODE_DEFAULT_MODEL, PI_DEFAULT_MODEL,
};
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{
    Crew, CrewAssignment, CrewEnvVar, ReasoningEffort, validate_antigravity_model,
};
use orbit_types::task::HostOs;
use orbit_types::workflow::activity_job::Provider;

use super::compatibility::reject_retired_crew_backend;
use super::config::ResolvedConfig;
use crate::crew_pools::reject_unpoolable_crew_name_in_config;
use crate::raw::RawCrewEntry;

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

/// The crew name the retired system lane used [ORB-15663].
///
/// Shipped templates and job steps named `crew: system`, which an alias
/// resolved onto `workflow.system_crew`. System work now draws from the
/// complexity pools like every other task. While no `[crews.system]` table
/// exists, a template, task or job step that still names it is routed
/// through the complexity pool for its work and warned about with
/// [`RETIRED_SYSTEM_CREW_WARNING`]; a configured `[crews.system]` is an
/// ordinary crew.
pub const RETIRED_SYSTEM_CREW: &str = "system";

/// Warning logged whenever a retired `system` crew reference is routed
/// through the complexity pools instead.
pub const RETIRED_SYSTEM_CREW_WARNING: &str = "crew `system` is retired and routes through \
     workflow.<tier>_complexity_crews for the work's complexity; remove the crew name, or \
     name a configured crew";

/// Whether `name` is the retired `system` crew rather than a crew `crews`
/// defines.
pub fn names_retired_system_crew(name: &str, crews: &BTreeMap<String, Crew>) -> bool {
    name.trim() == RETIRED_SYSTEM_CREW && !crews.contains_key(RETIRED_SYSTEM_CREW)
}

/// `workflow.default_crew` naming a disabled crew. Loading still succeeds —
/// unrelated commands keep working — but dispatch on that lane refuses, so
/// `orbit doctor` reports it ahead of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledLaneCrew {
    /// The `workflow.*` key that selects the crew.
    pub key: &'static str,
    /// Crew name the key resolves to.
    pub crew: String,
}

impl DisabledLaneCrew {
    /// Operator-facing warning line for doctor.
    pub fn warning_message(&self) -> String {
        format!(
            "{} names {}; dispatch on that lane is refused",
            self.key,
            disabled_crew_subject(&self.crew)
        )
    }

    /// Corrective command.
    pub fn remediation(&self) -> String {
        format!(
            "orbit config set crews.{}.enabled true (or point {} at an enabled crew)",
            self.crew, self.key
        )
    }
}

/// Refusal text for dispatching a disabled crew: names the crew, the crew
/// table that disables it, and the one-line edit that enables it.
pub fn disabled_crew_message(crew: &str) -> String {
    format!(
        "{}; enable it with `orbit config set crews.{crew}.enabled true` (or set \
         `enabled = true` in [crews.{crew}]), or select an enabled crew",
        disabled_crew_subject(crew)
    )
}

fn disabled_crew_subject(crew: &str) -> String {
    format!("crew `{crew}`, which is disabled ([crews.{crew}] enabled = false)")
}

impl ResolvedConfig {
    /// Lane keys whose crew is disabled. Only a crew this registry defines can
    /// be reported; an unknown name already fails elsewhere.
    pub fn disabled_lane_crews(&self) -> Vec<DisabledLaneCrew> {
        let key = "workflow.default_crew";
        self.default_crew
            .as_deref()
            .and_then(|name| self.crews.get(name))
            .filter(|crew| !crew.enabled)
            .map(|crew| DisabledLaneCrew {
                key,
                crew: crew.name.clone(),
            })
            .into_iter()
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
        ("luna", CODEX_LUNA_MODEL, "codex"),
        ("astra", CODEX_ASTRA_MODEL, "codex"),
        ("gemini", GEMINI_CREW_MODEL, "gemini"),
        ("antigravity", ANTIGRAVITY_DEFAULT_MODEL, "antigravity"),
        ("argon", ANTIGRAVITY_ARGON_MODEL, "antigravity"),
        ("grok", GROK_DEFAULT_MODEL, "grok"),
        ("copilot", COPILOT_DEFAULT_MODEL, "copilot"),
        ("cursor", CURSOR_DEFAULT_MODEL, "cursor"),
        ("pi", PI_DEFAULT_MODEL, "pi"),
        ("opencode", OPENCODE_DEFAULT_MODEL, "opencode"),
        ("muse", MUSE_DEFAULT_MODEL, "muse"),
    ] {
        crews.insert(
            name.to_string(),
            Crew {
                name: name.to_string(),
                assignment: crew_assignment(model, provider),
                description: None,
                tags: Vec::new(),
                enabled: true,
                env: Vec::new(),
            },
        );
    }
    crews
}

/// Host operating systems `provider`'s executor declares, when it declares any.
///
/// `declared` holds only executors that set `host_os`. The provider is
/// trimmed, matched exactly, then ASCII case-insensitively. An unknown
/// provider or an omitted declaration is unrestricted (`None`).
pub fn executor_host_os<'a>(
    provider: &str,
    declared: &'a BTreeMap<String, BTreeSet<HostOs>>,
) -> Option<&'a BTreeSet<HostOs>> {
    let provider = provider.trim();
    if provider.is_empty() {
        return None;
    }
    if let Some(hosts) = declared.get(provider) {
        return Some(hosts);
    }
    declared
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(provider))
        .map(|(_, hosts)| hosts)
}

fn crew_assignment(model: &str, provider: &str) -> CrewAssignment {
    CrewAssignment {
        model: model.to_string(),
        provider: provider.to_string(),
        effort: None,
    }
}
/// `crew_field_path` names the file that supplied `[crews.<name>].<field>`,
/// which a layered load can only know per field after the merge.
pub(crate) fn crews_from_raw(
    raw: Option<&BTreeMap<String, RawCrewEntry>>,
    config_path: &Path,
    crew_field_path: &dyn Fn(&str, &str) -> PathBuf,
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
            assignment: crew_assignment_from_raw(trimmed, entry, crew_field_path, &mut ignored)?,
            description: normalized_crew_description(entry.description.as_deref()),
            tags: normalized_crew_tags(&entry.tags),
            enabled: entry.enabled.unwrap_or(true),
            env: crew_env_from_raw(trimmed, entry.env.as_deref())?,
        };
        if crews.insert(trimmed.to_string(), crew).is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "[crews] contains duplicate name '{trimmed}' after whitespace normalization"
            )));
        }
    }
    Ok((crews, ignored))
}

/// `[crews.<name>] env`, parsed. An entry that is not `NAME` or
/// `NAME=SOURCE` over valid, non-`ORBIT_*` variable names, or a second entry
/// for the same agent variable, fails the load naming the crew and entry: a
/// credential routing the operator wrote must never be dropped silently.
fn crew_env_from_raw(crew: &str, raw: Option<&[String]>) -> Result<Vec<CrewEnvVar>, OrbitError> {
    let mut env: Vec<CrewEnvVar> = Vec::new();
    for entry in raw.unwrap_or_default() {
        let parsed = CrewEnvVar::parse(entry.trim()).map_err(|error| {
            OrbitError::InvalidInput(format!("[crews.{crew}].env entry {error}"))
        })?;
        if env.iter().any(|held| held.name == parsed.name) {
            return Err(OrbitError::InvalidInput(format!(
                "[crews.{crew}].env sets '{}' more than once",
                parsed.name
            )));
        }
        env.push(parsed);
    }
    Ok(env)
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
    crew_field_path: &dyn Fn(&str, &str) -> PathBuf,
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
            crew_field_path,
            ignored,
        ),
    })
}

/// Optional crew effort: invalid or provider-unsupported values are ignored
/// so a mistyped optional key cannot fail every command. The ignored record
/// names the file that set the effort, not the merged document. [ORB-12720]
fn crew_effort_from_raw(
    crew: &str,
    raw_effort: Option<&str>,
    provider: &str,
    raw_model: Option<&str>,
    crew_field_path: &dyn Fn(&str, &str) -> PathBuf,
    ignored: &mut Vec<IgnoredCrewProperty>,
) -> Option<ReasoningEffort> {
    let raw_effort = raw_effort?;
    let config_path = crew_field_path(crew, "effort");
    let effort = match raw_effort.parse::<ReasoningEffort>() {
        Ok(effort) => effort,
        Err(error) => {
            ignore_optional_crew_property(
                ignored,
                &config_path,
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
                &config_path,
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
            &config_path,
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
        "claude" | "codex" | "pi" | "muse" => ReasoningEffort::VALUES,
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
