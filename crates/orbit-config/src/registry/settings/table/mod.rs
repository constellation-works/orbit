use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::lock_mode::FootprintLockModeEntry;
use orbit_types::identity::Crew;
use orbit_types::workflow::automation::members::{MaterialField, SourceSensitivity};
use orbit_types::workflow::automation::recovery::DEFAULT_STALL_WINDOW_MINUTES;
use orbit_types::workflow::{
    CODEX_PROVIDER_SANDBOX_MODES, DEFAULT_REVIEW_MINUTES, MissingSectionPolicy, ScreenshotSection,
};
use serde_json::{Value as JsonValue, json};

use super::super::{
    CODEX_APPROVAL_POLICIES, CONSTELLATION_DEFAULT_PROVIDER_ENV, ConfigKeyDescriptor,
    ConfigKeyTier, ConfigSection, DEFAULT_WORKFLOW_BASE_BRANCH, SECURITY_ALERT_SEVERITIES,
};
use super::resolve::{
    DEFAULT_JOURNAL_RETENTION_DAYS, DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES, DEFAULT_RETENTION_DAYS,
    DEFAULT_STORE_WARN_BYTES, DEFAULT_WORKER_MEMORY_HIGH, DEFAULT_WORKER_MEMORY_MAX,
    default_log_rotation, default_pass_list, normalize_logins, normalize_pass_list, read_optional,
    resolve_agent_stall_minutes, resolve_at, resolve_bounded_count, resolve_bounded_minutes,
    resolve_bounded_range, resolve_branch_template, resolve_build_output_env, resolve_choice,
    resolve_commit_subject_template, resolve_cpu_light_leaves, resolve_distributed_completion,
    resolve_generated_paths, resolve_host_shutdown_hold_minutes, resolve_light_delivery_paths,
    resolve_machine_id, resolve_machine_labels, resolve_machine_name, resolve_material_fields,
    resolve_memory_limit, resolve_mid_run_steering_max_rounds, resolve_name_list,
    resolve_non_empty, resolve_optional_choice, resolve_optional_non_empty, resolve_percent,
    resolve_pr_body_template, resolve_reclaim, resolve_retention_days, resolve_task_prefix,
    resolve_task_tag_labels, resolve_task_type_labels, resolve_text_template,
    resolve_unlocked_context_paths, resolve_unlocked_path_validation_commands,
    resolve_validation_path_mode, resolve_worker_tasks_max,
};
use crate::crew_performance::CrewPerformanceType;
use crate::lock_modes::RawLockModeEntry;
use crate::memory_limit::MemoryLimit;
use crate::operation;

macro_rules! define_config_settings {
    ($(
        $field:ident : $resolved:ty => $raw:ty {
            key: $key:literal,
            value_type: $value_type:literal,
            description: $description:literal,
            section: $section:expr,
            order: $order:literal,
            $(tier: $tier:expr,)?
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
                section: $section,
                order: $order,
                tier: define_config_settings!(@tier $($tier)?),
            },)+
        ];

        impl ConfigSnapshot {
            pub(crate) fn admit(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
            ) -> Result<Self, OrbitError> {
                let env_default = std::env::var(CONSTELLATION_DEFAULT_PROVIDER_ENV).ok();
                Self::admit_with_env(document, config_path, crews, env_default.as_deref(), true)
            }

            /// A file view validates explicit references without requiring a
            /// runtime default-crew selection from this single layer.
            pub(crate) fn admit_scoped(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
            ) -> Result<Self, OrbitError> {
                let env_default = std::env::var(CONSTELLATION_DEFAULT_PROVIDER_ENV).ok();
                Self::admit_internal(
                    document,
                    config_path,
                    crews,
                    env_default.as_deref(),
                    false,
                    false,
                    None,
                )
            }

            pub(crate) fn admit_with_env(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
                env_default: Option<&str>,
                require_default_crew: bool,
            ) -> Result<Self, OrbitError> {
                Self::admit_internal(
                    document,
                    config_path,
                    crews,
                    env_default,
                    require_default_crew,
                    true,
                    None,
                )
            }

            /// Admit a merged layer using the global resource-throttle memory
            /// high-water mark for the host-wide auto-concurrency check.
            pub(crate) fn admit_with_global_memory_high(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
                global_memory_high_percent: u8,
            ) -> Result<Self, OrbitError> {
                let env_default = std::env::var(CONSTELLATION_DEFAULT_PROVIDER_ENV).ok();
                Self::admit_internal(
                    document,
                    config_path,
                    crews,
                    env_default.as_deref(),
                    true,
                    true,
                    Some(global_memory_high_percent),
                )
            }

            /// Admit the settings supplied by a workspace layer. The machine
            /// table is refused separately, so there is no host-wide memory
            /// threshold in this scoped view to compare against.
            pub(crate) fn admit_workspace(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
            ) -> Result<Self, OrbitError> {
                let env_default = std::env::var(CONSTELLATION_DEFAULT_PROVIDER_ENV).ok();
                Self::admit_internal(
                    document,
                    config_path,
                    crews,
                    env_default.as_deref(),
                    true,
                    false,
                    None,
                )
            }

            fn admit_internal(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
                env_default: Option<&str>,
                require_default_crew: bool,
                validate_auto_memory: bool,
                global_memory_high_percent: Option<u8>,
            ) -> Result<Self, OrbitError> {
                $(let $field: $resolved = {
                    let raw_value: Option<$raw> = read_optional(document, $key, config_path)?;
                    resolve_at(($resolve)(raw_value), config_path)?
                };)+
                let mut snapshot = Self {
                    execution_env_inherit: false,
                    $($field,)+
                };
                snapshot.finish_admission(crews, env_default, require_default_crew)?;
                if validate_auto_memory {
                    snapshot.auto_concurrency().validate_memory_high_percent(
                        global_memory_high_percent
                            .unwrap_or(snapshot.workflow_resource_throttle_memory_high_percent),
                    )?;
                }
                Ok(snapshot)
            }

            /// Admit one lower-layer setting in the effective snapshot's
            /// context. Other workspace settings may supply constraints or
            /// repair unrelated global values, so admitting the whole global
            /// document alone would not predict removal of just this key.
            pub(crate) fn admit_override(
                &self,
                document: &toml::Value,
                key: &str,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
            ) -> Result<Option<JsonValue>, OrbitError> {
                let mut snapshot = self.clone();
                match key {
                    $($key => {
                        let raw_value: Option<$raw> = read_optional(document, $key, config_path)?;
                        snapshot.$field = resolve_at(($resolve)(raw_value), config_path)?;
                    },)+
                    _ => return Ok(None),
                }
                snapshot.finish_admission(crews, None, false)?;
                Ok(snapshot.value_for(key))
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
    (@tier $tier:expr) => { $tier };
    (@tier) => { ConfigKeyTier::Standard };
}

// The key list is split across two fragments. They are `.inc` files because they
// are token fragments, not modules, so the orphan-module guard must not expect
// a `mod` declaration. `with_tail_keys!` is defined by `keys_tail.inc`, so it is
// included before the head.
include!("keys_tail.inc");
include!("keys_head.inc");
