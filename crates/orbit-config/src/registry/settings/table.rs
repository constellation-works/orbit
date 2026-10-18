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
    ConfigSection, DEFAULT_WORKFLOW_BASE_BRANCH, SECURITY_ALERT_SEVERITIES,
};
use super::resolve::{
    DEFAULT_JOURNAL_RETENTION_DAYS, DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES, DEFAULT_RETENTION_DAYS,
    DEFAULT_STORE_WARN_BYTES, DEFAULT_WORKER_MEMORY_HIGH, DEFAULT_WORKER_MEMORY_MAX,
    default_log_rotation, default_pass_list, normalize_logins, normalize_pass_list, read_optional,
    resolve_agent_stall_minutes, resolve_bounded_minutes, resolve_build_output_env, resolve_choice,
    resolve_cpu_light_leaves, resolve_distributed_completion, resolve_generated_paths,
    resolve_host_shutdown_hold_minutes, resolve_machine_id, resolve_machine_name,
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
}

define_config_settings! {
    ci_failure_operator_suppression_hours: u32 => u32 {
        key: "ci_failure.operator_suppression_hours", value_type: "integer",
        description: "Hours an archived or rejected exact-key CI sweep task without covered_by suppresses re-filing (0..=720, default 6).",
        section: ConfigSection::Housekeeping, order: 21,
        resolve: |raw: Option<u32>| {
            let hours = raw.unwrap_or(6);
            if hours > 720 {
                Err(OrbitError::InvalidInput("ci_failure.operator_suppression_hours must be 0..=720".into()))
            } else { Ok(hours) }
        },
    },
    automation_stall_window_minutes: u32 => u32 {
        key: "automation.stall_window_minutes", value_type: "integer",
        description: "Minutes a deferred delivery-automation reason may persist before the evaluator logs it at warn and files one friction (1..=1440).",
        section: ConfigSection::Housekeeping, order: 20,
        resolve: |raw: Option<u32>| resolve_bounded_minutes(raw, DEFAULT_STALL_WINDOW_MINUTES, "automation.stall_window_minutes"),
    },
    codex_approval_policy: Option<String> => String {
        key: "execution.codex.approval_policy", value_type: "string",
        description: "Codex approval policy: one of untrusted, on-request, never.",
        section: ConfigSection::Execution, order: 20,
        resolve: |raw: Option<String>| resolve_optional_choice(raw, "execution.codex.approval_policy", CODEX_APPROVAL_POLICIES),
    },
    codex_sandbox: String => String {
        key: "execution.codex.sandbox", value_type: "string",
        description: "Codex sandbox mode: one of read-only, workspace-write, danger-full-access.",
        section: ConfigSection::Execution, order: 10,
        resolve: |raw: Option<String>| resolve_choice(raw, "workspace-write", "execution.codex.sandbox", CODEX_PROVIDER_SANDBOX_MODES),
    },
    execution_env_pass: Vec<String> => Vec<String> {
        key: "execution.env.pass", value_type: "array<string>",
        description: "Environment variable names allow-listed for passthrough into agent subprocesses.",
        section: ConfigSection::Execution, order: 30,
        resolve: |raw: Option<Vec<String>>| raw.map(normalize_pass_list).unwrap_or_else(|| Ok(default_pass_list())),
    },
    execution_proc_spawn_max_timeout_minutes: u32 => u32 {
        key: "execution.proc_spawn_max_timeout_minutes", value_type: "integer",
        description: "Longest timeout one proc.spawn call may run with inside a managed activity, which also caps it at the activity's remaining wall-clock budget (1..=1440, default 45). Outside an activity the ceiling stays 60 seconds.",
        section: ConfigSection::Execution, order: 40,
        resolve: |raw: Option<u32>| resolve_bounded_minutes(raw, DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES, "execution.proc_spawn_max_timeout_minutes"),
    },
    execution_agent_stall_minutes: u32 => u32 {
        key: "execution.agent_stall_minutes", value_type: "integer",
        description: "Minutes a provider process may produce no output and use no CPU before its step fails as stalled and is retried (0 disables, at most 1440, default 30).",
        section: ConfigSection::Execution, order: 50,
        resolve: |raw: Option<u32>| resolve_agent_stall_minutes(raw),
    },
    machine_id: Option<String> => String {
        key: "machine.id", value_type: "string",
        description: "Stable generated identity of this machine (hm_...). Written once by `orbit init` and never reused; not settable.",
        section: ConfigSection::Machine, order: 10,
        resolve: |raw: Option<String>| resolve_machine_id(raw),
    },
    machine_auto_concurrency_admit_cpu_percent: u8 => u8 {
        key: "machine.auto_concurrency.admit_cpu_percent", value_type: "integer",
        description: "Maximum forecast CPU utilization as a percentage of host cores (1..=100, default 90).",
        section: ConfigSection::Machine, order: 33,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "machine.auto_concurrency.admit_cpu_percent"),
    },
    machine_auto_concurrency_admit_memory_percent: u8 => u8 {
        key: "machine.auto_concurrency.admit_memory_percent", value_type: "integer",
        description: "Maximum forecast non-reclaimable memory as a percentage of host memory (1..=100, default 75); must be below global workflow.resource_throttle.memory_high_percent.",
        section: ConfigSection::Machine, order: 34,
        resolve: |raw: Option<u8>| resolve_percent(raw, 75, "machine.auto_concurrency.admit_memory_percent"),
    },
    machine_auto_concurrency_head_max_wait_minutes: u32 => u32 {
        key: "machine.auto_concurrency.head_max_wait_minutes", value_type: "integer",
        description: "Starvation guard: maximum wait for the queue head, in minutes (default 30).",
        section: ConfigSection::Machine, order: 38,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(30)),
    },
    machine_auto_concurrency_head_stuck_passes: u32 => u32 {
        key: "machine.auto_concurrency.head_stuck_passes", value_type: "integer",
        description: "Starvation guard: consecutive protected passes whose miss has not improved before a drain reports the queue head stuck (>= 1, default 20).",
        section: ConfigSection::Machine, order: 39,
        resolve: |raw: Option<u32>| match raw.unwrap_or(20) {
            0 => Err(OrbitError::InvalidInput("machine.auto_concurrency.head_stuck_passes must be at least 1".into())),
            passes => Ok(passes),
        },
    },
    machine_auto_concurrency_horizon_cap_minutes: u32 => u32 {
        key: "machine.auto_concurrency.horizon_cap_minutes", value_type: "integer",
        description: "Maximum forecast horizon for a candidate, in minutes (default 120).",
        section: ConfigSection::Machine, order: 37,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(120)),
    },
    machine_auto_concurrency_min_profile_deliveries: u32 => u32 {
        key: "machine.auto_concurrency.min_profile_deliveries", value_type: "integer",
        description: "Minimum completed deliveries before a profile key is used without fallback (default 10).",
        section: ConfigSection::Machine, order: 36,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(10)),
    },
    machine_auto_concurrency_mode: String => String {
        key: "machine.auto_concurrency.mode", value_type: "string",
        description: "Forecast admission mode: off, shadow, or enforce (default off).",
        section: ConfigSection::Machine, order: 32,
        resolve: |raw: Option<String>| resolve_choice(raw, "off", "machine.auto_concurrency.mode", &["off", "shadow", "enforce"]),
    },
    machine_auto_concurrency_profile_window: u32 => u32 {
        key: "machine.auto_concurrency.profile_window", value_type: "integer",
        description: "Most recent completed deliveries retained per profile key; at least min_profile_deliveries (default 50).",
        section: ConfigSection::Machine, order: 35,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(50)),
    },
    machine_host_shutdown_hold_minutes: u32 => u32 {
        key: "machine.host_shutdown_hold_minutes", value_type: "integer",
        description: "Hold new unattended admissions only when a scheduled host shutdown or reboot is this many minutes away or sooner (>= 1, default 180). A later schedule is still reported and admission continues.",
        section: ConfigSection::Machine, order: 31,
        resolve: |raw: Option<u32>| resolve_host_shutdown_hold_minutes(raw),
    },
    machine_name: Option<String> => String {
        key: "machine.name", value_type: "string",
        description: "Operator-chosen display name for this machine. Change it with `orbit config set --global machine.name <value>`.",
        section: ConfigSection::Machine, order: 20,
        resolve: |raw: Option<String>| resolve_machine_name(raw),
    },
    machine_task_prefix: Option<String> => String {
        key: "machine.task_prefix", value_type: "string",
        description: "Immutable task-id namespace for ids minted on this machine (2-5 uppercase ASCII letters). Chosen once by `orbit init`; not settable.",
        section: ConfigSection::Machine, order: 30,
        resolve: |raw: Option<String>| resolve_task_prefix(raw),
    },
    machine_worker_containment: bool => bool {
        key: "machine.worker_containment", value_type: "bool",
        description: "Launch each detached pipeline worker in its own transient systemd user scope bounded by the machine.worker_* limits (Linux with a systemd user manager). false, or no reachable user manager, launches workers in the caller's cgroup with a warning.",
        section: ConfigSection::Machine, order: 40,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    machine_worker_containment_strict: bool => bool {
        key: "machine.worker_containment_strict", value_type: "bool",
        description: "Refuse detached worker launches when a systemd user scope is unavailable. Requires machine.worker_containment=true; default false keeps warn-and-launch behavior.",
        section: ConfigSection::Machine, order: 45,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    machine_worker_memory_high: MemoryLimit => String {
        key: "machine.worker_memory_high", value_type: "string",
        description: "MemoryHigh= for each contained worker scope, where the kernel starts throttling the run: bytes with an optional K/M/G/T suffix, a percentage of physical RAM, or infinity (default 40%).",
        section: ConfigSection::Machine, order: 50,
        resolve: |raw: Option<String>| resolve_memory_limit(raw, DEFAULT_WORKER_MEMORY_HIGH, "machine.worker_memory_high"),
    },
    machine_worker_memory_max: MemoryLimit => String {
        key: "machine.worker_memory_max", value_type: "string",
        description: "MemoryMax= for each contained worker scope, where the kernel OOM-kills inside the run instead of the host: bytes with an optional K/M/G/T suffix, a percentage of physical RAM, or infinity (default 50%).",
        section: ConfigSection::Machine, order: 60,
        resolve: |raw: Option<String>| resolve_memory_limit(raw, DEFAULT_WORKER_MEMORY_MAX, "machine.worker_memory_max"),
    },
    machine_worker_tasks_max: u32 => u32 {
        key: "machine.worker_tasks_max", value_type: "integer",
        description: "TasksMax= for each contained worker scope: processes and threads the run may hold at once before fork/clone fails (>= 1, default 4096).",
        section: ConfigSection::Machine, order: 70,
        resolve: |raw: Option<u32>| resolve_worker_tasks_max(raw),
    },
    machine_worker_cpu_quota: u32 => u32 {
        key: "machine.worker_cpu_quota", value_type: "integer",
        description: "CPUQuota= for each contained worker scope, as a percentage of one core (400 caps a run at four cores) so one run cannot saturate the host. 0 or unset sets no CPU limit. Linux only; enforced when the systemd user manager delegates the cpu controller.",
        section: ConfigSection::Machine, order: 75,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw.unwrap_or(0)),
    },
    operation_review_crew: Option<Vec<String>> => operation::ReviewCrewSetting {
        key: "operation.review_crew", value_type: "array<string>",
        description: "Crew for automatic review: the before-PR or before-landing reviewer, and the crew of every review task the delivery-code-review auto-task mints (unset, that definition's template crew). One crew name, or a pool written like the complexity pools (`name` or `name:weight`, all bare or all weighted) from which each review draws one crew by weight, preferring a crew other than the one that implemented the reviewed work and skipping disabled or provider-limited crews. [] unsets it. Before-PR and before-landing review refuse to start without it.",
        section: ConfigSection::Review, order: 30,
        resolve: operation::review_crew,
    },
    security_alert_sweep_min_severity: String => String {
        key: "security_alert_sweep.min_severity", value_type: "string",
        description: "Minimum severity filed by the security alert sweep for Dependabot and Code scanning: low, moderate (default), high, or critical. Run input overrides workspace then global config; secret scanning is always filed.",
        section: ConfigSection::Housekeeping, order: 90,
        resolve: |raw: Option<String>| resolve_choice(raw, "moderate", "security_alert_sweep.min_severity", SECURITY_ALERT_SEVERITIES),
    },
    plugin_legacy_callback_identity: bool => bool {
        key: "plugin.legacy_callback_identity", value_type: "bool",
        description: "Deprecated: also accept the environment token and process ancestry as a plugin callback credential. Off by default; identity is the session record the host hands a backend on file descriptor 3. Removed in the next release.",
        section: ConfigSection::Housekeeping, order: 80,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    pr_close_on_terminal: bool => bool {
        key: "pr.close_on_terminal", value_type: "bool",
        description: "Close a task's open Orbit-authored pull requests (delivery and preservation PRs for blocked tasks) when the task lands (done), is rejected or is archived, with a comment naming the landing or the decision. Branches are kept; a forge error is a warning, never a failure (default true).",
        section: ConfigSection::Delivery, order: 140,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    pr_delivery_authors: Vec<String> => Vec<String> {
        key: "pr.delivery_authors", value_type: "array<string>",
        description: "Forge logins whose pull requests count as Orbit-authored for pr.close_on_terminal and for settling a task to done from its delivery PR merged outside Orbit. Empty (default) means the login the forge CLI is authenticated as on this machine.",
        section: ConfigSection::Delivery, order: 141,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(normalize_logins(raw.unwrap_or_default())),
    },
    pr_task_url_template: Option<String> => String {
        key: "pr.task_url_template", value_type: "string",
        description: "URL template used to link a task ID in PR descriptions.",
        section: ConfigSection::Housekeeping, order: 70,
        resolve: |raw: Option<String>| Ok::<_, OrbitError>(raw),
    },
    review_before_pr: bool => bool {
        key: "review.before_pr", value_type: "bool",
        description: "Hold PR creation for a fresh reviewer that fixes what it finds (default false). A drain or ship captures it at submission, so a run in flight keeps the value it started with. PR route only: refused for local-only delivery, and distributed admission refuses an endpoint that has it on. Never on together with review.before_landing. After-landing review is not a config key: it is the delivery-code-review auto-task's own enabled flag.",
        section: ConfigSection::Review, order: 10,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    review_before_landing: bool => bool {
        key: "review.before_landing", value_type: "bool",
        description: "Open the PR first, then have a fresh reviewer review it and fix what it finds while hosted CI runs; the PR lands only at the head that review settled (default false). Any outcome other than an approve leaves the PR open and the task in review. Shares review.minutes and operation.review_crew with before-PR review, and is captured at submission like it. PR route only: refused for local-only delivery. Loading fails while it and review.before_pr are both on: there is one review layer before landing.",
        section: ConfigSection::Review, order: 15,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    review_before_landing_hosts: Vec<String> => Vec<String> {
        key: "review.before_landing_hosts", value_type: "array<string>",
        description: "Owner policy: machine ids (hm_...) whose claimed leaves review their open PR before it lands, as review.before_landing does, while this owner's own deliveries keep review.before_landing off. The probe and pull admission resolve it for the caller's machine label and the claim captures the result. Ignored for machines already covered by review.before_landing = true; loading fails while review.before_pr is on and the list is non-empty. The label is caller-chosen, so this is policy, not a security boundary. Default empty.",
        section: ConfigSection::Review, order: 16,
        resolve: |raw: Option<Vec<String>>| operation::before_landing_hosts(raw).map(Option::unwrap_or_default),
    },
    review_minutes: u32 => u32 {
        key: "review.minutes", value_type: "integer",
        description: "Reviewer runtime minutes for one candidate's before-PR or before-landing review, its fix commit and final validation included. Each candidate gets one review: retries and interruptions share these minutes, and once they are spent the review is not restarted; a changed candidate, such as a completion rebase, is a new review (1..=1440, default 30).",
        section: ConfigSection::Review, order: 20,
        resolve: |raw: Option<u32>| operation::review_minutes(raw).map(|minutes| minutes.unwrap_or(DEFAULT_REVIEW_MINUTES)),
    },
    review_verify_findings: bool => bool {
        key: "review.verify_findings", value_type: "bool",
        description: "Review findings must be verified before approval: a review-filed bug (tagged code-review, delivery-code-review, security-review or qa-sweep) is refused at approval, promotion and drain admission until `finding_verification_pipeline` records a CONFIRMED verdict for it. Off by default, when verification state is ignored. Read when the gate is checked, so a change applies to running drains and the dashboard. Turning it on does not enable the seeded `finding_verification` routine.",
        section: ConfigSection::Review, order: 40,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    retention_audit_days: u32 => u32 {
        key: "retention.audit_days", value_type: "integer",
        description: "Days `orbit gc audit` keeps command and run audit rows; older rows, and the audit blobs no remaining row or pending write names, are reclaimable (1..=36500; default 60).",
        section: ConfigSection::Housekeeping, order: 100,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.audit_days", DEFAULT_RETENTION_DAYS),
    },
    retention_runs_days: u32 => u32 {
        key: "retention.runs_days", value_type: "integer",
        description: "Days after a terminal job run finishes before `orbit gc runs` may drop its pipeline state; the run row and its steps stay (1..=36500; default 60). On a large store, 14 reclaims much more run state than 60.",
        section: ConfigSection::Housekeeping, order: 110,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.runs_days", DEFAULT_RETENTION_DAYS),
    },
    retention_journal_days: u32 => u32 {
        key: "retention.journal_days", value_type: "integer",
        description: "Days after a task commit is applied before `orbit gc journal` may delete its commit journal row (1..=36500; default 14). Recovery replays only prepared and committed rows, so an applied row is history kept for diagnosis; prepared, committed and aborted rows, and each task's newest applied row, are never deleted.",
        section: ConfigSection::Housekeeping, order: 112,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.journal_days", DEFAULT_JOURNAL_RETENTION_DAYS),
    },
    retention_store_warn_bytes: u64 => u64 {
        key: "retention.store_warn_bytes", value_type: "integer",
        description: "Warn from `orbit doctor` when the host store file is larger than this many bytes and this workspace has no enabled store-gc routine (default 1073741824, 1 GiB). Zero warns for any non-empty file. The check stays quiet under the threshold or while a store-gc routine is enabled.",
        section: ConfigSection::Housekeeping, order: 115,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or(DEFAULT_STORE_WARN_BYTES)),
    },
    worktree_reclaim: Vec<String> => Vec<String> {
        key: "worktree.reclaim", value_type: "array<string>",
        description: "Rebuildable paths to reclaim in terminal, registered run worktrees: relative globs with '*' within a component and '**' across components. Replaces earlier lists; defaults to ['target', '.orbit/tmp/*-target', '.orbit/tmp/base-wt']. Symlinks, tracked content and live or undecidable workers are protected. Collection never runs a command.",
        section: ConfigSection::Housekeeping, order: 120,
        resolve: resolve_reclaim,
    },
    worktree_reclaim_below_free_mib: Option<u64> => u64 {
        key: "worktree.reclaim_below_free_mib", value_type: "integer",
        description: "When free MiB under the state directory falls below this optional threshold, admission reclaims declared paths in oldest terminal worktrees first. Unset disables admission reclamation.",
        section: ConfigSection::Housekeeping, order: 130,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw),
    },
    worktree_parked_reclaim_after_hours: u64 => u64 {
        key: "worktree.parked_reclaim_after_hours", value_type: "integer",
        description: "Hours a task must stay parked in `someday`, counted from that status change, before worktree GC may remove the worktree of a terminal run serving it. Removal also needs no live worker, no unsettled claim and no Git worktree lock. A clean tree goes only when its run branch's commits are reachable from another ref or on origin; otherwise the worktree and branch stay as `skipped:unpublished_commits` (0..=876000, default 336).",
        section: ConfigSection::Housekeeping, order: 135,
        resolve: |raw: Option<u64>| match raw {
            Some(hours) if hours > 876_000 => Err(OrbitError::InvalidInput(format!(
                "worktree.parked_reclaim_after_hours must be at most 876000, got {hours}"
            ))),
            raw => Ok(raw.unwrap_or(336)),
        },
    },
    review_baseline_commands: Vec<String> => Vec<String> {
        key: "review.baseline_commands", value_type: "array<string>",
        description: "Commands before-PR review settlement may rerun on the host to confirm a reviewer's claim that a failed required check fails the same way on the pinned base; `workflow.required_validation_commands` always count. A confirmed claim holds the task in the backlog until the base passes instead of blocking it. When the host refutes the claim, its own run on the final candidate decides: a pass with no open finding counts the check as passed and the certificate records the override, and a failure while the base passes rejects the review. A claim about any other command cannot be confirmed and settles the review incomplete. A listed command's failure cannot be recorded as a diagnostic: it needs a passing record or a confirmed claim. Captured when a delivery or claim is admitted. With the required commands, these are also the gates an implementer may hand off when its sandbox deferred only Bubblewrap tests; validation then runs such a gate outside the agent sandbox before delivery. Default empty.",
        section: ConfigSection::Review, order: 30,
        resolve: resolve_unlocked_path_validation_commands,
    },
    runtime_log_max_file_mb: u64 => u64 {
        key: "runtime.log_max_file_mb", value_type: "integer",
        description: "Roll the active operational orbit.jsonl log once it grows past this many MiB (must be >= 1 and <= runtime.log_max_total_mb).",
        section: ConfigSection::Housekeeping, order: 50,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_file_bytes / (1024 * 1024))),
    },
    runtime_log_max_total_mb: u64 => u64 {
        key: "runtime.log_max_total_mb", value_type: "integer",
        description: "Total size budget (MiB) across operational orbit.jsonl archives; oldest are pruned first when exceeded (must be >= 1).",
        section: ConfigSection::Housekeeping, order: 40,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_total_bytes / (1024 * 1024))),
    },
    runtime_log_retention_days: u64 => u64 {
        key: "runtime.log_retention_days", value_type: "integer",
        description: "Delete operational and agent JSONL log archives whose mtime is older than this many days (must be >= 1).",
        section: ConfigSection::Housekeeping, order: 30,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().retention_days)),
    },
    tasks_id_start: Option<u32> => u32 {
        key: "tasks.id_start", value_type: "integer",
        description: "Floor for the local task-id allocator on this machine (forward-only; lets machines hold disjoint id ranges).",
        section: ConfigSection::Housekeeping, order: 60,
        resolve: |raw: Option<u32>| Ok::<_, OrbitError>(raw),
    },
    workflow_auto_ship: bool => bool {
        key: "workflow.auto_ship", value_type: "bool",
        description: "Opt-in for `orbit run ship-sweep` unattended ship dispatch; the seeded ship-sweep routine does not read it.",
        section: ConfigSection::Delivery, order: 40,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_base_branch: String => String {
        key: "workflow.base_branch", value_type: "string",
        description: "Config fallback for ship/auto/pilot base branch when no registered workspace base_branch is bound.",
        section: ConfigSection::Delivery, order: 10,
        resolve: |raw: Option<String>| resolve_non_empty(raw, DEFAULT_WORKFLOW_BASE_BRANCH, "workflow.base_branch"),
    },
    workflow_default_crew: Option<String> => String {
        key: "workflow.default_crew", value_type: "string",
        description: "Named crew used when a task does not declare `crew` and no CLI override is given.",
        section: ConfigSection::Delivery, order: 20,
        resolve: |raw: Option<String>| resolve_optional_non_empty(raw, "workflow.default_crew"),
    },
    workflow_distributed_completion: String => String {
        key: "workflow.distributed_completion", value_type: "string",
        description: "How far this owner takes a distributed execution claim's accepted handoff: `review` (default) waits for an operator's Approve handoff; `done` has the owner authorize and land it through its landing job, as `orbit run auto --complete` does for its own tasks.",
        section: ConfigSection::Delivery, order: 65,
        resolve: |raw: Option<String>| resolve_distributed_completion(raw),
    },
    workflow_final_recovery_crews: Option<Vec<String>> => Vec<String> {
        key: "workflow.final_recovery_crews", value_type: "array<string>",
        description: "Weighted crew pool the final-recovery activity draws once per run after step recovery is exhausted; entries are `name` or `name:weight` (all bare or all weighted). Unset defaults to [\"sol:100\", \"opus:20\"], keeping only the members the crew registry defines; [] disables final recovery.",
        section: ConfigSection::Delivery, order: 105,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw),
    },
    workflow_generated_paths: BTreeMap<String, String> => BTreeMap<String, String> {
        key: "workflow.generated_paths", value_type: "table<string,string>",
        description: "Generated files and the command that regenerates each, keyed by relative path glob (for example `\"docs/INDEX.md\" = \"make docs-index\"`). Conflict recovery never hand-merges a conflicting path that matches: the host takes one side, runs its command in the checkout and stages the output; a command that fails or leaves conflict markers fails the repair. Default empty.",
        section: ConfigSection::Delivery, order: 69,
        resolve: resolve_generated_paths,
    },
    workflow_hard_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.hard_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned hard-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool.",
        section: ConfigSection::Delivery, order: 90,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_low_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.low_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned low-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool.",
        section: ConfigSection::Delivery, order: 70,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_medium_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.medium_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned medium-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool.",
        section: ConfigSection::Delivery, order: 80,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_mid_run_steering_enabled: bool => bool {
        key: "workflow.mid_run_steering.enabled", value_type: "bool",
        description: "Record which task comments each agent prompt embedding task context included, as the run's steering watermark, and deliver operator comments written after it at the run's later prompts; the shipped delivery pipelines do not commit while one is undelivered (default false).",
        section: ConfigSection::Delivery, order: 140,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_mid_run_steering_max_rounds: u8 => u8 {
        key: "workflow.mid_run_steering.max_rounds", value_type: "integer",
        description: "Steering rounds a run may consume at the promotion gate before it holds as `steering_unconsumed` (1..=10, default 2).",
        section: ConfigSection::Delivery, order: 141,
        resolve: |raw: Option<u8>| resolve_mid_run_steering_max_rounds(raw),
    },
    workflow_provider_limit_budgets: Vec<String> => Vec<String> {
        key: "workflow.provider_limit_budgets", value_type: "array<string>",
        description: "Operator-declared rolling budgets for providers that report no usage, each `provider:<amount><usd|tokens>/<n><h|d>` such as `grok:30usd/5h`, with each provider named once. Orbit reads this host's spend from its invocation ledger and treats the provider as used `spent / budget`; none by default.",
        section: ConfigSection::Delivery, order: 109,
        resolve: |raw: Option<Vec<String>>| crate::provider_limit_budget::admit_budgets(raw),
    },
    workflow_provider_limit_explicit_crews: String => String {
        key: "workflow.provider_limit_explicit_crews", value_type: "string",
        description: "What delivery admission does with a task whose explicit crew is at its provider's usage limit: `wait` (default) keeps it in the backlog until the limit lifts; `pool` draws it from the unlimited members of its complexity pool.",
        section: ConfigSection::Delivery, order: 108,
        resolve: |raw: Option<String>| crate::provider_limit::admit_explicit_crews(raw),
    },
    workflow_provider_limit_max_used_pct: u8 => u8 {
        key: "workflow.provider_limit_max_used_pct", value_type: "integer",
        description: "Delivery admission skips a crew while a live usage window of its provider, or of its model, is exhausted or used at or above this percent (1..=100, default 90; 100 skips only on exhaustion).",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<u8>| resolve_percent(raw, crate::provider_limit::DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT, "workflow.provider_limit_max_used_pct"),
    },
    workflow_provider_limit_overrides: Vec<String> => Vec<String> {
        key: "workflow.provider_limit_overrides", value_type: "array<string>",
        description: "Per-provider thresholds replacing `workflow.provider_limit_max_used_pct`, each `provider:percent` (a percent in 1..=100) or `provider:off` (record readings without gating admission), each provider named once; aliases such as `anthropic` resolve to their provider.",
        section: ConfigSection::Delivery, order: 107,
        resolve: |raw: Option<Vec<String>>| crate::provider_limit::admit_overrides(raw),
    },
    workflow_purpose_crews_auto_fix: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.auto_fix", value_type: "array<string>",
        description: "Weighted crew pool `orbit run auto --auto-fix` pull-request repairs draw from, once per run. Unset or [] keeps today's route: `workflow.low_complexity_crews`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_baseline_red_repair: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.baseline_red_repair", value_type: "array<string>",
        description: "Weighted crew pool tasks the red-base re-check files (tag `baseline-red-repair`) draw from when they are created without a crew. Unset or [] keeps today's route: the task's complexity pool, then `workflow.default_crew`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_ci_failure_sweep: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.ci_failure_sweep", value_type: "array<string>",
        description: "Weighted crew pool tasks the CI failure sweep files (tag `ci-failure-sweep`) draw from when they are created without a crew. Unset or [] keeps today's route: the task's complexity pool, then `workflow.default_crew`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_dependabot: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.dependabot", value_type: "array<string>",
        description: "Weighted crew pool Dependabot and code-scanning sweep tasks (tags `dependabot-sweep`, `code-scanning-sweep`) draw from when they are created without a crew. Unset or [] keeps today's route: the task's complexity pool, then `workflow.default_crew`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_failure_recovery: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.failure_recovery", value_type: "array<string>",
        description: "Weighted crew pool step-failure and PR conflict recovery draw from, once per run. Unset or [] keeps today's route: `workflow.medium_complexity_crews`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_merged_head_repair: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.merged_head_repair", value_type: "array<string>",
        description: "Weighted crew pool merged-head repair tasks a review reconciliation files draw from when they are created without a crew. Unset or [] keeps today's route: the task's complexity pool, then `workflow.default_crew`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_purpose_crews_task_pilot: Vec<String> => Vec<String> {
        key: "workflow.purpose_crews.task_pilot", value_type: "array<string>",
        description: "Weighted crew pool the task pilot draws from, once per run, keeping only crews that can inspect source natively. Unset or [], or with no such crew, keeps today's route: `workflow.low_complexity_crews`. Entries are `name` or `name:weight` (all bare or all weighted). An explicit crew wins over it, and the run's allowed_crews still applies.",
        section: ConfigSection::Delivery, order: 106,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_required_validation_commands: Vec<String> => Vec<String> {
        key: "workflow.required_validation_commands", value_type: "array<string>",
        description: "Commands every delivered candidate must pass: owner PR and local deliveries run them before push or merge, and a distributed execution claim must pass them before this owner accepts its handoff; empty means no required check on any path: nothing runs and a claimed handoff carries no validation logs.",
        section: ConfigSection::Delivery, order: 60,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_resource_throttle_cpu_source: String => String {
        key: "workflow.resource_throttle.cpu_source", value_type: "string",
        description: "CPU throttle signal: load (default, Linux one-minute load divided by online CPUs) or utilization (busy CPU tick share excluding idle and I/O wait). macOS uses Mach utilization for either source.",
        section: ConfigSection::Delivery, order: 129,
        resolve: |raw: Option<String>| resolve_choice(raw, "load", "workflow.resource_throttle.cpu_source", &["load", "utilization"]),
    },
    workflow_resource_throttle_cpu_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.cpu_high_percent", value_type: "integer",
        description: "Host cpu high-water percentage (1..=100, default 90).",
        section: ConfigSection::Delivery, order: 130,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.cpu_high_percent"),
    },
    workflow_resource_throttle_cpu_light_leaves: u8 => u8 {
        key: "workflow.resource_throttle.cpu_light_leaves", value_type: "integer",
        description: "Leaves reserved for CPU-light work (`no-diff-expected` auto-tasks) while only CPU pressure throttles admissions; memory, disk and I/O pressure still hold them (0..=32, default 2; 0 holds them too).",
        section: ConfigSection::Delivery, order: 137,
        resolve: |raw: Option<u8>| resolve_cpu_light_leaves(raw),
    },
    workflow_resource_throttle_cpu_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.cpu_resume_percent", value_type: "integer",
        description: "Host cpu resume percentage (1..=100, default 85).",
        section: ConfigSection::Delivery, order: 131,
        resolve: |raw: Option<u8>| resolve_percent(raw, 85, "workflow.resource_throttle.cpu_resume_percent"),
    },
    workflow_resource_throttle_disk_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.disk_high_percent", value_type: "integer",
        description: "Host disk high-water percentage (1..=100, default 90).",
        section: ConfigSection::Delivery, order: 132,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.disk_high_percent"),
    },
    workflow_resource_throttle_disk_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.disk_resume_percent", value_type: "integer",
        description: "Host disk resume percentage (1..=100, default 85).",
        section: ConfigSection::Delivery, order: 133,
        resolve: |raw: Option<u8>| resolve_percent(raw, 85, "workflow.resource_throttle.disk_resume_percent"),
    },
    workflow_resource_throttle_enabled: bool => bool {
        key: "workflow.resource_throttle.enabled", value_type: "bool",
        description: "Enable the host resource throttle verdict; disabled still reports pressure.",
        section: ConfigSection::Delivery, order: 134,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    workflow_resource_throttle_io_full_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.io_full_high_percent", value_type: "integer",
        description: "Linux disk I/O high-water mark: the share of the last minute (`/proc/pressure/io` `full avg60`) in which every runnable task was stalled on I/O (1..=100, default 20). Hosts without pressure stall information are not gated.",
        section: ConfigSection::Delivery, order: 138,
        resolve: |raw: Option<u8>| resolve_percent(raw, 20, "workflow.resource_throttle.io_full_high_percent"),
    },
    workflow_resource_throttle_io_full_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.io_full_resume_percent", value_type: "integer",
        description: "Linux disk I/O resume mark: admission resumes once `/proc/pressure/io` `full avg60` falls below it (1..=100, default 10).",
        section: ConfigSection::Delivery, order: 139,
        resolve: |raw: Option<u8>| resolve_percent(raw, 10, "workflow.resource_throttle.io_full_resume_percent"),
    },
    workflow_resource_throttle_memory_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.memory_high_percent", value_type: "integer",
        description: "Host memory high-water percentage (1..=100, default 90).",
        section: ConfigSection::Delivery, order: 135,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.memory_high_percent"),
    },
    workflow_resource_throttle_memory_resume_percent: u8 => u8 {
        key: "workflow.resource_throttle.memory_resume_percent", value_type: "integer",
        description: "Host memory resume percentage (1..=100, default 85).",
        section: ConfigSection::Delivery, order: 136,
        resolve: |raw: Option<u8>| resolve_percent(raw, 85, "workflow.resource_throttle.memory_resume_percent"),
    },
    workflow_task_pilot_freshness_material_fields: Vec<MaterialField> => Vec<MaterialField> {
        key: "workflow.task_pilot_freshness.material_fields", value_type: "array<string>",
        description: "Task inputs whose edit makes an accepted task-pilot assessment stale, from title, description, criteria, plan, selectors, tags, crew, tools, type, complexity, relations, dependencies, instructions (default title, description, criteria, plan, selectors). A routine's trigger.state.freshness overrides it.",
        section: ConfigSection::Delivery, order: 110,
        resolve: |raw: Option<Vec<MaterialField>>| resolve_material_fields(raw),
    },
    workflow_task_pilot_freshness_source_sensitivity: SourceSensitivity => SourceSensitivity {
        key: "workflow.task_pilot_freshness.source_sensitivity", value_type: "string",
        description: "Whether a branch-head move makes an accepted task-pilot assessment stale: ignore (default), context_files (only when the head changed a path the task's selectors name) or any. A routine's trigger.state.freshness overrides it.",
        section: ConfigSection::Delivery, order: 120,
        resolve: |raw: Option<SourceSensitivity>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_unlocked_context_paths: Vec<String> => Vec<String> {
        key: "workflow.unlocked_context_paths", value_type: "array<string>",
        description: "Relative path globs (`*` within a component, `**` across components) whose `context_files` entries take no context lock: the task still receives them as context, but admission, readiness and lock reservations skip them. A task whose every entry matches still locks them all. Default [\"**/*.md\"]; [] locks every entry.",
        section: ConfigSection::Delivery, order: 67,
        resolve: resolve_unlocked_context_paths,
    },
    workflow_unlocked_path_validation_commands: Vec<String> => Vec<String> {
        key: "workflow.unlocked_path_validation_commands", value_type: "array<string>",
        description: "Commands that replace `workflow.required_validation_commands` for a candidate whose tree differs from the last tree this run passed full required validation on only in paths matching `workflow.unlocked_context_paths` (a conflict or recovery repair that touched only prose, on an unchanged code base). Any other difference, including a rebase onto a base that changed code, runs the full list. Empty (default) always runs the full list.",
        section: ConfigSection::Delivery, order: 68,
        resolve: resolve_unlocked_path_validation_commands,
    },
    workflow_validation_env_login_shell: bool => bool {
        key: "workflow.validation_env.login_shell", value_type: "bool",
        description: "Resolve required-validation and local_shell PATH (plus allowlisted toolchain locators such as CARGO_HOME) from the owner's shell via `-i -l -c`, falling back to `-l -c` on failure (default true); false never probes the shell. Each probe is bounded to 10 seconds and the outcome is cached for two minutes.",
        section: ConfigSection::Delivery, order: 61,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    workflow_validation_env_interactive: bool => bool {
        key: "workflow.validation_env.interactive", value_type: "bool",
        description: "Try an interactive login shell (`-i -l -c`) so toolchains exported in rc files are found (default true). Startup failure, nonzero exit, timeout or a missing marker falls back to `-l -c`, recording the mode and reason. False probes only `-l -c`; ignored when login_shell is false. Only allowlisted toolchain variables cross.",
        section: ConfigSection::Delivery, order: 62,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
    },
    workflow_validation_env_path: Vec<String> => Vec<String> {
        key: "workflow.validation_env.path", value_type: "array<string>",
        description: "PATH entries for required validation and local_shell steps, combined with the resolved PATH per `workflow.validation_env.path_mode`; a leading `~/` expands to HOME. Empty adds nothing.",
        section: ConfigSection::Delivery, order: 63,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_validation_env_build_output_env: Vec<String> => Vec<String> {
        key: "workflow.validation_env.build_output_env", value_type: "array<string>",
        description: "Workspace output variables bound to a host-owned source-keyed directory for required validation and host evidence on every platform. Empty (default) allocates nothing. Configured bindings replace inherited values; commands must honor them. Execution identity and ORBIT_ variables are reserved.",
        section: ConfigSection::Delivery, order: 65,
        resolve: |raw: Option<Vec<String>>| resolve_build_output_env(raw),
    },
    workflow_validation_env_cargo_incremental: bool => bool {
        key: "workflow.validation_env.cargo_incremental", value_type: "bool",
        description: "When false (default), required validation, host evidence, baseline reruns and recovery that bind a host validation target force CARGO_INCREMENTAL=0 so one-shot targets do not accumulate an incremental cache. True leaves CARGO_INCREMENTAL untouched. Applied where Orbit spawns those commands. No binding allocates nothing and does not set the variable.",
        section: ConfigSection::Delivery, order: 66,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    workflow_validation_env_path_mode: String => String {
        key: "workflow.validation_env.path_mode", value_type: "string",
        description: "How `workflow.validation_env.path` combines with the resolved PATH: `prepend` (default) puts it first, `replace` makes it the whole PATH.",
        section: ConfigSection::Delivery, order: 64,
        resolve: |raw: Option<String>| resolve_validation_path_mode(raw),
    },
    workflow_xhard_complexity_crews: Vec<String> => Vec<String> {
        key: "workflow.xhard_complexity_crews", value_type: "array<string>",
        description: "Weighted crew pool for unassigned xhard-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool.",
        section: ConfigSection::Delivery, order: 100,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
}
