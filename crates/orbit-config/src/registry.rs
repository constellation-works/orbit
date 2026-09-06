//! Admission registry for fixed `config.toml` settings.
//!
//! Each setting is declared once in [`define_config_settings!`]. That row
//! drives TOML extraction, defaulting/validation, `orbit config keys`
//! metadata, the resolved snapshot, and JSON lookup used by `get`/`show`.
//! Runtime consumers read the admitted snapshot instead of re-parsing raw
//! section structs. Dynamically named tables (`crews.*`) and removed-key
//! migration guards remain in `raw`/`runtime` because they are not fixed
//! settings addressable by `orbit config set`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::LogRotationConfig;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{Crew, CrewAssignment, resolve_crew};
use orbit_types::workflow::Provider;
use serde::de::DeserializeOwned;
use serde_json::{Value as JsonValue, json};

const DEFAULT_WORKFLOW_BASE_BRANCH: &str = "main";
const DEFAULT_WORKFLOW_CREW: &str = "opus";
/// Name of the crew seeded for the bounded system lane. `orbit init` writes
/// both this crew table and the `workflow.system_crew` key that points at it,
/// so the two must stay in step. Shipped job steps also name this crew
/// directly, so it must resolve on hosts whose config predates it — see the
/// alias in `resolved::crews_from_raw`.
pub(crate) const DEFAULT_WORKFLOW_SYSTEM_CREW: &str = "system";
/// The crew that carried the system lane before `system` existed. Still seeded
/// in its own right; named here because a config written before ORB-10877
/// defines only this one and must keep resolving system work.
pub(crate) const LEGACY_WORKFLOW_SYSTEM_CREW: &str = "qa";
const LEGACY_DEFAULT_WORKFLOW_CREW: &str = "claude";
const CONSTELLATION_DEFAULT_PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";

/// One settable `config.toml` key, as advertised by `orbit config keys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigKeyDescriptor {
    /// Dotted key path.
    pub key: &'static str,
    /// Human-readable value type (`string`, `bool`, `array<string>`, ...).
    pub value_type: &'static str,
    /// What the setting controls.
    pub description: &'static str,
}

macro_rules! define_config_settings {
    ($(
        $field:ident : $resolved:ty => $raw:ty {
            key: $key:literal,
            value_type: $value_type:literal,
            description: $description:literal,
            resolve: $resolve:expr $(,)?
        }
    ),+ $(,)?) => {
        /// Fully admitted, defaulted view of every fixed configuration key.
        #[derive(Debug, Clone)]
        pub struct ConfigSnapshot {
            /// Derived security invariant, shown by `config show` but not settable.
            pub execution_env_inherit: bool,
            $(
                #[doc = $description]
                pub $field: $resolved,
            )+
        }

        /// Every settable key, in declaration order.
        pub const CONFIG_KEY_REGISTRY: &[ConfigKeyDescriptor] = &[
            $(ConfigKeyDescriptor {
                key: $key,
                value_type: $value_type,
                description: $description,
            },)+
        ];

        impl ConfigSnapshot {
            pub(crate) fn admit(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
            ) -> Result<Self, OrbitError> {
                let env_default = std::env::var(CONSTELLATION_DEFAULT_PROVIDER_ENV).ok();
                Self::admit_with_env(document, config_path, crews, env_default.as_deref())
            }

            fn admit_with_env(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
                env_default: Option<&str>,
            ) -> Result<Self, OrbitError> {
                $(let $field: $resolved = {
                    let raw_value: Option<$raw> = read_optional(document, $key, config_path)?;
                    ($resolve)(raw_value)?
                };)+
                let mut snapshot = Self {
                    execution_env_inherit: false,
                    $($field,)+
                };
                snapshot.finish_admission(crews, env_default)?;
                Ok(snapshot)
            }

            /// JSON projection of one registry key, or `None` when the key is
            /// not a registered setting.
            pub fn value_for(&self, key: &str) -> Option<JsonValue> {
                match key {
                    $($key => Some(json!(self.$field)),)+
                    _ => None,
                }
            }

            /// JSON projection of every registry key, in registry order.
            pub fn all_values(&self) -> Vec<(&'static str, JsonValue)> {
                CONFIG_KEY_REGISTRY
                    .iter()
                    .map(|entry| {
                        // Both match arms are emitted by this macro, so every
                        // registry row has a projection by construction.
                        (entry.key, self.value_for(entry.key).unwrap_or(JsonValue::Null))
                    })
                    .collect()
            }
        }
    };
}

define_config_settings! {
    codex_approval_policy: Option<String> => String {
        key: "execution.codex.approval_policy", value_type: "string",
        description: "Codex approval policy: one of untrusted, on-request, never.",
        resolve: |raw: Option<String>| resolve_optional_choice(raw, "execution.codex.approval_policy", &["untrusted", "on-request", "never"]),
    },
    codex_sandbox: String => String {
        key: "execution.codex.sandbox", value_type: "string",
        description: "Codex sandbox mode: one of read-only, workspace-write, danger-full-access.",
        resolve: |raw: Option<String>| resolve_choice(raw, "workspace-write", "execution.codex.sandbox", &["read-only", "workspace-write", "danger-full-access"]),
    },
    execution_env_pass: Vec<String> => Vec<String> {
        key: "execution.env.pass", value_type: "array<string>",
        description: "Environment variable names allow-listed for passthrough into agent subprocesses.",
        resolve: |raw: Option<Vec<String>>| raw.map(normalize_pass_list).unwrap_or_else(|| Ok(default_pass_list())),
    },
    pr_task_url_template: Option<String> => String {
        key: "pr.task_url_template", value_type: "string",
        description: "URL template used to link a task ID in PR descriptions.",
        resolve: |raw: Option<String>| Ok::<_, OrbitError>(raw),
    },
    routines_role: Option<String> => String {
        key: "routines.role", value_type: "string",
        description: "Opt-in for the routine scheduler; the only supported value is 'source' (marks this workspace as a routine source for `orbit sweep`).",
        resolve: |raw: Option<String>| resolve_optional_choice(raw, "routines.role", &["source"]),
    },
    runtime_log_max_file_mb: u64 => u64 {
        key: "runtime.log_max_file_mb", value_type: "integer",
        description: "Roll the active JSONL log once it grows past this many MiB (must be >= 1 and <= runtime.log_max_total_mb).",
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_file_bytes / (1024 * 1024))),
    },
    runtime_log_max_total_mb: u64 => u64 {
        key: "runtime.log_max_total_mb", value_type: "integer",
        description: "Total size budget (MiB) across JSONL log archives; oldest are pruned first when exceeded (must be >= 1).",
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_total_bytes / (1024 * 1024))),
    },
    runtime_log_retention_days: u64 => u64 {
        key: "runtime.log_retention_days", value_type: "integer",
        description: "Delete JSONL log archives whose mtime is older than this many days (must be >= 1).",
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().retention_days)),
    },
    scoring_enabled: bool => bool {
        key: "scoring.enabled", value_type: "bool",
        description: "Whether scoreboard metrics are recorded for task runs.",
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    tasks_id_start: Option<u32> => u32 {
        key: "tasks.id_start", value_type: "integer",
        description: "Floor for the local task-id allocator on this machine (forward-only; lets machines hold disjoint id ranges).",
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw),
    },
    workflow_auto_ship: bool => bool {
        key: "workflow.auto_ship", value_type: "bool",
        description: "Opt-in for unattended ship dispatch via the routine/sweep scheduler.",
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_base_branch: String => String {
        key: "workflow.base_branch", value_type: "string",
        description: "Default base branch for ship workflows.",
        resolve: |raw: Option<String>| resolve_non_empty(raw, DEFAULT_WORKFLOW_BASE_BRANCH, "workflow.base_branch"),
    },
    workflow_default_crew: Option<String> => String {
        key: "workflow.default_crew", value_type: "string",
        description: "Named crew used when a task does not declare `crew` and no CLI override is given.",
        resolve: |raw: Option<String>| resolve_optional_non_empty(raw, "workflow.default_crew"),
    },
    workflow_system_crew: String => String {
        key: "workflow.system_crew", value_type: "string",
        description: "Named crew used by system activities such as step-failure recovery and failed-run triage.",
        resolve: |raw: Option<String>| resolve_non_empty(raw, DEFAULT_WORKFLOW_SYSTEM_CREW, "workflow.system_crew"),
    },
}

impl ConfigSnapshot {
    fn finish_admission(
        &mut self,
        crews: &BTreeMap<String, Crew>,
        env_default: Option<&str>,
    ) -> Result<(), OrbitError> {
        LogRotationConfig::from_parts(
            Some(self.runtime_log_retention_days),
            Some(self.runtime_log_max_total_mb),
            Some(self.runtime_log_max_file_mb),
        )?;
        self.workflow_default_crew =
            resolve_default_crew(self.workflow_default_crew.take(), crews, env_default)?;
        Ok(())
    }
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        let document = toml::Value::Table(toml::map::Map::new());
        ConfigSnapshot::admit_with_env(
            &document,
            Path::new("<built-in defaults>"),
            &default_admission_crews(),
            None,
        )
        .unwrap_or_else(|error| panic!("built-in configuration defaults must admit: {error}"))
    }
}

fn default_admission_crews() -> BTreeMap<String, Crew> {
    BTreeMap::from([(
        DEFAULT_WORKFLOW_CREW.to_string(),
        Crew {
            name: DEFAULT_WORKFLOW_CREW.to_string(),
            assignment: CrewAssignment {
                model: String::new(),
                provider: "claude".to_string(),
                effort: None,
            },
            description: None,
            tags: Vec::new(),
        },
    )])
}

/// Look up one registry key's metadata.
pub fn describe(key: &str) -> Option<&'static ConfigKeyDescriptor> {
    CONFIG_KEY_REGISTRY.iter().find(|entry| entry.key == key)
}

/// Every settable key name, used for did-you-mean suggestions.
pub(crate) fn all_key_names() -> Vec<String> {
    CONFIG_KEY_REGISTRY
        .iter()
        .map(|entry| entry.key.to_string())
        .collect()
}

fn read_optional<T: DeserializeOwned>(
    document: &toml::Value,
    key: &str,
    config_path: &Path,
) -> Result<Option<T>, OrbitError> {
    let mut value = document;
    for segment in key.split('.') {
        let table = value.as_table().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': table path for '{key}' contains a non-table value",
                redact_home_dir(&config_path.display().to_string())
            ))
        })?;
        let Some(next) = table.get(segment) else {
            return Ok(None);
        };
        value = next;
    }
    value.clone().try_into().map(Some).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': invalid value for '{key}': {error}",
            redact_home_dir(&config_path.display().to_string())
        ))
    })
}

fn resolve_choice(
    raw: Option<String>,
    default: &str,
    key: &str,
    choices: &[&str],
) -> Result<String, OrbitError> {
    let value = raw.as_deref().unwrap_or(default).trim();
    if choices.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value '{value}'; expected one of: {}",
            choices.join(", ")
        )))
    }
}

fn resolve_optional_choice(
    raw: Option<String>,
    key: &str,
    choices: &[&str],
) -> Result<Option<String>, OrbitError> {
    raw.map(|value| resolve_choice(Some(value), "", key, choices))
        .transpose()
}

fn resolve_non_empty(raw: Option<String>, default: &str, key: &str) -> Result<String, OrbitError> {
    let value = raw.as_deref().unwrap_or(default).trim();
    if value.is_empty() {
        Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
    } else {
        Ok(value.to_string())
    }
}

fn resolve_optional_non_empty(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    raw.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
        } else {
            Ok(trimmed.to_string())
        }
    })
    .transpose()
}

pub(crate) fn resolve_default_crew(
    configured: Option<String>,
    crews: &BTreeMap<String, Crew>,
    env_default: Option<&str>,
) -> Result<Option<String>, OrbitError> {
    let selected = if let Some(configured) = configured.filter(|value| !value.trim().is_empty()) {
        Some(configured)
    } else if let Some(raw_env) = env_default.filter(|value| !value.trim().is_empty()) {
        let provider = Provider::parse(raw_env).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{CONSTELLATION_DEFAULT_PROVIDER_ENV} has invalid value: {error}"
            ))
        })?;
        let preferred = match provider.as_str() {
            "claude" => "opus",
            "codex" => "sol",
            provider => provider,
        };
        Some(if crews.contains_key(preferred) {
            preferred.to_string()
        } else {
            provider.as_str().to_string()
        })
    } else {
        None
    };
    if let Some(selected) = selected {
        resolve_crew(&selected, crews)?;
        return Ok(Some(selected));
    }
    if crews.contains_key(DEFAULT_WORKFLOW_CREW) {
        return Ok(Some(DEFAULT_WORKFLOW_CREW.to_string()));
    }
    if crews.contains_key(LEGACY_DEFAULT_WORKFLOW_CREW) {
        return Ok(Some(LEGACY_DEFAULT_WORKFLOW_CREW.to_string()));
    }
    if crews.is_empty() {
        return Ok(None);
    }
    Err(OrbitError::InvalidInput(format!(
        "[workflow].default_crew must be set when defining [crews.*]; choose one of: {}",
        crews.keys().cloned().collect::<Vec<_>>().join(", ")
    )))
}

fn default_log_rotation() -> LogRotationConfig {
    LogRotationConfig::default()
}

fn default_pass_list() -> Vec<String> {
    #[allow(unused_mut)]
    let mut vars = vec!["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER"];
    #[cfg(target_os = "macos")]
    vars.push("__CF_USER_TEXT_ENCODING");
    vars.into_iter().map(ToString::to_string).collect()
}

fn normalize_pass_list(pass: Vec<String>) -> Result<Vec<String>, OrbitError> {
    let mut normalized = BTreeSet::new();
    for entry in pass {
        let value = entry.trim();
        let mut chars = value.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
        if !valid {
            return Err(OrbitError::InvalidInput(format!(
                "execution.env.pass contains invalid variable name '{value}'"
            )));
        }
        normalized.insert(value.to_string());
    }
    Ok(normalized.into_iter().collect())
}
