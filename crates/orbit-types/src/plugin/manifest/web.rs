//! `spec.web`: dashboard panels, links and pages (§4.7).

use serde::{Deserialize, Serialize};

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

/// The markup file every page root holds.
pub const PAGE_INDEX_FILE: &str = "index.html";

/// The page bridge versions this Orbit knows (`spec.web.pages[].kit`).
pub const KNOWN_PAGE_KITS: &[u32] = &[1];

/// The file name endings a page script may have. `.mjs` is written as a
/// module script, any other as a classic one.
pub const PAGE_SCRIPT_EXTENSIONS: &[&str] = &[".js", ".mjs"];

/// The file name ending a page style must have.
pub const PAGE_STYLE_EXTENSION: &str = ".css";

/// The most bytes one page may hold: `index.html` plus every listed script
/// and style (2 MiB).
pub const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024;

/// Text a page's markup may not hold, compared without ASCII case: Orbit
/// writes the page's scripts and styles itself.
pub const PAGE_MARKUP_REFUSED: &[&str] = &["<script", "<link"];

/// Text a page script may not hold, compared without ASCII case: either
/// would end its `script` element early.
pub const PAGE_SCRIPT_REFUSED: &[&str] = &["</script", "<!--"];

/// Text a page style may not hold, compared without ASCII case: it would
/// end its `style` element early.
pub const PAGE_STYLE_REFUSED: &[&str] = &["</style"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebSection {
    #[serde(default)]
    pub panels: Vec<PluginWebPanel>,
    #[serde(default)]
    pub links: Vec<PluginWebLink>,
    #[serde(default)]
    pub pages: Vec<PluginWebPage>,
}

impl PluginWebSection {
    /// Page id → its `actions`, for every page that names one: the list an
    /// enabling command asks consent for.
    pub fn declared_actions(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.pages
            .iter()
            .filter(|page| !page.actions.is_empty())
            .map(|page| (page.id.clone(), page.actions.clone()))
            .collect()
    }
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

/// A screen a plugin ships and the dashboard shows under Plugins (design
/// `docs/design/plugin-pages/2_design.md` §1): `index.html` for the markup,
/// plus the scripts and styles listed here, all under `root`. Files in
/// `root` that are not listed are never loaded, hashed or served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebPage {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// A directory in the plugin root holding `index.html`.
    pub root: String,
    /// Files under `root`, written into the page in this order.
    #[serde(default)]
    pub scripts: Vec<String>,
    /// Files under `root`, written into the page in this order.
    #[serde(default)]
    pub styles: Vec<String>,
    /// The bridge version the page was written for; one of
    /// [`KNOWN_PAGE_KITS`]. Omitted: the page uses no kit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit: Option<u32>,
    /// Verbs of this plugin's `read_only` tools the page may call.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Verbs of this plugin's mutating tools the page may ask to run. Each
    /// run needs the operator session, the operator's consent to this list
    /// at enable, and a confirmation the dashboard draws outside the frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

impl PluginWebPage {
    /// The sub-tab label: `title`, or the id when the title is blank.
    pub fn label(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.id
        } else {
            &self.title
        }
    }
}

/// The first of `needles` found in `haystack`, compared without ASCII case.
/// The one matcher every page-part check uses, for a local page's files and
/// another host's parts alike.
pub fn find_page_refused<'a>(haystack: &[u8], needles: &[&'a str]) -> Option<&'a str> {
    needles.iter().copied().find(|needle| {
        let needle = needle.as_bytes();
        haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    })
}
