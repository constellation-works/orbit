//! Admission registry for fixed `config.toml` settings.
//!
//! Each setting is declared once in [`define_config_settings!`]. That row
//! drives TOML extraction, defaulting/validation, `orbit config keys`
//! metadata, the resolved snapshot, and JSON lookup used by `get`/`show`.
//! Runtime consumers read the admitted snapshot instead of re-parsing raw
//! section structs. Removed-key migration guards remain in `raw`/`runtime`.
//! Dynamically named crew tables are not fixed registry rows, but live
//! `crews.<name>.<field>` keys are addressable by `orbit config set`/`get`
//! through [`admit_config_key`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::LogRotationConfig;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{Crew, CrewAssignment, resolve_crew};
use orbit_types::workflow::automation::recovery::DEFAULT_STALL_WINDOW_MINUTES;
use orbit_types::workflow::{CODEX_PROVIDER_SANDBOX_MODES, Provider};

use crate::operation::{
    self, CompletionPreference, DeliveryCap, OperationPreset, PreparationPreference,
    PromotionPreference, RecoveryPreference, ReviewPolicy, admit_choice,
};
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

/// Live `[crews.<name>]` fields addressable as `crews.<name>.<field>`.
///
/// The crew name is not known at compile time, so these are not registry
/// rows. `orbit config keys` still lists only the fixed settings.
pub(crate) const CREW_CONFIG_FIELDS: &[&str] =
    &["description", "effort", "model", "provider", "tags"];

/// One live field on a named crew, as used by `orbit config get`/`set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrewFieldKey<'a> {
    pub name: &'a str,
    pub field: &'a str,
}

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
    automation_stall_window_minutes: u32 => u32 {
        key: "automation.stall_window_minutes", value_type: "integer",
        description: "Minutes a deferred delivery-automation reason may persist before the evaluator logs it at warn and files one friction (1..=1440).",
        resolve: |raw: Option<u32>| resolve_bounded_minutes(raw, DEFAULT_STALL_WINDOW_MINUTES, "automation.stall_window_minutes"),
    },
    codex_approval_policy: Option<String> => String {
        key: "execution.codex.approval_policy", value_type: "string",
        description: "Codex approval policy: one of untrusted, on-request, never.",
        resolve: |raw: Option<String>| resolve_optional_choice(raw, "execution.codex.approval_policy", &["untrusted", "on-request", "never"]),
    },
    codex_sandbox: String => String {
        key: "execution.codex.sandbox", value_type: "string",
        description: "Codex sandbox mode: one of read-only, workspace-write, danger-full-access.",
        resolve: |raw: Option<String>| resolve_choice(raw, "workspace-write", "execution.codex.sandbox", CODEX_PROVIDER_SANDBOX_MODES),
    },
    execution_env_pass: Vec<String> => Vec<String> {
        key: "execution.env.pass", value_type: "array<string>",
        description: "Environment variable names allow-listed for passthrough into agent subprocesses.",
        resolve: |raw: Option<Vec<String>>| raw.map(normalize_pass_list).unwrap_or_else(|| Ok(default_pass_list())),
    },
    operation_completion: Option<String> => String {
        key: "operation.completion", value_type: "string",
        description: "Operation-mode completion preference: review or done. Preset-managed; bounded by operation.delivery_cap and the grant.",
        resolve: |raw: Option<String>| admit_choice::<CompletionPreference>(raw, CompletionPreference::as_str),
    },
    operation_delivery_cap: Option<String> => String {
        key: "operation.delivery_cap", value_type: "string",
        description: "Repository ceiling on managed delivery: review (default) or done. Independent of the preset.",
        resolve: |raw: Option<String>| admit_choice::<DeliveryCap>(raw, DeliveryCap::as_str),
    },
    operation_leaf_ceiling: Option<u32> => u32 {
        key: "operation.leaf_ceiling", value_type: "integer",
        description: "Operation-mode ceiling on concurrently live leaf runs (1..=500). Preset-managed; the job's hard limit still applies.",
        resolve: |raw: Option<u32>| operation::leaf_ceiling(raw),
    },
    operation_preparation: Option<String> => String {
        key: "operation.preparation", value_type: "string",
        description: "Operation-mode preparation preference: manual or automatic. Preset-managed.",
        resolve: |raw: Option<String>| admit_choice::<PreparationPreference>(raw, PreparationPreference::as_str),
    },
    operation_preparation_due_seconds: Option<u64> => u64 {
        key: "operation.preparation_due_seconds", value_type: "integer",
        description: "Seconds after a material change before an in-grant task's preparation is due (1..=86400). Preset-managed.",
        resolve: |raw: Option<u64>| operation::preparation_due_seconds(raw),
    },
    operation_preset: Option<String> => String {
        key: "operation.preset", value_type: "string",
        description: "Operation-mode preset: supervised (default) or autonomous. Selecting a preset resets the preset-managed operation.* fields at that layer. Grants nothing by itself.",
        resolve: |raw: Option<String>| admit_choice::<OperationPreset>(raw, OperationPreset::as_str),
    },
    operation_promotion: Option<String> => String {
        key: "operation.promotion", value_type: "string",
        description: "Operation-mode promotion preference: separate_approval or automatic. Preset-managed; automatic promotion still needs a grant with the promote right.",
        resolve: |raw: Option<String>| admit_choice::<PromotionPreference>(raw, PromotionPreference::as_str),
    },
    operation_recovery: Option<String> => String {
        key: "operation.recovery", value_type: "string",
        description: "Operation-mode recovery preference: existing or scheduled. Preset-managed.",
        resolve: |raw: Option<String>| admit_choice::<RecoveryPreference>(raw, RecoveryPreference::as_str),
    },
    operation_recovery_episodes_per_task: Option<u32> => u32 {
        key: "operation.recovery_episodes_per_task", value_type: "integer",
        description: "Aggregate recovery episodes allowed per task inside a grant (0..=10). Preset-managed.",
        resolve: |raw: Option<u32>| operation::recovery_episodes_per_task(raw),
    },
    operation_recovery_minutes_per_task: Option<u32> => u32 {
        key: "operation.recovery_minutes_per_task", value_type: "integer",
        description: "Aggregate recovery wall-time minutes allowed per task inside a grant (1..=1440). Preset-managed.",
        resolve: |raw: Option<u32>| operation::recovery_minutes_per_task(raw),
    },
    operation_review_crew: Option<String> => String {
        key: "operation.review_crew", value_type: "string",
        description: "Crew selected for before-PR automatic review. Independent of the preset. After-landing review runs from its delivery auto-task and uses that definition's template crew.",
        resolve: |raw: Option<String>| operation::review_crew(raw),
    },
    operation_review_minutes: Option<u32> => u32 {
        key: "operation.review_minutes", value_type: "integer",
        description: "Aggregate before-PR reviewer, repair, and final-validation wall-time minutes per delivery candidate lineage (1..=1440, default 30). Independent of the preset.",
        resolve: |raw: Option<u32>| operation::review_minutes(raw),
    },
    operation_review_policy: Option<String> => String {
        key: "operation.review_policy", value_type: "string",
        description: "Automatic review timing: none (default), before-pr, or after-landing. Independent of the preset. before-pr holds PR creation for a fresh reviewer on the PR route and is refused for local-only delivery.",
        resolve: |raw: Option<String>| admit_choice::<ReviewPolicy>(raw, ReviewPolicy::as_str),
    },
    operation_review_repair_cycles: Option<u32> => u32 {
        key: "operation.review_repair_cycles", value_type: "integer",
        description: "Reviewer repair/validation cycles allowed per delivery candidate lineage (0..=10, default 2). Independent of the preset.",
        resolve: |raw: Option<u32>| operation::review_repair_cycles(raw),
    },
    operation_review_reviewer_starts: Option<u32> => u32 {
        key: "operation.review_reviewer_starts", value_type: "integer",
        description: "Fresh reviewer invocations allowed per delivery candidate lineage, including retries and invalidations (1..=10, default 2). Independent of the preset.",
        resolve: |raw: Option<u32>| operation::review_reviewer_starts(raw),
    },
    pr_task_url_template: Option<String> => String {
        key: "pr.task_url_template", value_type: "string",
        description: "URL template used to link a task ID in PR descriptions.",
        resolve: |raw: Option<String>| Ok::<_, OrbitError>(raw),
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
    workflow_hard_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.hard_complexity_crews", value_type: "array<string>",
        description: "Random crew pool for unassigned hard-complexity tasks in auto drains; empty disables the pool.",
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_low_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.low_complexity_crews", value_type: "array<string>",
        description: "Random crew pool for unassigned low-complexity tasks in auto drains; empty disables the pool.",
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_medium_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.medium_complexity_crews", value_type: "array<string>",
        description: "Random crew pool for unassigned medium-complexity tasks in auto drains; empty disables the pool.",
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
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
        self.workflow_low_complexity_crews = crate::canonical_crew_pool(
            &self.workflow_low_complexity_crews,
            crews,
            "workflow.low_complexity_crews",
        )?;
        self.workflow_medium_complexity_crews = crate::canonical_crew_pool(
            &self.workflow_medium_complexity_crews,
            crews,
            "workflow.medium_complexity_crews",
        )?;
        self.workflow_hard_complexity_crews = crate::canonical_crew_pool(
            &self.workflow_hard_complexity_crews,
            crews,
            "workflow.hard_complexity_crews",
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

/// Admit a dotted key for `orbit config get`/`set`.
///
/// Fixed registry keys and live `crews.<name>.<field>` keys succeed. Unknown
/// registry keys and misspelled crew fields fail with suggestions before any
/// document mutation.
pub fn admit_config_key(key: &str) -> Result<(), OrbitError> {
    if describe(key).is_some() {
        return Ok(());
    }
    match parse_crew_field_key(key)? {
        Some(_) => Ok(()),
        None => Err(OrbitError::invalid_input_with_suggestions(
            format!("unknown config key '{key}'"),
            all_key_names(),
        )),
    }
}

/// Parse `crews.<name>.<field>` when `key` is a crew-table path.
///
/// `None` means this is not a crew key (including the bare `crews` table).
/// An ill-formed crew path or unknown field is an error, not a fallthrough
/// to the fixed-key registry, so `crews.sol.effrot` is not reported as an
/// unknown registry setting.
pub(crate) fn parse_crew_field_key(key: &str) -> Result<Option<CrewFieldKey<'_>>, OrbitError> {
    let mut parts = key.split('.');
    if parts.next() != Some("crews") {
        return Ok(None);
    }
    let Some(name) = parts.next() else {
        return Ok(None);
    };
    let Some(field) = parts.next() else {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' is missing a field"
        )));
    };
    if parts.next().is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' has extra segments"
        )));
    }
    if name.is_empty() {
        return Err(OrbitError::InvalidInput(
            "crew config keys require a non-empty crew name".to_string(),
        ));
    }
    if field.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' is missing a field"
        )));
    }
    if CREW_CONFIG_FIELDS.contains(&field) {
        return Ok(Some(CrewFieldKey { name, field }));
    }
    Err(OrbitError::invalid_input_with_suggestions(
        format!("unknown crew field '{field}' in '{key}'"),
        CREW_CONFIG_FIELDS
            .iter()
            .map(|known| format!("crews.{name}.{known}"))
            .collect(),
    ))
}

/// Every settable key name, used for did-you-mean suggestions.
pub(crate) fn all_key_names() -> Vec<String> {
    CONFIG_KEY_REGISTRY
        .iter()
        .map(|entry| entry.key.to_string())
        .collect()
}

pub(crate) fn read_optional<T: DeserializeOwned>(
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

/// Admit a positive minute budget, defaulting when unset. A day is the
/// ceiling: anything longer is indistinguishable from never escalating.
fn resolve_bounded_minutes(raw: Option<u32>, default: u32, key: &str) -> Result<u32, OrbitError> {
    const MAX_MINUTES: u32 = 1440;
    match raw {
        Some(value) if value == 0 || value > MAX_MINUTES => Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value {value}; expected 1..={MAX_MINUTES}"
        ))),
        Some(value) => Ok(value),
        None => Ok(default),
    }
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
