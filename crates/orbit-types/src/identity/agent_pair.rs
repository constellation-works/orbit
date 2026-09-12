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
    /// Claude and Codex expose the complete crew vocabulary. Pi does too: its
    /// `--thinking` flag validates against a fixed, model-independent set
    /// (`off, minimal, low, medium, high, xhigh, max`) that is a strict
    /// superset of this enum, and it rejects a value outside that set with a
    /// diagnostic rather than ignoring it. [ORB-11296]
    ///
    /// Grok's published contract is model-specific, so unknown models fail
    /// closed instead of accepting a setting the CLI might silently
    /// reinterpret. OpenCode is narrower still: see
    /// [`Self::validate_opencode_effort`]. [ORB-11295]
    pub fn validate_for_provider_model(
        self,
        provider: &str,
        model: Option<&str>,
    ) -> Result<(), String> {
        match provider {
            "claude" | "codex" | "pi" => Ok(()),
            "grok" => Self::validate_grok_model_effort(self, model),
            "antigravity" => Self::validate_antigravity_effort(self, model),
            "opencode" => Self::validate_opencode_effort(self),
            other => Err(format!(
                "provider '{other}' does not support configured reasoning effort"
            )),
        }
    }

    /// OpenCode renders effort as `--variant`, documented as "model variant
    /// (provider-specific reasoning effort, e.g., high, max, minimal)". The
    /// value is forwarded verbatim to whichever model provider `--model`
    /// selected, and OpenCode publishes no provider-independent vocabulary, so
    /// only the two spellings its own help text names *and* that exist in
    /// Orbit's crew vocabulary are accepted. `low`, `medium`, and `xhigh` fail
    /// closed rather than being remapped onto `minimal`/`high`: a variant the
    /// underlying provider does not define is a configuration error, not
    /// something Orbit should guess at. [ORB-11295]
    fn validate_opencode_effort(self) -> Result<(), String> {
        match self {
            Self::High | Self::Max => Ok(()),
            other => Err(format!(
                "OpenCode CLI supports effort values high, max (`opencode run --variant`); '{other}' is unsupported. Values are not remapped; choose a supported effort or set a model whose provider defines the variant."
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

    fn validate_antigravity_effort(self, model: Option<&str>) -> Result<(), String> {
        match self {
            Self::Low | Self::Medium | Self::High => validate_antigravity_model(model),
            other => Err(format!(
                "Antigravity CLI supports effort values low, medium, high (`agy --effort`); '{other}' is unsupported. Migrate xhigh/max to high, or choose a *-high model slug from `agy models`. Values are not remapped."
            )),
        }
    }
}

/// Reject Gemini CLI model ids that `agy` does not accept.
///
/// Verified against Antigravity CLI 1.1.27 (`agy models`): Gemini slugs carry
/// an effort suffix (`gemini-3.8-flash-high`). Bare ids such as
/// `gemini-3.8-flash` fail at the CLI rather than falling back. Orbit fails
/// the same way with migration text instead of rewriting the id. [ORB-11299]
pub fn validate_antigravity_model(model: Option<&str>) -> Result<(), String> {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return Ok(());
    };
    if is_legacy_gemini_cli_model_id(model) {
        return Err(format!(
            "Antigravity CLI does not accept Gemini CLI model id '{model}'. Use a slug from `agy models` such as gemini-3.8-flash-high; ids are not remapped. Individual Gemini CLI accounts stopped on 2026-06-18, but enterprise Gemini Code Assist and API-key Gemini CLI remain available on the legacy `gemini` provider."
        ));
    }
    Ok(())
}

fn is_legacy_gemini_cli_model_id(model: &str) -> bool {
    if !model.starts_with("gemini-") {
        return false;
    }
    !(model.ends_with("-low") || model.ends_with("-medium") || model.ends_with("-high"))
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

    if all_agent_families().iter().any(|family| model == *family) {
        return Some(model);
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
        .filter(|value| !value.trim().is_empty())
        // `agy` / `antigravity` name the execution lane, not a family.
        // Family comes from the model string (`gemini-*` stays `gemini`).
        .filter(|value| !is_antigravity_cli(value));
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

/// Resolve an optional agent/model pair to a canonical write-attribution family.
///
/// A present `model` (or `agent`) must name `codex`, `claude`, `gemini`, or
/// `grok`, or be a full model string those families can infer. Unrecognized
/// values such as `llama` are refused rather than stored verbatim.
pub fn require_canonical_agent_family(
    agent_cli: Option<&str>,
    model: Option<&str>,
) -> Result<Option<String>, IdentityError> {
    let family = normalize_agent_family_for_model(agent_cli, model)?;
    let shown = model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| agent_cli.map(str::trim).filter(|value| !value.is_empty()));
    let Some(shown) = shown else {
        return Ok(None);
    };

    match family {
        Some(family) if all_agent_families().iter().any(|known| family == *known) => {
            Ok(Some(family))
        }
        _ => Err(IdentityError::Invalid(format!(
            "`model` '{shown}' is not a canonical agent family (codex, claude, gemini, grok) or a recognized full model string"
        ))),
    }
}

fn is_antigravity_cli(name: &str) -> bool {
    matches!(name, "agy" | "antigravity")
}
