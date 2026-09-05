//! Authoritative agent-family helpers and named crew resolution.
//!
//! This is the single source of truth Orbit consults whenever an activity needs
//! to embed a model duo into its instructions. Splitting the heavy "judgment"
//! model from a cheaper "implementation" helper makes execution mode
//! deterministic per agent family rather than depending on per-prompt edits.
//!
//! Activities reference the resolved pair via the `{{orchestrator_model}}`,
//! `{{helper_model}}`, and `{{agent_family}}` placeholders, which the runtime
//! substitutes into the instruction text before invoking the agent.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{AgentFamily, IdentityError};

/// A resolved (orchestrator, helper) duo for a given agent family.
///
/// - `orchestrator` owns plan, review, and integration responsibilities.
/// - `helper` owns the bounded implementation work delegated by the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModelPair {
    pub orchestrator: String,
    pub helper: String,
}

impl AgentModelPair {
    pub fn new(orchestrator: impl Into<String>, helper: impl Into<String>) -> Self {
        Self {
            orchestrator: orchestrator.into(),
            helper: helper.into(),
        }
    }
}

/// The provider-model assignment selected by a named crew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewAssignment {
    pub model: String,
    pub provider: String,
    /// Optional provider-specific reasoning effort selected for this crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
}

/// The shared crew effort vocabulary accepted by supported provider CLIs.
///
/// This closed set is shared by config admission and CLI argument rendering so
/// an unsupported value cannot make it past config loading and then be
/// silently ignored at execution time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub const VALUES: &'static str = "low, medium, high, xhigh, max";

    /// Validates the provider-model contract before an effort reaches argv.
    ///
    /// Claude and Codex expose the complete crew vocabulary. Grok's published
    /// contract is model-specific, so unknown models fail closed instead of
    /// accepting a setting the CLI might silently reinterpret.
    pub fn validate_for_provider_model(
        self,
        provider: &str,
        model: Option<&str>,
    ) -> Result<(), String> {
        match provider {
            "claude" | "codex" => Ok(()),
            "grok" => Self::validate_grok_model_effort(self, model),
            other => Err(format!(
                "provider '{other}' does not support configured reasoning effort"
            )),
        }
    }

    fn validate_grok_model_effort(self, model: Option<&str>) -> Result<(), String> {
        let model = model.map(str::trim).filter(|model| !model.is_empty());
        match (model, self) {
            (Some("grok-4.6"), Self::Low | Self::Medium | Self::High | Self::Xhigh) => Ok(()),
            (Some("grok-4.5"), Self::Low | Self::Medium | Self::High) => Ok(()),
            (Some("grok-4.6"), effort) => Err(format!(
                "Grok model 'grok-4.6' supports effort values low, medium, high, xhigh; '{effort}' is unsupported"
            )),
            (Some("grok-4.5"), effort) => Err(format!(
                "Grok model 'grok-4.5' supports effort values low, medium, high; '{effort}' is unsupported"
            )),
            (Some(model), _) => Err(format!(
                "Grok effort support is verified only for models 'grok-4.5' and 'grok-4.6'; model '{model}' is unsupported"
            )),
            (None, _) => Err(
                "Grok effort requires an explicit model; supported models are 'grok-4.5' and 'grok-4.6'"
                    .to_string(),
            ),
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        })
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            other => Err(format!(
                "invalid effort '{other}'; expected one of {}",
                Self::VALUES
            )),
        }
    }
}

/// A named provider-model assignment used for activity dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crew {
    pub name: String,
    pub assignment: CrewAssignment,
    /// Optional human-facing summary carried through every canonical crew
    /// projection. Execution-profile publication normalizes blank values to
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Search/discovery labels. Runtime config loading canonicalizes these to
    /// a sorted, deduplicated list of non-empty strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Resolve a named crew from the active registry.
pub fn resolve_crew(name: &str, registry: &BTreeMap<String, Crew>) -> Result<Crew, IdentityError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(IdentityError::invalid_input_with_suggestions(
            "crew name must not be empty",
            registry.keys().cloned().collect(),
        ));
    }
    registry.get(trimmed).cloned().ok_or_else(|| {
        IdentityError::invalid_input_with_suggestions(
            format!("crew '{trimmed}' is not defined in [crews.*]"),
            registry.keys().cloned().collect(),
        )
    })
}

/// The full set of agent CLI families Orbit knows how to orchestrate.
///
/// This is the single source of truth for the supported agent families.
///
/// The return type is a fixed-size array rather than a `Vec` so the
/// cardinality is enforced at compile time: adding a family requires
/// changing the array size, which in turn surfaces any call site that
/// made assumptions about the previous number of families.
pub const fn all_agent_families() -> [&'static str; 4] {
    [
        AgentFamily::Codex.as_str(),
        AgentFamily::Claude.as_str(),
        AgentFamily::Gemini.as_str(),
        AgentFamily::Grok.as_str(),
    ]
}

/// Normalize an `agent_cli` value into a stable, lowercased family identifier
/// (e.g. `/usr/local/bin/Codex` -> `codex`).
pub fn agent_family_from_cli(agent_cli: &str) -> String {
    Path::new(agent_cli)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(agent_cli)
        .to_ascii_lowercase()
}

/// Best-effort reverse mapping from an exact model string to the agent CLI
/// family that would invoke it.
///
/// Orbit stores model-only attribution on tasks, but some execution paths still
/// need to recover the agent family for provider dispatch. This helper accepts
/// both the new exact model strings (for example a `claude-opus` build id) and
/// the older shorthand values that may still appear in legacy artifacts.
pub fn infer_agent_family_from_model(model: &str) -> Option<String> {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }

    if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        return Some("codex".to_string());
    }
    if model.starts_with("claude-")
        || model.starts_with("opus")
        || model.starts_with("sonnet")
        || model.starts_with("fable")
    {
        return Some("claude".to_string());
    }
    if model.starts_with("gemini-") {
        return Some("gemini".to_string());
    }
    // Grok (xAI) — supports both grok-4 style and the shorter grok3* naming
    if model.starts_with("grok-") || model.starts_with("grok3") {
        return Some("grok".to_string());
    }

    None
}

/// Normalize an optional legacy agent family and optional model into the agent
/// family implied by the pair.
///
/// `model` is the preferred provenance field for tool calls. When it names a
/// known Orbit provider family, this helper infers the agent family from the
/// model. Legacy callers may still pass `agent`; if both are present and the
/// model maps to a different family, Orbit rejects the inconsistent identity
/// instead of recording contradictory attribution.
pub fn normalize_agent_family_for_model(
    agent_cli: Option<&str>,
    model: Option<&str>,
) -> Result<Option<String>, IdentityError> {
    let agent = agent_cli
        .map(agent_family_from_cli)
        .filter(|value| !value.trim().is_empty());
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let inferred = model.and_then(infer_agent_family_from_model);

    if let (Some(agent), Some(inferred)) = (agent.as_deref(), inferred.as_deref())
        && agent != inferred
    {
        return Err(IdentityError::Invalid(format!(
            "`agent` '{agent}' does not match `model` '{}' (inferred agent family '{inferred}')",
            model.unwrap_or_default()
        )));
    }

    Ok(agent.or(inferred))
}
