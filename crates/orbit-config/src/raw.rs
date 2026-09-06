//! Private serde schema for the parts of `config.toml` that are not fixed
//! registry keys: dynamically named crew tables, and the retired keys whose
//! continued presence has to be diagnosed rather than ignored.
//!
//! Fixed settings are admitted by [`crate::registry`] instead. Nothing here is
//! public except [`CrewSeed`], which is the narrow DTO the CLI init adapter
//! fills in from its prompts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawRuntimeConfig {
    // Deliberately no `deny_unknown_fields`: config.toml is also home to
    // independently parsed surfaces such as `[docs]`. Runtime admission reads
    // only its owned keys, while explicit migration guards below reject retired
    // runtime keys whose continued acceptance would be unsafe or misleading.
    #[allow(dead_code)]
    pub(crate) identity: Option<toml::Value>,
    pub(crate) task: Option<RawTaskSection>,
    pub(crate) knowledge: Option<RawKnowledgeConfig>,
    pub(crate) watch: Option<toml::Value>,
    /// Removed in ORB-00058. Kept only so config loading can reject stale
    /// `[agent.<role>]` tables with an explicit migration error.
    pub(crate) agent: Option<BTreeMap<String, toml::Value>>,
    /// `[crews.<name>]` registry. Each table supplies one assignment Orbit
    /// resolves for activity dispatch at run start.
    pub(crate) crews: Option<BTreeMap<String, RawCrewEntry>>,
    /// Retired in ORB-10627. Existing workspaces may still carry the section
    /// written by older `orbit init`; loaders warn and ignore it.
    pub(crate) duel: Option<toml::Value>,
}

/// One provider-model crew assignment supplied by a caller seeding a fresh
/// `config.toml`.
///
/// This is the crate's only public raw DTO. It exists because the CLI init
/// adapter — which owns host detection and the interactive prompts — has to
/// hand its collected answers back across the crate boundary; see
/// [`crate::ConfigSeed`].
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CrewSeed {
    /// Provider family (`claude`, `codex`, `gemini`, `grok`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model name dispatched for this crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct RawCrewEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    /// Optional Codex reasoning effort. Parsed into the typed closed set when
    /// the effective crew registry is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<String>,
    /// Retired in ORB-10801. Read only so a crew that still pins the agent
    /// execution backend is either accepted as inert (`cli`) or refused with
    /// the migration message, never silently re-pointed at another runtime.
    #[serde(default, skip_serializing)]
    pub(crate) backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tags: Vec<String>,
    /// Retired role tables are retained only to reject stale configuration
    /// with rewrite guidance at load time. They are not part of the crew
    /// schema and never participate in assignment resolution or serialization.
    #[serde(default, skip_serializing)]
    pub(crate) planner: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing)]
    pub(crate) implementer: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing)]
    pub(crate) reviewer: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawKnowledgeConfig {
    /// Deprecated legacy key. Kept only so loaders can warn and ignore it.
    pub(crate) task_id_pattern: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawTaskSection {
    /// Removed pre-release selector. Kept here only so config loading can
    /// reject stale keys with an explicit task-artifacts cutover message.
    pub(crate) artifact_store: Option<String>,
}
