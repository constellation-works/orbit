//! Private serde schema for the parts of `config.toml` that are not fixed
//! registry keys: dynamically named crew tables, and the retired keys whose
//! continued presence has to be diagnosed rather than ignored.
//!
//! Fixed settings are admitted by [`crate::registry`] instead. Nothing here is
//! public.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawRuntimeConfig {
    // Deliberately no `deny_unknown_fields`: config.toml is also home to
    // forward-compatible extension tables. Runtime admission reads only its
    // owned keys; crate::unknown diagnoses unrecognised paths without failing.
    // Retired runtime tables with no field here (`[watch]`, `[agent.<role>]`,
    // `[task]`) take the same path: warned about and ignored.
    /// `[crews.<name>]` registry. Each table supplies one assignment Orbit
    /// resolves for activity dispatch at run start.
    pub(crate) crews: Option<BTreeMap<String, RawCrewEntry>>,
    /// `[plugins.<ns>]` registry. Each table is owned by one installed plugin
    /// and validated against that plugin's JSON Schema by the layer that knows
    /// which plugins exist (`orbit-core`). A section for a plugin this host has
    /// not installed is deliberately tolerated here: a workspace that pins a
    /// plugin must still load on a machine that has not installed it yet.
    pub(crate) plugins: Option<BTreeMap<String, toml::Value>>,
    /// Retired in ORB-12236. Existing workspaces may still carry
    /// `[routines] role = "source"`; loaders warn and ignore it.
    /// Delete this guard after 2026-12-01, when the section becomes an
    /// ordinary unknown table.
    pub(crate) routines: Option<toml::Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct RawCrewEntry {
    /// Whether dispatch may run the crew. Absent means enabled, so a table
    /// written before the key existed keeps working unchanged. Serde admits
    /// only a TOML boolean here, so a mistyped value fails the load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    /// `agent` (the default when absent) or `human`. A human crew names an
    /// operator: it sets no provider, model, effort, env or vision, and
    /// nothing dispatches it. A dedicated key rather than the retired
    /// `backend`, whose migration guard stays as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
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
    /// `NAME` / `NAME=SOURCE` entries this crew's agents receive on top of
    /// `[execution.env] pass`. Global config only; parsed and validated when
    /// the registry is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) env: Option<Vec<String>>,
    /// Whether the crew may be given images. Absent means it may when its
    /// executor declares `image_input`; `false` opts a text-only model out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vision: Option<bool>,
}
