//! The committed `.orbit/plugins.yaml` pin file: what a workspace declares it
//! uses. The host installs; the repository never vendors a plugin tree.

use serde::{Deserialize, Serialize};

pub const PIN_FILE_NAME: &str = "plugins.yaml";
pub const PIN_FILE_SCHEMA_VERSION: u32 = 1;

/// Suffixes Orbit recognises as a plugin archive.
pub const PLUGIN_ARCHIVE_EXTENSIONS: &[&str] = &[".tar.gz", ".tgz", ".tar", ".zip"];

/// The URL when `source` names an archive Orbit fetches over the network.
///
/// This predicate lives beside the pin file because the pin is the only place
/// such an archive's digest can be declared, and it is what decides which
/// entries [`PluginPinFile::validate`] requires a `digest` on.
pub fn remote_archive_source(source: &str) -> Option<&str> {
    if !source.starts_with("https://") {
        return None;
    }
    let path = source.split(['?', '#']).next().unwrap_or(source);
    let lowered = path.to_ascii_lowercase();
    PLUGIN_ARCHIVE_EXTENSIONS
        .iter()
        .any(|extension| lowered.ends_with(extension))
        .then_some(source)
}

/// Normalize a `sha256:<hex>` pin to its lowercase hex digest.
///
/// The prefix is required rather than optional so the pin file always says
/// which algorithm it names, leaving room to add another without guessing
/// from a digest's length.
pub fn parse_archive_digest(value: &str) -> Result<String, String> {
    let Some(hex) = value.trim().strip_prefix("sha256:") else {
        return Err(format!(
            "'{value}' is not a supported digest; use `sha256:<64 hex characters>`"
        ));
    };
    let normalized = hex.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "'{value}' is not a `sha256:` digest of 64 hex characters"
        ));
    }
    Ok(normalized)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPinFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub plugins: Vec<PluginPin>,
}

impl Default for PluginPinFile {
    fn default() -> Self {
        Self {
            schema_version: PIN_FILE_SCHEMA_VERSION,
            plugins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPin {
    /// Plugin namespace (`metadata.name`).
    pub name: String,
    /// Version requirement the workspace expects (a version or a range).
    #[serde(default)]
    pub version: Option<String>,
    /// Where `orbit plugin sync` installs it from when the host lacks it.
    #[serde(default)]
    pub source: Option<String>,
    /// `sha256:<hex>` the archive at an `https://` source must hash to.
    /// Required for such a source and refused for any other: nothing else
    /// here is fetched by Orbit, so a digest on it would claim a guarantee
    /// the install never checks.
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl PluginPinFile {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PIN_FILE_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion: unsupported pin file schemaVersion {}; expected {PIN_FILE_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (index, pin) in self.plugins.iter().enumerate() {
            if !super::is_valid_namespace(&pin.name) {
                return Err(format!(
                    "plugins[{index}].name: '{}' is not a valid plugin namespace",
                    pin.name
                ));
            }
            if !seen.insert(pin.name.as_str()) {
                return Err(format!(
                    "plugins[{index}].name: '{}' is pinned more than once",
                    pin.name
                ));
            }
            if let Some(version) = &pin.version {
                super::SemverRange::parse(version)
                    .map_err(|error| format!("plugins[{index}].version: {error}"))?;
            }
            validate_pin_digest(index, pin)?;
        }
        Ok(())
    }
}

/// A fetched archive is pinned by digest or it is not installed: Orbit has no
/// trust-on-first-use path, because the first fetch is exactly the one an
/// attacker who controls the URL would serve.
fn validate_pin_digest(index: usize, pin: &PluginPin) -> Result<(), String> {
    let archive = pin.source.as_deref().and_then(remote_archive_source);
    match (&pin.digest, archive) {
        (Some(digest), Some(_)) => parse_archive_digest(digest)
            .map(|_| ())
            .map_err(|error| format!("plugins[{index}].digest: {error}")),
        (Some(_), None) => Err(format!(
            "plugins[{index}].digest: only an `https://` archive source is digest-verified; \
             remove the digest or pin an archive URL"
        )),
        (None, Some(url)) => Err(format!(
            "plugins[{index}].digest: the archive source '{url}' must pin a `sha256:` digest; \
             Orbit does not trust a fetched archive on first use"
        )),
        (None, None) => Ok(()),
    }
}
