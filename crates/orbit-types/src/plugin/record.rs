//! Host-local plugin state persisted in `plugin_store`, and the provenance
//! stamped on every audited plugin tool call.

use serde::{Deserialize, Serialize};

/// One installed plugin as the host records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    /// Namespace (`metadata.name`).
    pub name: String,
    pub version: String,
    /// Where the plugin was installed from (path, `git+url#ref`, archive).
    pub source: String,
    /// Absolute install directory (`~/.orbit/plugins/<ns>/<version>`).
    pub install_path: String,
    /// SHA-256 of the manifest bytes at install time, hex encoded.
    pub manifest_digest: String,
    /// SHA-256 of the archive this was installed from, hex encoded, when
    /// `source` was an `https://` archive Orbit fetched and verified against
    /// a pinned digest. `None` for a directory, a `git+` clone, or a local
    /// archive, none of which Orbit downloads.
    #[serde(default)]
    pub archive_digest: Option<String>,
    pub enabled: bool,
    /// Grants recorded at `orbit plugin enable --grant …`; the loader refuses
    /// a tool whose plugin lacks one it requires (design §4.1).
    #[serde(default)]
    pub grants: Vec<String>,
    /// Whether the loader verified a first-party origin for `orbit.<ns>.*`.
    #[serde(default)]
    pub first_party: bool,
    /// The Orbit version whose `orbit plugin test` run this plugin's goldens
    /// last passed on, recorded by that run (design §5). `None` until the
    /// conformance suite passes here; `orbit plugin show` prints it as
    /// "certified for <version>".
    #[serde(default)]
    pub certified_orbit_version: Option<String>,
    pub installed_at: String,
    pub updated_at: String,
}

/// Why a plugin's tools are (or are not) on the active surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    /// Installed, enabled, manifest loads, requirements satisfied.
    Active,
    /// Installed but `orbit plugin enable` has not been run.
    Disabled,
    /// Pinned by the workspace but not installed on this host.
    Missing,
    /// Enabled but refused at load: the diagnostic names the reason.
    Inactive,
}

impl PluginStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Missing => "missing",
            Self::Inactive => "inactive",
        }
    }
}

/// Identity of the plugin behind a tool call, carried on the audit row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginProvenance {
    pub name: String,
    pub version: String,
    pub manifest_digest: String,
    /// The grant set the plugin ran under, as recorded at enable time
    /// (design §4.4). Empty for a plugin that requested nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<String>,
}
