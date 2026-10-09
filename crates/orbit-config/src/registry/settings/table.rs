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
    ConfigSection, DEFAULT_WORKFLOW_BASE_BRANCH, DEFAULT_WORKFLOW_SYSTEM_CREW,
    SECURITY_ALERT_SEVERITIES,
};
use super::resolve::{
    DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES, DEFAULT_WORKER_MEMORY_HIGH, DEFAULT_WORKER_MEMORY_MAX,
    default_log_rotation, default_pass_list, normalize_logins, normalize_pass_list, read_optional,
    resolve_bounded_minutes, resolve_choice, resolve_cpu_light_leaves,
    resolve_distributed_completion, resolve_machine_id, resolve_machine_name,
    resolve_material_fields, resolve_memory_limit, resolve_non_empty, resolve_optional_choice,
    resolve_optional_non_empty, resolve_percent, resolve_reclaim, resolve_retention_days,
    resolve_task_prefix, resolve_validation_path_mode, resolve_worker_tasks_max,
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
                Self::admit_with_env(document, config_path, crews, env_default.as_deref(), false)
            }

            pub(super) fn admit_with_env(
                document: &toml::Value,
                config_path: &Path,
                crews: &BTreeMap<String, Crew>,
                env_default: Option<&str>,
                require_default_crew: bool,
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
    machine_id: Option<String> => String {
        key: "machine.id", value_type: "string",
        description: "Stable generated identity of this machine (hm_...). Written once by `orbit init` and never reused; not settable.",
        section: ConfigSection::Machine, order: 10,
        resolve: |raw: Option<String>| resolve_machine_id(raw),
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
    operation_review_crew: Option<String> => String {
        key: "operation.review_crew", value_type: "string",
        description: "Crew for automatic review: the before-PR or before-landing reviewer, and the crew of every review task the delivery-code-review auto-task mints (unset, that definition's template crew). Before-PR and before-landing review refuse to start without it.",
        section: ConfigSection::Review, order: 30,
        resolve: |raw: Option<String>| operation::review_crew(raw),
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
        description: "Forge logins whose pull requests count as Orbit-authored for pr.close_on_terminal. Empty (default) means the login the forge CLI is authenticated as on this machine.",
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
    review_minutes: u32 => u32 {
        key: "review.minutes", value_type: "integer",
        description: "Reviewer runtime minutes for one candidate's before-PR or before-landing review, its fix commit and final validation included. Each candidate gets one review: retries and interruptions share these minutes, and once they are spent the review is not restarted; a changed candidate, such as a completion rebase, is a new review (1..=1440, default 30).",
        section: ConfigSection::Review, order: 20,
        resolve: |raw: Option<u32>| operation::review_minutes(raw).map(|minutes| minutes.unwrap_or(DEFAULT_REVIEW_MINUTES)),
    },
    retention_audit_days: u32 => u32 {
        key: "retention.audit_days", value_type: "integer",
        description: "Days `orbit gc audit` keeps command and run audit rows; older rows, and the audit blobs no remaining row or pending write names, are reclaimable (1..=36500; default 60).",
        section: ConfigSection::Housekeeping, order: 100,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.audit_days"),
    },
    retention_runs_days: u32 => u32 {
        key: "retention.runs_days", value_type: "integer",
        description: "Days after a terminal job run finishes before `orbit gc runs` may drop its pipeline state; the run row and its steps stay (1..=36500; default 60).",
        section: ConfigSection::Housekeeping, order: 110,
        resolve: |raw: Option<u32>| resolve_retention_days(raw, "retention.runs_days"),
    },
    worktree_reclaim: Vec<String> => Vec<String> {
        key: "worktree.reclaim", value_type: "array<string>",
        description: "Rebuildable paths to reclaim in terminal, registered run worktrees: relative globs with '*' within a component and '**' across components. Replaces earlier lists; defaults to ['target']. Symlinks, tracked content and live or undecidable workers are protected. Collection never runs a command.",
        section: ConfigSection::Housekeeping, order: 120,
        resolve: resolve_reclaim,
    },
    worktree_reclaim_below_free_mib: Option<u64> => u64 {
        key: "worktree.reclaim_below_free_mib", value_type: "integer",
        description: "When free MiB under the state directory falls below this optional threshold, admission reclaims declared paths in oldest terminal worktrees first. Unset disables admission reclamation.",
        section: ConfigSection::Housekeeping, order: 130,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw),
    },
    review_baseline_commands: Vec<String> => Vec<String> {
        key: "review.baseline_commands", value_type: "array<string>",
        description: "Commands before-PR review settlement may rerun on the host to confirm a reviewer's claim that a failed required check fails the same way on the pinned base; `workflow.required_validation_commands` always count. A confirmed claim holds the task in the backlog until the base passes instead of blocking it; a claim about any other command cannot be confirmed and settles the review incomplete. A listed command's failure cannot be recorded as a diagnostic: it needs a passing record or a confirmed claim. Captured when a delivery or claim is admitted. Default empty.",
        section: ConfigSection::Review, order: 30,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default().into_iter().map(|command| command.trim().to_string()).filter(|command| !command.is_empty()).collect::<Vec<_>>()),
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
    scoring_enabled: bool => bool {
        key: "scoring.enabled", value_type: "bool",
        description: "Whether scoreboard metrics are recorded for task runs.",
        section: ConfigSection::Housekeeping, order: 10,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(true)),
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
        description: "Per-provider thresholds replacing `workflow.provider_limit_max_used_pct`, each `provider:percent` with a percent in 1..=100 and each provider named once; aliases such as `anthropic` resolve to their provider.",
        section: ConfigSection::Delivery, order: 107,
        resolve: |raw: Option<Vec<String>>| crate::provider_limit::admit_overrides(raw),
    },
    workflow_required_validation_commands: Vec<String> => Vec<String> {
        key: "workflow.required_validation_commands", value_type: "array<string>",
        description: "Commands every delivered candidate must pass: owner PR and local deliveries run them before push or merge, and a distributed execution claim must pass them before this owner accepts its handoff; empty means no required check on any path: nothing runs and a claimed handoff carries no validation logs.",
        section: ConfigSection::Delivery, order: 60,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_resource_throttle_cpu_high_percent: u8 => u8 {
        key: "workflow.resource_throttle.cpu_high_percent", value_type: "integer",
        description: "Host cpu high-water percentage (1..=100, default 90).",
        section: ConfigSection::Delivery, order: 130,
        resolve: |raw: Option<u8>| resolve_percent(raw, 90, "workflow.resource_throttle.cpu_high_percent"),
    },
    workflow_resource_throttle_cpu_light_leaves: u8 => u8 {
        key: "workflow.resource_throttle.cpu_light_leaves", value_type: "integer",
        description: "Leaves reserved for CPU-light work (`no-diff-expected` auto-tasks) while only CPU pressure throttles admissions; memory and disk pressure still hold them (0..=32, default 2; 0 holds them too).",
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
    workflow_system_crew: String => String {
        key: "workflow.system_crew", value_type: "string",
        description: "Named crew used by system activities such as step-failure recovery and the task pilot.",
        section: ConfigSection::Delivery, order: 30,
        resolve: |raw: Option<String>| resolve_non_empty(raw, DEFAULT_WORKFLOW_SYSTEM_CREW, "workflow.system_crew"),
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
