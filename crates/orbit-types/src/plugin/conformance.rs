//! `spec.tests` goldens: the request/response pairs `orbit plugin test` runs
//! through the real protocol (design §5).
//!
//! A golden is a plugin's own statement of what its tools return, written
//! once and re-checked against every Orbit it is certified for. The shape is
//! deliberately small: a tool, an input, and the output or error fields the
//! plugin promises.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::manifest::PluginManifestError;
use super::namespace::is_valid_verb;

pub const TEST_FILE_SCHEMA_VERSION: u32 = 1;
pub const TEST_FILE_KIND: &str = "PluginTest";

/// One `spec.tests[]` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginTestFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub kind: String,
    pub tests: Vec<PluginTestCase>,
}

/// One golden: call `tool` with `input`, expect output or plugin error fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginTestCase {
    pub name: String,
    /// The manifest verb, not the canonical `<ns>.<verb>` name: a golden
    /// travels with the plugin and must not depend on whether the host
    /// verified a first-party origin.
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    pub expect: PluginTestExpectation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum PluginTestExpectation {
    /// The exact tool output. Compared as JSON, so key order does not matter.
    Output { output: Value },
    /// A backend-declared error, with optional retryability and detail checks.
    Error { error: PluginTestErrorExpectation },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginTestErrorExpectation {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_json"
    )]
    pub detail: Option<Value>,
}

/// Keep an explicitly expected JSON null distinct from an omitted detail.
fn present_json<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl PluginTestFile {
    /// Structural validation: versions, kinds, verb spelling, duplicate test
    /// names. `field` prefixes each diagnostic with the file it came from.
    pub fn validate(&self, field: &str) -> Result<(), PluginManifestError> {
        if self.schema_version != TEST_FILE_SCHEMA_VERSION {
            return Err(PluginManifestError::new(
                field,
                format!(
                    "unsupported schemaVersion {}; expected {TEST_FILE_SCHEMA_VERSION}",
                    self.schema_version
                ),
            ));
        }
        if self.kind != TEST_FILE_KIND {
            return Err(PluginManifestError::new(
                field,
                format!("expected 'kind: {TEST_FILE_KIND}', found '{}'", self.kind),
            ));
        }
        if self.tests.is_empty() {
            return Err(PluginManifestError::new(
                field,
                "declares no tests; remove the file or add one",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for case in &self.tests {
            if case.name.trim().is_empty() {
                return Err(PluginManifestError::new(field, "a test needs a name"));
            }
            if !seen.insert(case.name.as_str()) {
                return Err(PluginManifestError::new(
                    field,
                    format!("test '{}' is declared more than once", case.name),
                ));
            }
            if !is_valid_verb(&case.tool) {
                return Err(PluginManifestError::new(
                    field,
                    format!(
                        "test '{}' names tool '{}', which is not a valid tool verb",
                        case.name, case.tool
                    ),
                ));
            }
            if let PluginTestExpectation::Error { error } = &case.expect
                && error.code.trim().is_empty()
            {
                return Err(PluginManifestError::new(
                    field,
                    format!("test '{}' has an empty expect.error.code", case.name),
                ));
            }
        }
        Ok(())
    }
}
