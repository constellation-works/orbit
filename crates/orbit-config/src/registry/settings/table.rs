use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::workflow::automation::members::{MaterialField, SourceSensitivity};
use orbit_types::workflow::automation::recovery::DEFAULT_STALL_WINDOW_MINUTES;
use orbit_types::workflow::{CODEX_PROVIDER_SANDBOX_MODES, DEFAULT_REVIEW_MINUTES};
use serde_json::{Value as JsonValue, json};

use super::super::{
    CODEX_APPROVAL_POLICIES, CONSTELLATION_DEFAULT_PROVIDER_ENV, ConfigKeyDescriptor,
    ConfigKeyTier, ConfigSection, DEFAULT_WORKFLOW_BASE_BRANCH, SECURITY_ALERT_SEVERITIES,
};
use super::resolve::{
    DEFAULT_JOURNAL_RETENTION_DAYS, DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES, DEFAULT_RETENTION_DAYS,
    DEFAULT_STORE_WARN_BYTES, DEFAULT_WORKER_MEMORY_HIGH, DEFAULT_WORKER_MEMORY_MAX,
    default_log_rotation, default_pass_list, normalize_logins, normalize_pass_list, read_optional,
    resolve_agent_stall_minutes, resolve_bounded_count, resolve_bounded_minutes,
    resolve_bounded_range, resolve_build_output_env, resolve_choice, resolve_cpu_light_leaves,
    resolve_distributed_completion, resolve_generated_paths, resolve_host_shutdown_hold_minutes,
    resolve_light_delivery_paths, resolve_machine_id, resolve_machine_name,
    resolve_material_fields, resolve_memory_limit, resolve_mid_run_steering_max_rounds,
    resolve_non_empty, resolve_optional_choice, resolve_optional_non_empty, resolve_percent,
    resolve_reclaim, resolve_retention_days, resolve_task_prefix, resolve_unlocked_context_paths,
    resolve_unlocked_path_validation_commands, resolve_validation_path_mode,
    resolve_worker_tasks_max,
};
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
                    ($resolve)(raw_value)?
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
                        snapshot.$field = ($resolve)(raw_value)?;
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

define_config_settings! {
    ci_failure_operator_suppression_hours: u32 => u32 {
        key: "ci_failure.operator_suppression_hours", value_type: "integer",
        description: "Hours an archived or rejected CI sweep task suppresses re-filing.",
        section: ConfigSection::Housekeeping, order: 21,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| {
            let hours = raw.unwrap_or(6);
            if hours > 720 {
                Err(OrbitError::InvalidInput("ci_failure.operator_suppression_hours must be 0..=720".into()))
            } else { Ok(hours) }
        },
    },
    automation_stall_window_minutes: u32 => u32 {
        key: "automation.stall_window_minutes", value_type: "integer",
        description: "Minutes a deferred delivery-automation reason may persist before it is logged and filed.",
        section: ConfigSection::Housekeeping, order: 20,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_minutes(raw, DEFAULT_STALL_WINDOW_MINUTES, "automation.stall_window_minutes"),
    },
    codex_approval_policy: Option<String> => String {
        key: "execution.codex.approval_policy", value_type: "string",
        description: "Codex approval policy: untrusted, on-request, or never.",
        section: ConfigSection::Execution, order: 20,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_optional_choice(raw, "execution.codex.approval_policy", CODEX_APPROVAL_POLICIES),
    },
    codex_sandbox: String => String {
        key: "execution.codex.sandbox", value_type: "string",
        description: "Codex sandbox mode: read-only, workspace-write, or danger-full-access.",
        section: ConfigSection::Execution, order: 10,
        resolve: |raw: Option<String>| resolve_choice(raw, "workspace-write", "execution.codex.sandbox", CODEX_PROVIDER_SANDBOX_MODES),
    },
    execution_env_pass: Vec<String> => Vec<String> {
        key: "execution.env.pass", value_type: "array<string>",
        description: "Environment variable names passed through into agent subprocesses.",
        section: ConfigSection::Execution, order: 30,
        resolve: |raw: Option<Vec<String>>| raw.map(normalize_pass_list).unwrap_or_else(|| Ok(default_pass_list())),
    },
    execution_proc_spawn_max_timeout_minutes: u32 => u32 {
        key: "execution.proc_spawn_max_timeout_minutes", value_type: "integer",
        description: "Longest timeout, in minutes, one proc.spawn call may use inside an activity.",
        section: ConfigSection::Execution, order: 40,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_minutes(raw, DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES, "execution.proc_spawn_max_timeout_minutes"),
    },
    execution_agent_stall_minutes: u32 => u32 {
        key: "execution.agent_stall_minutes", value_type: "integer",
        description: "Minutes a provider process may sit idle before its step fails as stalled.",
        section: ConfigSection::Execution, order: 50,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_agent_stall_minutes(raw),
    },
    machine_id: Option<String> => String {
        key: "machine.id", value_type: "string",
        description: "Stable generated identity of this machine, written once by orbit init.",
        section: ConfigSection::Machine, order: 10,
        resolve: |raw: Option<String>| resolve_machine_id(raw),
    },
    machine_auto_concurrency_admit_cpu_percent: u8 => u8 {
        key: "machine.auto_concurrency.admit_cpu_percent", value_type: "integer",
        description: "Maximum forecast CPU utilization as a percentage of host cores.",
        section: ConfigSection::Machine, order: 33,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "machine.auto_concurrency.admit_cpu_percent"),
    },
    machine_auto_concurrency_admit_memory_percent: u8 => u8 {
        key: "machine.auto_concurrency.admit_memory_percent", value_type: "integer",
        description: "Maximum forecast non-reclaimable memory as a percentage of host memory.",
        section: ConfigSection::Machine, order: 34,
        resolve: |raw: Option<u8>| resolve_percent(raw, 75, "machine.auto_concurrency.admit_memory_percent"),
    },
    machine_auto_concurrency_head_max_wait_minutes: u32 => u32 {
        key: "machine.auto_concurrency.head_max_wait_minutes", value_type: "integer",
        description: "Starvation guard: maximum minutes the queue head may wait.",
        section: ConfigSection::Machine, order: 38,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(30)),
    },
    machine_auto_concurrency_head_stuck_passes: u32 => u32 {
        key: "machine.auto_concurrency.head_stuck_passes", value_type: "integer",
        description: "Starvation guard: protected passes before a stuck queue head is reported.",
        section: ConfigSection::Machine, order: 39,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| match raw.unwrap_or(20) {
            0 => Err(OrbitError::InvalidInput("machine.auto_concurrency.head_stuck_passes must be at least 1".into())),
            passes => Ok(passes),
        },
    },
    machine_auto_concurrency_horizon_cap_minutes: u32 => u32 {
        key: "machine.auto_concurrency.horizon_cap_minutes", value_type: "integer",
        description: "Maximum forecast horizon for one candidate, in minutes.",
        section: ConfigSection::Machine, order: 37,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(120)),
    },
    machine_auto_concurrency_min_profile_deliveries: u32 => u32 {
        key: "machine.auto_concurrency.min_profile_deliveries", value_type: "integer",
        description: "Minimum completed deliveries before a profile is used without fallback.",
        section: ConfigSection::Machine, order: 36,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(10)),
    },
    machine_auto_concurrency_mode: String => String {
        key: "machine.auto_concurrency.mode", value_type: "string",
        description: "Forecast admission mode: off, shadow, or enforce.",
        section: ConfigSection::Machine, order: 32,
        resolve: |raw: Option<String>| resolve_choice(raw, "off", "machine.auto_concurrency.mode", &["off", "shadow", "enforce"]),
    },
    machine_auto_concurrency_profile_window: u32 => u32 {
        key: "machine.auto_concurrency.profile_window", value_type: "integer",
        description: "Most recent completed deliveries retained for each profile key.",
        section: ConfigSection::Machine, order: 35,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(50)),
    },
    machine_host_shutdown_hold_minutes: u32 => u32 {
        key: "machine.host_shutdown_hold_minutes", value_type: "integer",
        description: "Minutes before a scheduled shutdown when new unattended admissions hold.",
        section: ConfigSection::Machine, order: 31,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_host_shutdown_hold_minutes(raw),
    },
    machine_name: Option<String> => String {
        key: "machine.name", value_type: "string",
        description: "Operator-chosen display name for this machine.",
        section: ConfigSection::Machine, order: 20,
        resolve: |raw: Option<String>| resolve_machine_name(raw),
    },
    machine_task_prefix: Option<String> => String {
        key: "machine.task_prefix", value_type: "string",
        description: "Task-id namespace for ids minted on this machine, chosen once and not settable.",
        section: ConfigSection::Machine, order: 30,
        resolve: |raw: Option<String>| resolve_task_prefix(raw),
    },
    machine_worker_containment: String => crate::aliases::WorkerContainmentSetting {
        key: "machine.worker_containment", value_type: "string",
        description: "How detached pipeline workers are bounded: warn, strict, or off.",
        section: ConfigSection::Machine, order: 40,
        resolve: |raw: Option<crate::aliases::WorkerContainmentSetting>| crate::aliases::resolve_worker_containment(raw).map(|mode| mode.as_str().to_string()),
    },
    machine_worker_memory_high: MemoryLimit => String {
        key: "machine.worker_memory_high", value_type: "string",
        description: "MemoryHigh for each contained worker scope, where throttling starts.",
        section: ConfigSection::Machine, order: 50,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_memory_limit(raw, DEFAULT_WORKER_MEMORY_HIGH, "machine.worker_memory_high"),
    },
    machine_worker_memory_max: MemoryLimit => String {
        key: "machine.worker_memory_max", value_type: "string",
        description: "MemoryMax for each contained worker scope, where the kernel may OOM-kill.",
        section: ConfigSection::Machine, order: 60,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_memory_limit(raw, DEFAULT_WORKER_MEMORY_MAX, "machine.worker_memory_max"),
    },
    machine_worker_tasks_max: u32 => u32 {
        key: "machine.worker_tasks_max", value_type: "integer",
        description: "TasksMax for each contained worker scope, capping processes and threads.",
        section: ConfigSection::Machine, order: 70,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_worker_tasks_max(raw),
    },
    machine_worker_cpu_quota: u32 => u32 {
        key: "machine.worker_cpu_quota", value_type: "integer",
        description: "CPUQuota for each contained worker scope, as a percentage of one core.",
        section: ConfigSection::Machine, order: 75,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(0)),
    },
    security_alert_sweep_min_severity: String => String {
        key: "security_alert_sweep.min_severity", value_type: "string",
        description: "Minimum Dependabot and code-scanning severity the security sweep files.",
        section: ConfigSection::Housekeeping, order: 90,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_choice(raw, "moderate", "security_alert_sweep.min_severity", SECURITY_ALERT_SEVERITIES),
    },
    pr_close_on_terminal: bool => bool {
        key: "pr.close_on_terminal", value_type: "bool",
        description: "Close a task's open Orbit-authored pull requests when the task ends.",
        section: ConfigSection::Delivery, order: 140,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    pr_delivery_authors: Vec<String> => Vec<String> {
        key: "pr.delivery_authors", value_type: "array<string>",
        description: "Forge logins whose pull requests count as Orbit-authored.",
        section: ConfigSection::Delivery, order: 141,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(normalize_logins(raw.unwrap_or_default())),
    },
    pr_task_url_template: Option<String> => String {
        key: "pr.task_url_template", value_type: "string",
        description: "URL template used to link a task id in pull-request descriptions.",
        section: ConfigSection::Housekeeping, order: 70,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| Ok::<_, OrbitError>(raw),
    },
    review_timing: String => String {
        key: "review.timing", value_type: "string",
        description: "When a fresh reviewer runs before landing: off, before_pr, or before_landing.",
        section: ConfigSection::Review, order: 10,
        resolve: |raw: Option<String>| resolve_choice(raw, "off", "review.timing", operation::ReviewTimingSetting::CHOICES),
    },
    review_before_landing_hosts: Vec<String> => Vec<String> {
        key: "review.before_landing_hosts", value_type: "array<string>",
        description: "Machine ids whose claimed leaves review their open pull request before landing.",
        section: ConfigSection::Review, order: 16,
        resolve: |raw: Option<Vec<String>>| operation::before_landing_hosts(raw).map(Option::unwrap_or_default),
    },
    review_minutes: u32 => u32 {
        key: "review.minutes", value_type: "integer",
        description: "Reviewer runtime minutes for one candidate's review before landing.",
        section: ConfigSection::Review, order: 20,
        resolve: |raw: Option<u32>| operation::review_minutes(raw).map(|minutes| minutes.unwrap_or(DEFAULT_REVIEW_MINUTES)),
    },
    review_verify_findings: bool => bool {
        key: "review.verify_findings", value_type: "bool",
        description: "Refuse approval until a review-filed bug has a confirmed verification.",
        section: ConfigSection::Review, order: 40,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    retention_audit_days: u32 => u32 {
        key: "retention.audit_days", value_type: "integer",
        description: "Days orbit gc audit keeps command and run audit rows.",
        section: ConfigSection::Housekeeping, order: 100,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.audit_days", DEFAULT_RETENTION_DAYS),
    },
    retention_runs_days: u32 => u32 {
        key: "retention.runs_days", value_type: "integer",
        description: "Days after a terminal run before orbit gc runs may drop its pipeline state (minimum 30).",
        section: ConfigSection::Housekeeping, order: 110,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.runs_days", DEFAULT_RETENTION_DAYS),
    },
    retention_journal_days: u32 => u32 {
        key: "retention.journal_days", value_type: "integer",
        description: "Days after a task commit is applied before its journal row may be deleted.",
        section: ConfigSection::Housekeeping, order: 112,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.journal_days", DEFAULT_JOURNAL_RETENTION_DAYS),
    },
    retention_store_warn_bytes: u64 => u64 {
        key: "retention.store_warn_bytes", value_type: "integer",
        description: "Store file size, in bytes, above which orbit doctor warns without store-gc.",
        section: ConfigSection::Housekeeping, order: 115,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or(DEFAULT_STORE_WARN_BYTES)),
    },
    worktree_reclaim: Vec<String> => Vec<String> {
        key: "worktree.reclaim", value_type: "array<string>",
        description: "Relative globs of rebuildable paths reclaimed in terminal run worktrees.",
        section: ConfigSection::Housekeeping, order: 120,
        tier: ConfigKeyTier::Advanced,
        resolve: resolve_reclaim,
    },
    worktree_reclaim_below_free_mib: Option<u64> => u64 {
        key: "worktree.reclaim_below_free_mib", value_type: "integer",
        description: "Free mebibytes under the state directory below which admission reclaims paths.",
        section: ConfigSection::Housekeeping, order: 130,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw),
    },
    worktree_parked_reclaim_after_hours: u64 => u64 {
        key: "worktree.parked_reclaim_after_hours", value_type: "integer",
        description: "Hours a someday task stays parked before worktree GC may remove its worktree.",
        section: ConfigSection::Housekeeping, order: 135,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u64>| match raw {
            Some(hours) if hours > 876_000 => Err(OrbitError::InvalidInput(format!(
                "worktree.parked_reclaim_after_hours must be at most 876000, got {hours}"
            ))),
            raw => Ok(raw.unwrap_or(336)),
        },
    },
    review_baseline_commands: Vec<String> => Vec<String> {
        key: "review.baseline_commands", value_type: "array<string>",
        description: "Commands before-PR review may rerun to confirm a failure also fails on the base.",
        section: ConfigSection::Review, order: 30,
        resolve: resolve_unlocked_path_validation_commands,
    },
    runtime_log_max_file_mb: u64 => u64 {
        key: "runtime.log_max_file_mb", value_type: "integer",
        description: "Mebibytes the active operational log may reach before it rolls.",
        section: ConfigSection::Housekeeping, order: 50,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_file_bytes / (1024 * 1024))),
    },
    runtime_log_max_total_mb: u64 => u64 {
        key: "runtime.log_max_total_mb", value_type: "integer",
        description: "Total mebibyte budget for operational log archives before the oldest are pruned.",
        section: ConfigSection::Housekeeping, order: 40,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_total_bytes / (1024 * 1024))),
    },
    runtime_log_retention_days: u64 => u64 {
        key: "runtime.log_retention_days", value_type: "integer",
        description: "Days after which old operational and agent log archives are deleted.",
        section: ConfigSection::Housekeeping, order: 30,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().retention_days)),
    },
    tasks_id_start: Option<u32> => u32 {
        key: "tasks.id_start", value_type: "integer",
        description: "Floor for this machine's local task-id allocator.",
        section: ConfigSection::Housekeeping, order: 60,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw),
    },
    workflow_admission_affinity: String => String {
        key: "workflow.admission_affinity", value_type: "string",
        description: "How far work only the requesting executor can run moves ahead in admission order: band, priority or off.",
        section: ConfigSection::Delivery, order: 66,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_choice(raw, "band", "workflow.admission_affinity", &orbit_types::task::AdmissionAffinityReach::CHOICES),
    },
    workflow_auto_ship: bool => bool {
        key: "workflow.auto_ship", value_type: "bool",
        description: "Opt this workspace in to unattended ship-sweep dispatch.",
        section: ConfigSection::Delivery, order: 40,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_base_branch: String => String {
        key: "workflow.base_branch", value_type: "string",
        description: "Fallback base branch for ship, auto, and the task pilot.",
        section: ConfigSection::Delivery, order: 10,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_non_empty(raw, DEFAULT_WORKFLOW_BASE_BRANCH, "workflow.base_branch"),
    },
    workflow_default_crew: Option<String> => String {
        key: "workflow.default_crew", value_type: "string",
        description: "Crew used when a task names none and no override is given.",
        section: ConfigSection::Delivery, order: 20,
        resolve: |raw: Option<String>| resolve_optional_non_empty(raw, "workflow.default_crew"),
    },
    workflow_distributed_completion: String => String {
        key: "workflow.distributed_completion", value_type: "string",
        description: "How far this owner takes an accepted distributed handoff: review or done.",
        section: ConfigSection::Delivery, order: 65,
        resolve: |raw: Option<String>| resolve_distributed_completion(raw),
    },
    workflow_explore_enabled: bool => bool {
        key: "workflow.explore.enabled", value_type: "bool",
        description: "Let implement and review-repair runs delegate read-only exploration to explorer crews.",
        section: ConfigSection::Delivery, order: 160,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_explore_max_answer_bytes: u32 => u32 {
        key: "workflow.explore.max_answer_bytes", value_type: "integer",
        description: "Largest explorer answer, in bytes, returned to the calling agent.",
        section: ConfigSection::Delivery, order: 165,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_range(raw, 16_384, 167, 262_144, "workflow.explore.max_answer_bytes"),
    },
    workflow_explore_max_calls_per_run: u32 => u32 {
        key: "workflow.explore.max_calls_per_run", value_type: "integer",
        description: "Explorer invocations one implement or review-repair run may submit.",
        section: ConfigSection::Delivery, order: 161,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_count(raw, 6, 100, "workflow.explore.max_calls_per_run"),
    },
    workflow_explore_max_concurrent_per_run: u32 => u32 {
        key: "workflow.explore.max_concurrent_per_run", value_type: "integer",
        description: "Explorer invocations one run may have in flight at once.",
        section: ConfigSection::Delivery, order: 162,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_count(raw, 2, 16, "workflow.explore.max_concurrent_per_run"),
    },
    workflow_explore_timeout_seconds: u32 => u32 {
        key: "workflow.explore.timeout_seconds", value_type: "integer",
        description: "Wall-clock bound, in seconds, of one explorer invocation.",
        section: ConfigSection::Delivery, order: 163,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_count(raw, 600, 3_600, "workflow.explore.timeout_seconds"),
    },
    workflow_generated_paths: BTreeMap<String, String> => BTreeMap<String, String> {
        key: "workflow.generated_paths", value_type: "table<string,string>",
        description: "Generated files and the command that regenerates each one.",
        section: ConfigSection::Delivery, order: 69,
        resolve: resolve_generated_paths,
    },
    workflow_hard_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.hard_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned hard-complexity tasks.",
        section: ConfigSection::Delivery, order: 90,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_low_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.low_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned low-complexity tasks.",
        section: ConfigSection::Delivery, order: 70,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_medium_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.medium_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned medium-complexity tasks.",
        section: ConfigSection::Delivery, order: 80,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_mid_run_steering_enabled: bool => bool {
        key: "workflow.mid_run_steering.enabled", value_type: "bool",
        description: "Deliver operator comments written after a run's steering watermark.",
        section: ConfigSection::Delivery, order: 140,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_mid_run_steering_max_rounds: u8 => u8 {
        key: "workflow.mid_run_steering.max_rounds", value_type: "integer",
        description: "Steering rounds a run may consume before it holds as unconsumed.",
        section: ConfigSection::Delivery, order: 141,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u8>| resolve_mid_run_steering_max_rounds(raw),
    },
    workflow_regression_risk_enabled: bool => bool {
        key: "workflow.regression_risk.enabled", value_type: "bool",
        description: "Mark regression-prone context files in implement and review prompts.",
        section: ConfigSection::Delivery, order: 150,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    workflow_regression_risk_window_days: u32 => u32 {
        key: "workflow.regression_risk.window_days", value_type: "integer",
        description: "Trailing days of delivered tasks a regression-risk mark is computed over.",
        section: ConfigSection::Delivery, order: 151,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_count(raw, 30, 365, "workflow.regression_risk.window_days"),
    },
    workflow_regression_risk_min_support: u32 => u32 {
        key: "workflow.regression_risk.min_support", value_type: "integer",
        description: "Touching tasks a file needs in the window before it can be marked.",
        section: ConfigSection::Delivery, order: 152,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_count(raw, 5, 10_000, "workflow.regression_risk.min_support"),
    },
    workflow_regression_risk_threshold_percent: u8 => u8 {
        key: "workflow.regression_risk.threshold_percent", value_type: "integer",
        description: "Share of a file's touching tasks that regressed, in percent, that marks it.",
        section: ConfigSection::Delivery, order: 153,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u8>| resolve_percent(raw, 40, "workflow.regression_risk.threshold_percent"),
    },
    workflow_regression_risk_max_files: u32 => u32 {
        key: "workflow.regression_risk.max_files", value_type: "integer",
        description: "Most context files one prompt marks, highest regression rate first.",
        section: ConfigSection::Delivery, order: 154,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u32>| resolve_bounded_count(raw, 3, 20, "workflow.regression_risk.max_files"),
    },
    workflow_provider_limit_budgets: Vec<String> => Vec<String> {
        key: "workflow.provider_limit_budgets", value_type: "array<string>",
        description: "Rolling spend budgets for providers that report no usage.",
        section: ConfigSection::Delivery, order: 109,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<Vec<String>>| crate::provider_limit_budget::admit_budgets(raw),
    },
    workflow_provider_limit_explicit_crews: String => String {
        key: "workflow.provider_limit_explicit_crews", value_type: "string",
        description: "Whether a task whose explicit crew is usage-limited waits or redraws.",
        section: ConfigSection::Delivery, order: 108,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| crate::provider_limit::admit_explicit_crews(raw),
    },
    workflow_provider_limit_max_used_pct: u8 => u8 {
        key: "workflow.provider_limit_max_used_pct", value_type: "integer",
        description: "Usage percent at which delivery admission skips a provider-limited crew.",
        section: ConfigSection::Delivery, order: 106,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u8>| resolve_percent(raw, crate::provider_limit::DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT, "workflow.provider_limit_max_used_pct"),
    },
    workflow_provider_limit_overrides: Vec<String> => Vec<String> {
        key: "workflow.provider_limit_overrides", value_type: "array<string>",
        description: "Per-provider usage thresholds that replace the shared percent.",
        section: ConfigSection::Delivery, order: 107,
        resolve: |raw: Option<Vec<String>>| crate::provider_limit::admit_overrides(raw),
    },
    workflow_purpose_crews_auto_fix: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.auto_fix", value_type: "array<string>",
        description: "Weighted crew pool for pull-request auto-fix repairs.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_baseline_red_repair: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.baseline_red_repair", value_type: "array<string>",
        description: "Weighted crew pool for tasks the red-base re-check files.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_explorer: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.explorer", value_type: "array<string>",
        description: "Weighted crew pool for read-only exploration a run delegates.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_failure_recovery: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.failure_recovery", value_type: "array<string>",
        description: "Weighted crew pool for step-failure and pull-request conflict recovery.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_final_recovery: Option<Vec<String>> => Vec<String> {
        key: "workflow.purpose_crews.final_recovery", value_type: "array<string>",
        description: "Weighted crew pool the final-recovery activity draws once per run.",
        section: ConfigSection::Delivery, order: 105,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw),
    },
    workflow_purpose_crews_pre_landing_review: Option<Vec<String>> => operation::ReviewCrewSetting {
        key: "workflow.purpose_crews.pre_landing_review", value_type: "array<string>",
        description: "Crew or pool for before-PR and before-landing review; unset uses review.",
        section: ConfigSection::Review, order: 31,
        resolve: operation::pre_landing_review_crew,
    },
    workflow_purpose_crews_review: Option<Vec<String>> => operation::ReviewCrewSetting {
        key: "workflow.purpose_crews.review", value_type: "array<string>",
        description: "Crew or pool for after-landing review, and before landing by default.",
        section: ConfigSection::Review, order: 30,
        resolve: operation::review_crew,
    },
    workflow_purpose_crews_sweep_tasks: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.sweep_tasks", value_type: "array<string>",
        description: "Weighted crew pool for tasks filed by sweeps and merged-head repairs.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_task_pilot: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.task_pilot", value_type: "array<string>",
        description: "Weighted crew pool the task pilot draws from once per run.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_required_validation_commands: Vec<String> => Vec<String> {
        key: "workflow.required_validation_commands", value_type: "array<string>",
        description: "Commands every delivered candidate must pass before it is accepted.",
        section: ConfigSection::Delivery, order: 60,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_resource_throttle_cpu_source: String => String {
        key: "workflow.resource_throttle.cpu_source", value_type: "string",
        description: "CPU throttle signal: load average or busy utilization.",
        section: ConfigSection::Delivery, order: 129,
        resolve: |raw: Option<String>| resolve_choice(raw, "load", "workflow.resource_throttle.cpu_source", &["load", "utilization"]),
    },
    workflow_resource_throttle_cpu_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.cpu_high_percent", value_type: "integer",
        description: "Host CPU high-water percentage that starts throttling admissions.",
        section: ConfigSection::Delivery, order: 130,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.cpu_high_percent"),
    },
    workflow_resource_throttle_cpu_light_leaves: u8 => u8 {
        key: "workflow.resource_throttle.cpu_light_leaves", value_type: "integer",
        description: "CPU-light leaves still admitted while only CPU pressure throttles.",
        section: ConfigSection::Delivery, order: 137,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u8>| resolve_cpu_light_leaves(raw),
    },
    workflow_resource_throttle_cpu_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.cpu_resume_percent", value_type: "integer",
        description: "Host CPU percentage below which throttled admissions resume.",
        section: ConfigSection::Delivery, order: 131,
        resolve: |raw: Option<u8>| resolve_percent(raw, 85, "workflow.resource_throttle.cpu_resume_percent"),
    },
    workflow_resource_throttle_disk_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.disk_high_percent", value_type: "integer",
        description: "Host disk high-water percentage that starts throttling admissions.",
        section: ConfigSection::Delivery, order: 132,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.disk_high_percent"),
    },
    workflow_resource_throttle_disk_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.disk_resume_percent", value_type: "integer",
        description: "Host disk percentage below which throttled admissions resume.",
        section: ConfigSection::Delivery, order: 133,
        resolve: |raw: Option<u8>| resolve_percent(raw, 85, "workflow.resource_throttle.disk_resume_percent"),
    },
    workflow_resource_throttle_enabled: bool => bool {
        key: "workflow.resource_throttle.enabled", value_type: "bool",
        description: "Whether the host resource throttle blocks new admissions.",
        section: ConfigSection::Delivery, order: 134,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    workflow_resource_throttle_io_full_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.io_full_high_percent", value_type: "integer",
        description: "Linux disk I/O stall percentage that starts throttling admissions.",
        section: ConfigSection::Delivery, order: 138,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<u8>| resolve_percent(raw, 20, "workflow.resource_throttle.io_full_high_percent"),
    },
    workflow_resource_throttle_io_full_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.io_full_resume_percent", value_type: "integer",
        description: "Linux disk I/O stall percentage below which admissions resume.",
        section: ConfigSection::Delivery, order: 139,
        resolve: |raw: Option<u8>| resolve_percent(raw, 10, "workflow.resource_throttle.io_full_resume_percent"),
    },
    workflow_resource_throttle_memory_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.memory_high_percent", value_type: "integer",
        description: "Host memory high-water percentage that starts throttling admissions.",
        section: ConfigSection::Delivery, order: 135,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.memory_high_percent"),
    },
    workflow_resource_throttle_memory_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.memory_resume_percent", value_type: "integer",
        description: "Host memory percentage below which throttled admissions resume.",
        section: ConfigSection::Delivery, order: 136,
        resolve: |raw: Option<u8>| resolve_percent(raw, 85, "workflow.resource_throttle.memory_resume_percent"),
    },
    workflow_task_pilot_freshness_material_fields: Vec<MaterialField> => Vec<MaterialField> {
        key: "workflow.task_pilot_freshness.material_fields", value_type: "array<string>",
        description: "Task fields whose edit makes an accepted pilot assessment stale.",
        section: ConfigSection::Delivery, order: 110,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<Vec<MaterialField>>| resolve_material_fields(raw),
    },
    workflow_task_pilot_freshness_source_sensitivity: SourceSensitivity => SourceSensitivity {
        key: "workflow.task_pilot_freshness.source_sensitivity", value_type: "string",
        description: "Whether a branch-head move makes an accepted pilot assessment stale.",
        section: ConfigSection::Delivery, order: 120,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<SourceSensitivity>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_unlocked_context_paths: Vec<String> => Vec<String> {
        key: "workflow.unlocked_context_paths", value_type: "array<string>",
        description: "Path globs whose context entries take no context lock.",
        section: ConfigSection::Delivery, order: 67,
        resolve: resolve_unlocked_context_paths,
    },
    workflow_light_delivery_paths: Vec<String> => Vec<String> {
        key: "workflow.light_delivery_paths", value_type: "array<string>",
        description: "Path globs a skip-review or skip-validation task must change only within.",
        section: ConfigSection::Delivery, order: 71,
        resolve: resolve_light_delivery_paths,
    },
    workflow_unlocked_path_validation_commands: Vec<String> => Vec<String> {
        key: "workflow.unlocked_path_validation_commands", value_type: "array<string>",
        description: "Commands that replace full required validation for a prose-only repair.",
        section: ConfigSection::Delivery, order: 68,
        resolve: resolve_unlocked_path_validation_commands,
    },
    workflow_validation_env_shell_probe: String => String {
        key: "workflow.validation_env.shell_probe", value_type: "string",
        description: "How validation PATH is resolved from the owner's shell.",
        section: ConfigSection::Delivery, order: 61,
        resolve: |raw: Option<String>| resolve_choice(raw, "interactive", "workflow.validation_env.shell_probe", crate::aliases::ShellProbeMode::CHOICES),
    },
    workflow_validation_env_path: Vec<String> => Vec<String> {
        key: "workflow.validation_env.path", value_type: "array<string>",
        description: "PATH entries added for required validation and local shell steps.",
        section: ConfigSection::Delivery, order: 63,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_validation_env_build_output_env: Vec<String> => Vec<String> {
        key: "workflow.validation_env.build_output_env", value_type: "array<string>",
        description: "Workspace output variables bound to a host-owned directory.",
        section: ConfigSection::Delivery, order: 65,
        resolve: |raw: Option<Vec<String>>| resolve_build_output_env(raw),
    },
    workflow_validation_env_cargo_incremental: bool => bool {
        key: "workflow.validation_env.cargo_incremental", value_type: "bool",
        description: "Whether host validation forces Cargo incremental compilation off.",
        section: ConfigSection::Delivery, order: 66,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_validation_env_path_mode: String => String {
        key: "workflow.validation_env.path_mode", value_type: "string",
        description: "How validation PATH entries combine with the resolved PATH.",
        section: ConfigSection::Delivery, order: 64,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<String>| resolve_validation_path_mode(raw),
    },
    workflow_xhard_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.xhard_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned xhard-complexity tasks.",
        section: ConfigSection::Delivery, order: 100,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
}
