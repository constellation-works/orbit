//! The committed `.orbit/plugins.yaml` pin file: what a workspace declares it
//! uses. The host installs; the repository never vendors a plugin tree.

use serde::{Deserialize, Serialize};

pub const PIN_FILE_NAME: &str = "plugins.yaml";
pub const PIN_FILE_SCHEMA_VERSION: u32 = 1;

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
        }
        Ok(())
    }
}
