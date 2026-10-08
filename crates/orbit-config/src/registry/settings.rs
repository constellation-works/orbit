use super::*;

/// Built-in `workflow.final_recovery_crews`: the strongest reasoning crews, so
/// the last automated look at a failed task is the most capable one.
pub(crate) const DEFAULT_FINAL_RECOVERY_CREWS: &[&str] = &["sol:100", "opus:20"];

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

            fn admit_with_env(
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
        description: "Crew for automatic review: the before-PR reviewer, and the crew of every review task the delivery-code-review auto-task mints (unset, that definition's template crew). Before-PR review refuses to start without it.",
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
        description: "Hold PR creation for a fresh reviewer that fixes what it finds (default false). A drain or ship captures it at submission, so a run in flight keeps the value it started with. PR route only: refused for local-only delivery, and distributed admission refuses an endpoint that has it on. After-landing review is not a config key: it is the delivery-code-review auto-task's own enabled flag.",
        section: ConfigSection::Review, order: 10,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    review_minutes: u32 => u32 {
        key: "review.minutes", value_type: "integer",
        description: "Reviewer runtime minutes for one candidate's before-PR review, its fix commit and final validation included. Each candidate gets one review: retries and interruptions share these minutes, and once they are spent the review is not restarted; a changed candidate, such as a completion rebase, is a new review (1..=1440, default 30).",
        section: ConfigSection::Review, order: 20,
        resolve: |raw: Option<u32>| operation::review_minutes(raw).map(|minutes| minutes.unwrap_or(DEFAULT_REVIEW_MINUTES)),
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

impl ConfigSnapshot {
    fn finish_admission(
        &mut self,
        crews: &BTreeMap<String, Crew>,
        env_default: Option<&str>,
        require_default_crew: bool,
    ) -> Result<(), OrbitError> {
        LogRotationConfig::from_parts(
            Some(self.runtime_log_retention_days),
            Some(self.runtime_log_max_total_mb),
            Some(self.runtime_log_max_file_mb),
        )?;
        admit_crew_pool(
            &mut self.workflow_low_complexity_crews,
            crews,
            "workflow.low_complexity_crews",
        )?;
        admit_crew_pool(
            &mut self.workflow_medium_complexity_crews,
            crews,
            "workflow.medium_complexity_crews",
        )?;
        admit_crew_pool(
            &mut self.workflow_hard_complexity_crews,
            crews,
            "workflow.hard_complexity_crews",
        )?;
        admit_crew_pool(
            &mut self.workflow_xhard_complexity_crews,
            crews,
            "workflow.xhard_complexity_crews",
        )?;
        self.workflow_final_recovery_crews = Some(admit_final_recovery_crews(
            self.workflow_final_recovery_crews.take(),
            crews,
        )?);
        self.workflow_default_crew = resolve_default_crew(
            self.workflow_default_crew.take(),
            crews,
            env_default,
            require_default_crew,
        )?;
        if self.machine_worker_containment_strict && !self.machine_worker_containment {
            return Err(OrbitError::InvalidInput(
                "machine.worker_containment_strict=true requires machine.worker_containment=true"
                    .to_string(),
            ));
        }
        let resources = self.resource_throttle();
        for (resource, high, resume) in [
            (
                "cpu",
                resources.cpu_high_percent,
                resources.cpu_resume_percent,
            ),
            (
                "memory",
                resources.memory_high_percent,
                resources.memory_resume_percent,
            ),
            (
                "disk",
                resources.disk_high_percent,
                resources.disk_resume_percent,
            ),
        ] {
            if resume >= high {
                return Err(OrbitError::InvalidInput(format!(
                    "workflow.resource_throttle.{resource}_resume_percent must be less than {resource}_high_percent"
                )));
            }
        }
        self.machine().check_complete()?;
        Ok(())
    }

    /// The admitted `workflow.final_recovery_crews` pool; empty disables final
    /// recovery. Admission always fills the field, so `None` never reaches here.
    pub fn final_recovery_crews(&self) -> &[String] {
        self.workflow_final_recovery_crews
            .as_deref()
            .unwrap_or_default()
    }

    /// This machine's identity, as admitted from the same registry rows every
    /// other consumer reads.
    pub fn machine(&self) -> MachineSettings {
        MachineSettings {
            id: self.machine_id.clone(),
            name: self.machine_name.clone(),
            task_prefix: self.machine_task_prefix.clone(),
        }
    }
}

/// Resource limits for each detached pipeline worker (`machine.worker_*`).
///
/// Values are already admitted: memory limits are typed [`MemoryLimit`]s, so
/// a consumer only formats them for the service manager and has nothing left
/// to reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerContainmentSettings {
    /// `machine.worker_containment` — launch workers in their own scope.
    pub enabled: bool,
    /// `machine.worker_containment_strict` — refuse uncontained launches.
    pub strict: bool,
    /// `machine.worker_memory_high` — throttling threshold.
    pub memory_high: MemoryLimit,
    /// `machine.worker_memory_max` — hard limit; OOM kills stay inside the run.
    pub memory_max: MemoryLimit,
    /// `machine.worker_tasks_max` — process/thread ceiling.
    pub tasks_max: u32,
}

impl ConfigSnapshot {
    /// The admitted `[workflow.task_pilot_freshness]` table: the global layer
    /// a routine's `trigger.state.freshness` overrides.
    pub fn task_pilot_freshness(&self) -> PreparationFreshness {
        PreparationFreshness {
            material_fields: self.workflow_task_pilot_freshness_material_fields.clone(),
            source_sensitivity: self.workflow_task_pilot_freshness_source_sensitivity,
        }
        .normalized()
    }

    /// The admitted `machine.worker_*` limits.
    pub fn worker_containment(&self) -> WorkerContainmentSettings {
        WorkerContainmentSettings {
            enabled: self.machine_worker_containment,
            strict: self.machine_worker_containment_strict,
            memory_high: self.machine_worker_memory_high,
            memory_max: self.machine_worker_memory_max,
            tasks_max: self.machine_worker_tasks_max,
        }
    }
}

/// The `[machine]` table, admitted on its own.
///
/// Identity is resolved on every runtime open and by `orbit init` before the
/// rest of the document is known to admit, so it is readable without resolving
/// crews, execution policy, or review preferences. The values still go
/// through the registry rows' own resolvers, so there is exactly one validator
/// and `orbit config get machine.id` cannot disagree with a runtime open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineSettings {
    /// `machine.id` — the stable generated `hm_…` identity.
    pub id: Option<String>,
    /// `machine.name` — the operator-chosen display name.
    pub name: Option<String>,
    /// `machine.task_prefix` — the immutable task-id namespace.
    pub task_prefix: Option<String>,
}

impl MachineSettings {
    /// Admit `[machine]` from one already-parsed document.
    pub(crate) fn admit(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        let settings = Self {
            id: resolve_machine_id(read_optional(document, "machine.id", config_path)?)?,
            name: resolve_machine_name(read_optional(document, "machine.name", config_path)?)?,
            task_prefix: resolve_task_prefix(read_optional(
                document,
                "machine.task_prefix",
                config_path,
            )?)?,
        };
        settings.check_complete()?;
        Ok(settings)
    }

    /// The complete identity, or `None` when no `[machine]` table exists.
    /// A partial table never reaches here — `check_complete` refuses it.
    pub fn complete(self) -> Option<(String, String, String)> {
        Some((self.id?, self.name?, self.task_prefix?))
    }

    /// `[machine]` is one identity, not three independent settings: a file
    /// either carries the whole table or none of it. A partial table is a hand
    /// edit that would otherwise resolve to a machine with no id or no
    /// namespace, so it fails closed naming the missing keys.
    fn check_complete(&self) -> Result<(), OrbitError> {
        let present = [
            ("machine.id", self.id.is_some()),
            ("machine.name", self.name.is_some()),
            ("machine.task_prefix", self.task_prefix.is_some()),
        ];
        if present.iter().all(|(_, set)| *set) || present.iter().all(|(_, set)| !*set) {
            return Ok(());
        }
        let missing = present
            .iter()
            .filter(|(_, set)| !*set)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
            .join(", ");
        Err(OrbitError::InvalidInput(format!(
            "[machine] is incomplete: {missing} must be set alongside the keys already present; \
             run `orbit init` to create this machine's identity"
        )))
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
            true,
        )
        .unwrap_or_else(|error| panic!("built-in configuration defaults must admit: {error}"))
    }
}

/// The built-in crew registry a config without `[crews]` resolves to, so the
/// built-in snapshot admits the same defaults such a config does.
fn default_admission_crews() -> BTreeMap<String, Crew> {
    crate::resolved::default_crews()
}

/// Admit one `workflow.*_complexity_crews` value in place, replacing it with
/// its canonical `name[:weight]` rendering.
fn admit_crew_pool(
    pool: &mut Vec<String>,
    crews: &BTreeMap<String, Crew>,
    setting: &str,
) -> Result<(), OrbitError> {
    *pool = crate::canonical_crew_pool(pool, crews, setting)?.to_setting_value();
    Ok(())
}

/// Admit `workflow.final_recovery_crews`.
///
/// A written pool is admitted exactly like the complexity pools, so a typo
/// fails at load. An unset key is the built-in default, which names crews a
/// custom `[crews]` registry may not define; it keeps only the members that
/// registry does define rather than failing every command on such a host, and
/// a registry with none of them leaves final recovery disabled.
fn admit_final_recovery_crews(
    raw: Option<Vec<String>>,
    crews: &BTreeMap<String, Crew>,
) -> Result<Vec<String>, OrbitError> {
    let pool = match raw {
        Some(pool) => pool,
        None => DEFAULT_FINAL_RECOVERY_CREWS
            .iter()
            .filter(|entry| {
                let name = entry
                    .rsplit_once(crate::crew_pools::WEIGHT_SEPARATOR)
                    .map_or(**entry, |(name, _)| name);
                crews.contains_key(name)
            })
            .map(ToString::to_string)
            .collect(),
    };
    Ok(
        crate::canonical_crew_pool(&pool, crews, "workflow.final_recovery_crews")?
            .to_setting_value(),
    )
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
        // `orbit config set` falls back to a plain string when the value is not
        // a TOML literal, so `A,B` for a list key lands here as a string.
        let error = error.to_string();
        let error = error.trim_end();
        let hint = if value.is_str() && error.contains("expected a sequence") {
            "; a list is a TOML array, e.g. '[\"A\",\"B\"]' (quote it in the shell)"
        } else {
            ""
        };
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': invalid value for '{key}': {error}{hint}",
            redact_home_dir(&config_path.display().to_string())
        ))
    })
}

/// Trim forge logins, drop blanks and duplicates, and keep a stable order.
/// Forge logins are case-insensitive, so they are compared lowercased.
fn normalize_logins(raw: Vec<String>) -> Vec<String> {
    let mut logins = raw
        .iter()
        .map(|login| login.trim().to_ascii_lowercase())
        .filter(|login| !login.is_empty())
        .collect::<Vec<_>>();
    logins.sort();
    logins.dedup();
    logins
}

/// Default `execution.proc_spawn_max_timeout_minutes`: long enough for a
/// cold workspace build and test gate, still bounded below a typical activity.
const DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES: u32 = 45;

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

/// Default `machine.worker_memory_high`: throttle one run well before it can
/// crowd out the host (2026-09-23 OOM outage, ORB-12903).
const DEFAULT_WORKER_MEMORY_HIGH: MemoryLimit = MemoryLimit::Percent(40);
/// Default `machine.worker_memory_max`: one runaway run keeps at most half of
/// physical RAM, leaving the rest for the host and sibling runs.
const DEFAULT_WORKER_MEMORY_MAX: MemoryLimit = MemoryLimit::Percent(50);
const DEFAULT_WORKER_TASKS_MAX: u32 = 4096;

/// Admit a systemd memory size through [`MemoryLimit::parse`].
///
/// The value later becomes one `systemd-run --property=` argument, so
/// anything outside the grammar is refused here instead of failing every
/// worker launch.
fn resolve_memory_limit(
    raw: Option<String>,
    default: MemoryLimit,
    key: &str,
) -> Result<MemoryLimit, OrbitError> {
    let Some(value) = raw else {
        return Ok(default);
    };
    MemoryLimit::parse(&value).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "{key} has invalid value '{}'; expected a size such as 8G or 512M, \
             a percentage of physical memory such as 50%, or infinity",
            value.trim()
        ))
    })
}

fn resolve_worker_tasks_max(raw: Option<u32>) -> Result<u32, OrbitError> {
    match raw {
        Some(0) => Err(OrbitError::InvalidInput(
            "machine.worker_tasks_max has invalid value 0; expected >= 1".to_string(),
        )),
        Some(value) => Ok(value),
        None => Ok(DEFAULT_WORKER_TASKS_MAX),
    }
}

fn resolve_machine_id(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.id")? else {
        return Ok(None);
    };
    validate_machine_id(&value)
        .map_err(|error| OrbitError::InvalidInput(format!("machine.id is invalid: {error}")))?;
    Ok(Some(value))
}

fn resolve_machine_name(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.name")? else {
        return Ok(None);
    };
    validate_machine_name(&value)
        .map_err(|error| OrbitError::InvalidInput(format!("machine.name is invalid: {error}")))?;
    Ok(Some(value))
}

fn resolve_task_prefix(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.task_prefix")? else {
        return Ok(None);
    };
    validate_stored_task_prefix(&value).map_err(|error| {
        OrbitError::InvalidInput(format!("machine.task_prefix is invalid: {error}"))
    })?;
    Ok(Some(value))
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

/// The default material set when unset; an explicit list must name a field.
fn resolve_material_fields(
    raw: Option<Vec<MaterialField>>,
) -> Result<Vec<MaterialField>, OrbitError> {
    match raw {
        None => Ok(MaterialField::DEFAULT.to_vec()),
        Some(fields) if fields.is_empty() => Err(OrbitError::InvalidInput(
            "workflow.task_pilot_freshness.material_fields must name at least one field"
                .to_string(),
        )),
        Some(fields) => Ok(fields),
    }
}

/// `review` unless the owner opts into landing claimed handoffs itself.
fn resolve_distributed_completion(raw: Option<String>) -> Result<String, OrbitError> {
    match raw.as_deref().map(str::trim) {
        None | Some("review") => Ok("review".to_string()),
        Some("done") => Ok("done".to_string()),
        Some(other) => Err(OrbitError::InvalidInput(format!(
            "workflow.distributed_completion must be `review` or `done`, not `{other}`"
        ))),
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

fn resolve_default_crew(
    configured: Option<String>,
    crews: &BTreeMap<String, Crew>,
    env_default: Option<&str>,
    require_selection: bool,
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
    if crews.is_empty() || !require_selection {
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

/// High-water and recovery thresholds for the host resource verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResourceThrottleSettings {
    /// Evaluate a throttle verdict while always retaining pressure telemetry.
    pub enabled: bool,
    /// CPU high-water percentage.
    pub cpu_high_percent: u8,
    /// CPU recovery percentage, strictly below the high-water mark.
    pub cpu_resume_percent: u8,
    /// Leaves CPU-light work may hold while only CPU pressure throttles.
    pub cpu_light_leaves: u8,
    /// Memory high-water percentage.
    pub memory_high_percent: u8,
    /// Memory recovery percentage.
    pub memory_resume_percent: u8,
    /// Disk high-water percentage.
    pub disk_high_percent: u8,
    /// Disk recovery percentage.
    pub disk_resume_percent: u8,
}

impl ConfigSnapshot {
    /// Admitted host resource thresholds, shared by dashboard and admission consumers.
    pub fn resource_throttle(&self) -> ResourceThrottleSettings {
        ResourceThrottleSettings {
            enabled: self.workflow_resource_throttle_enabled,
            cpu_high_percent: self.workflow_resource_throttle_cpu_high_percent,
            cpu_resume_percent: self.workflow_resource_throttle_cpu_resume_percent,
            cpu_light_leaves: self.workflow_resource_throttle_cpu_light_leaves,
            memory_high_percent: self.workflow_resource_throttle_memory_high_percent,
            memory_resume_percent: self.workflow_resource_throttle_memory_resume_percent,
            disk_high_percent: self.workflow_resource_throttle_disk_high_percent,
            disk_resume_percent: self.workflow_resource_throttle_disk_resume_percent,
        }
    }
}

fn resolve_validation_path_mode(raw: Option<String>) -> Result<String, OrbitError> {
    let mode = raw.unwrap_or_else(|| "prepend".to_string());
    match mode.trim() {
        "prepend" | "replace" => Ok(mode.trim().to_string()),
        other => Err(OrbitError::InvalidInput(format!(
            "workflow.validation_env.path_mode must be `prepend` or `replace`, got `{other}`"
        ))),
    }
}

/// Ceiling for `workflow.resource_throttle.cpu_light_leaves`: a reservation,
/// not a second concurrency limit.
const MAX_CPU_LIGHT_LEAVES: u8 = 32;

fn resolve_cpu_light_leaves(raw: Option<u8>) -> Result<u8, OrbitError> {
    let value = raw.unwrap_or(2);
    if value <= MAX_CPU_LIGHT_LEAVES {
        Ok(value)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "workflow.resource_throttle.cpu_light_leaves must be in 0..={MAX_CPU_LIGHT_LEAVES}"
        )))
    }
}

fn resolve_percent(raw: Option<u8>, default: u8, key: &str) -> Result<u8, OrbitError> {
    let value = raw.unwrap_or(default);
    if (1..=100).contains(&value) {
        Ok(value)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{key} must be in 1..=100"
        )))
    }
}
