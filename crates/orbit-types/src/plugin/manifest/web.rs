//! `spec.web`: dashboard panels and links (§4.7).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebSection {
    #[serde(default)]
    pub panels: Vec<PluginWebPanel>,
    #[serde(default)]
    pub links: Vec<PluginWebLink>,
}

/// One dashboard panel (§4.7): the output of a `read_only` tool, drawn by
/// the generic renderer named in `render`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebPanel {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// `tool:<verb>`, naming one of this plugin's `read_only` tools.
    pub source: String,
    #[serde(default)]
    pub render: PluginPanelRender,
    #[serde(default)]
    pub group: PluginPanelGroup,
    /// How long the dashboard server may reuse this panel's last successful
    /// response. Omitted panels use [`DEFAULT_PANEL_REFRESH_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_ms: Option<u64>,
}

impl PluginWebPanel {
    /// The verb after `tool:`, when the source has that form.
    pub fn source_verb(&self) -> Option<&str> {
        self.source
            .strip_prefix(PANEL_SOURCE_TOOL_PREFIX)
            .map(str::trim)
            .filter(|verb| !verb.is_empty())
    }
}

/// The only source form v1 accepts.
pub const PANEL_SOURCE_TOOL_PREFIX: &str = "tool:";

/// Default server-side panel cache window. It matches the dashboard refresh
/// cadence, so overlapping tabs share a single audited backend execution.
pub const DEFAULT_PANEL_REFRESH_MS: u64 = 30_000;

/// Bounds keep a manifest from accidentally turning a visible dashboard tab
/// into a tight backend loop or a day-long stale view.
pub const MIN_PANEL_REFRESH_MS: u64 = 1_000;
pub const MAX_PANEL_REFRESH_MS: u64 = 3_600_000;

/// The schemes a `spec.web.links[].url` may use (§4.7).
pub const LINK_URL_SCHEMES: &[&str] = &["http://", "https://"];

/// How the dashboard draws a panel's JSON (§4.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginPanelRender {
    /// An object as label/value pairs.
    Kv,
    /// An array of objects as one table; columns are the union of keys.
    Table,
    /// A string (or an object's `markdown`/`text` field) as sanitised Markdown.
    Markdown,
    /// Pretty-printed JSON.
    #[default]
    Json,
}

impl PluginPanelRender {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kv => "kv",
            Self::Table => "table",
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

/// Which section of a plugin's dashboard card a panel lands in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginPanelGroup {
    #[default]
    Diagnostics,
    Operations,
    Config,
}

impl PluginPanelGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diagnostics => "diagnostics",
            Self::Operations => "operations",
            Self::Config => "config",
        }
    }
}

/// A plain tile pointing at a plugin-hosted UI (§4.7). `url` may use the
/// manifest template variables, typically `{{config.<key>}}` for a port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebLink {
    pub title: String,
    pub url: String,
}
