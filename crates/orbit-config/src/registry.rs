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
use orbit_types::identity::{
    Crew, CrewAssignment, resolve_crew, validate_machine_id, validate_machine_name,
    validate_stored_task_prefix,
};
use orbit_types::workflow::automation::recovery::DEFAULT_STALL_WINDOW_MINUTES;
use orbit_types::workflow::{CODEX_PROVIDER_SANDBOX_MODES, Provider};

use crate::operation::{self, ReviewPolicy};
use serde::de::DeserializeOwned;
use serde_json::{Value as JsonValue, json};

const DEFAULT_WORKFLOW_BASE_BRANCH: &str = "main";
/// Approval policies `execution.codex.approval_policy` admits.
const CODEX_APPROVAL_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
const DEFAULT_WORKFLOW_CREW: &str = "opus";
/// Built-in name of the bounded system lane and the default value of
/// `workflow.system_crew`. Shipped job steps name this crew directly, and a
/// seeded config defines no `[crews.system]` table — `orbit init` points
/// `workflow.system_crew` at a real cheap-tier crew instead — so the name is
/// resolved onto that crew at load; see `resolved::alias_system_crew`.
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

/// Where a key belongs in the grouped `orbit config show`/`keys` output.
///
/// Declared per registry row so a new key cannot be added without a home,
/// and so `keys` and `show` group identically. [`ConfigSection::ORDER`] is
/// the rendering order; [`ConfigKeyDescriptor::order`] orders keys inside a
/// section by relevance rather than alphabetically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigSection {
    /// `machine.*` — this machine's identity. Global-only: a workspace
    /// `config.toml` may neither supply nor override it.
    Machine,
    /// `workflow.*` — how tasks are shipped.
    Delivery,
    /// `crews.*` — named provider/model assignments. No fixed registry rows:
    /// crew tables are dynamically named (see `CREW_CONFIG_FIELDS`).
    Crews,
    /// `execution.*` — how agent subprocesses run.
    Execution,
    /// `operation.*` — automatic review policy.
    Operation,
    /// Everything else: `automation.*`, `runtime.*`, `scoring.*`, `tasks.*`,
    /// `pr.*`, `plugin.*`.
    Housekeeping,
}

impl ConfigSection {
    /// Sections in rendering order.
    pub const ORDER: &'static [ConfigSection] = &[
        ConfigSection::Machine,
        ConfigSection::Delivery,
        ConfigSection::Crews,
        ConfigSection::Execution,
        ConfigSection::Operation,
        ConfigSection::Housekeeping,
    ];

    /// Section heading shown by `orbit config show`.
    pub fn title(self) -> &'static str {
        match self {
            Self::Machine => "Machine (machine.*)",
            Self::Delivery => "Delivery (workflow.*)",
            Self::Crews => "Crews (crews.*)",
            Self::Execution => "Execution (execution.*)",
            Self::Operation => "Review (operation.*)",
            Self::Housekeeping => "Housekeeping",
        }
    }

    /// One-line explanation of what the section governs.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Machine => "who this machine is (global config only)",
            Self::Delivery => "how tasks are shipped",
            Self::Crews => "named provider/model assignments",
            Self::Execution => "how agent subprocesses run",
            Self::Operation => "automatic review policy",
            Self::Housekeeping => "logs, scoring, ids, plugins, and PR links",
        }
    }

    /// Dotted prefix every key in the section shares, when there is one.
    /// `Housekeeping` spans several prefixes and has none, so its rows print
    /// the full key.
    pub fn key_prefix(self) -> Option<&'static str> {
        match self {
            Self::Machine => Some("machine"),
            Self::Delivery => Some("workflow"),
            Self::Crews => Some("crews"),
            Self::Execution => Some("execution"),
            Self::Operation => Some("operation"),
            Self::Housekeeping => None,
        }
    }

    /// Stable lowercase token for `--json` output.
    pub fn token(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Delivery => "delivery",
            Self::Crews => "crews",
            Self::Execution => "execution",
            Self::Operation => "operation",
            Self::Housekeeping => "housekeeping",
        }
    }
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
    /// Grouping for `orbit config show`/`keys`.
    pub section: ConfigSection,
    /// Relevance ordinal within the section; lower renders first. Registry
    /// declaration order stays alphabetical (a store test enforces it), so
    /// this is what puts the settings an operator reaches for at the top.
    pub order: u16,
}

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
    machine_id: Option<String> => String {
        key: "machine.id", value_type: "string",
        description: "Stable generated identity of this machine (hm_...). Written once by `orbit init` and never reused; not settable.",
        section: ConfigSection::Machine, order: 10,
        resolve: |raw: Option<String>| resolve_machine_id(raw),
    },
    machine_name: Option<String> => String {
        key: "machine.name", value_type: "string",
        description: "Operator-chosen display name for this machine. The one `[machine]` value that may change: `orbit config set --global machine.name <value>`.",
        section: ConfigSection::Machine, order: 20,
        resolve: |raw: Option<String>| resolve_machine_name(raw),
    },
    machine_task_prefix: Option<String> => String {
        key: "machine.task_prefix", value_type: "string",
        description: "Immutable task-id namespace for ids minted on this machine (2-5 uppercase ASCII letters). Chosen once by `orbit init`; not settable.",
        section: ConfigSection::Machine, order: 30,
        resolve: |raw: Option<String>| resolve_task_prefix(raw),
    },
    operation_review_crew: Option<String> => String {
        key: "operation.review_crew", value_type: "string",
        description: "Crew selected for before-PR automatic review. After-landing review runs from its delivery auto-task and uses that definition's template crew.",
        section: ConfigSection::Operation, order: 20,
        resolve: |raw: Option<String>| operation::review_crew(raw),
    },
    operation_review_minutes: Option<u32> => u32 {
        key: "operation.review_minutes", value_type: "integer",
        description: "Aggregate before-PR reviewer, repair, and final-validation wall-time minutes per delivery candidate lineage (1..=1440, default 30).",
        section: ConfigSection::Operation, order: 50,
        resolve: |raw: Option<u32>| operation::review_minutes(raw),
    },
    operation_review_policy: Option<String> => String {
        key: "operation.review_policy", value_type: "string",
        description: "Automatic review timing: none (default), before-pr, or after-landing. before-pr holds PR creation for a fresh reviewer on the PR route and is refused for local-only delivery.",
        section: ConfigSection::Operation, order: 10,
        resolve: |raw: Option<String>| operation::admit_review_policy(raw),
    },
    operation_review_repair_cycles: Option<u32> => u32 {
        key: "operation.review_repair_cycles", value_type: "integer",
        description: "Reviewer repair/validation cycles allowed per delivery candidate lineage (0..=10, default 2).",
        section: ConfigSection::Operation, order: 40,
        resolve: |raw: Option<u32>| operation::review_repair_cycles(raw),
    },
    operation_review_reviewer_starts: Option<u32> => u32 {
        key: "operation.review_reviewer_starts", value_type: "integer",
        description: "Fresh reviewer invocations allowed per delivery candidate lineage, including retries and invalidations (1..=10, default 2).",
        section: ConfigSection::Operation, order: 30,
        resolve: |raw: Option<u32>| operation::review_reviewer_starts(raw),
    },
    plugin_legacy_callback_identity: bool => bool {
        key: "plugin.legacy_callback_identity", value_type: "bool",
        description: "Deprecated: also accept the environment token and process ancestry as a plugin callback credential. Off by default; identity is the session record the host hands a backend on file descriptor 3. Removed in the next release.",
        section: ConfigSection::Housekeeping, order: 80,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    pr_task_url_template: Option<String> => String {
        key: "pr.task_url_template", value_type: "string",
        description: "URL template used to link a task ID in PR descriptions.",
        section: ConfigSection::Housekeeping, order: 70,
        resolve: |raw: Option<String>| Ok::<_, OrbitError>(raw),
    },
    runtime_log_max_file_mb: u64 => u64 {
        key: "runtime.log_max_file_mb", value_type: "integer",
        description: "Roll the active JSONL log once it grows past this many MiB (must be >= 1 and <= runtime.log_max_total_mb).",
        section: ConfigSection::Housekeeping, order: 50,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_file_bytes / (1024 * 1024))),
    },
    runtime_log_max_total_mb: u64 => u64 {
        key: "runtime.log_max_total_mb", value_type: "integer",
        description: "Total size budget (MiB) across JSONL log archives; oldest are pruned first when exceeded (must be >= 1).",
        section: ConfigSection::Housekeeping, order: 40,
        resolve: |raw: Option<u64>| Ok::<_, OrbitError>(raw.unwrap_or_else(|| default_log_rotation().max_total_bytes / (1024 * 1024))),
    },
    runtime_log_retention_days: u64 => u64 {
        key: "runtime.log_retention_days", value_type: "integer",
        description: "Delete JSONL log archives whose mtime is older than this many days (must be >= 1).",
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
        description: "Opt-in for unattended ship dispatch via the routine/sweep scheduler.",
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
        description: "Commands a distributed execution claim must pass on its exact candidate before this owner accepts its delivery handoff; empty means no claimed handoff can be accepted.",
        section: ConfigSection::Delivery, order: 60,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
    workflow_system_crew: String => String {
        key: "workflow.system_crew", value_type: "string",
        description: "Named crew used by system activities such as step-failure recovery and the task pilot.",
        section: ConfigSection::Delivery, order: 30,
        resolve: |raw: Option<String>| resolve_non_empty(raw, DEFAULT_WORKFLOW_SYSTEM_CREW, "workflow.system_crew"),
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
        self.workflow_default_crew =
            resolve_default_crew(self.workflow_default_crew.take(), crews, env_default)?;
        self.machine().check_complete()?;
        Ok(())
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

/// Every literal a key's resolver accepts, or an empty list when the key is
/// free-form.
///
/// The choices are read from the same constants the resolvers admit against,
/// so an editor that offers them cannot drift from what a write would accept,
/// and a retired choice disappears from both at once.
pub fn config_key_options(key: &str) -> Vec<&'static str> {
    match key {
        "execution.codex.sandbox" => CODEX_PROVIDER_SANDBOX_MODES.to_vec(),
        "execution.codex.approval_policy" => CODEX_APPROVAL_POLICIES.to_vec(),
        "operation.review_policy" => ReviewPolicy::CHOICES.to_vec(),
        _ => Vec::new(),
    }
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

/// Look up one registry key's metadata.
pub fn describe(key: &str) -> Option<&'static ConfigKeyDescriptor> {
    CONFIG_KEY_REGISTRY.iter().find(|entry| entry.key == key)
}

/// Fixed keys retired from the registry that an existing `config.toml` may
/// still carry. Loading warns and ignores each one for one release (see
/// `resolved::warn_compatibility_keys`); `orbit config get`/`set` refuse it
/// with the migration note instead of a did-you-mean, so the operator learns
/// the key is gone rather than misspelled. Delete an entry together with its
/// load warning once the release window has passed.
pub(crate) const REMOVED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "workflow.pilot_max_complexity",
        "the task pilot applies its assessed complexity as-is; route a tier with \
     workflow.<tier>_complexity_crews or pin `crew` on the task instead",
    ),
    (
        "semantic",
        "semantic search was removed; delete the table and use lexical search",
    ),
    (
        "search.model",
        "search uses SQLite FTS5 and no longer selects a model; delete this key",
    ),
    // Operation mode was removed on 2026-09-21; the `[operation]` table keeps
    // only the review keys. See docs/design/orbit-core/4_decisions.md.
    ("operation.preset", OPERATION_MODE_REMOVED_NOTE),
    ("operation.completion", OPERATION_MODE_REMOVED_NOTE),
    ("operation.preparation", OPERATION_MODE_REMOVED_NOTE),
    (
        "operation.preparation_due_seconds",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    ("operation.promotion", OPERATION_MODE_REMOVED_NOTE),
    ("operation.leaf_ceiling", OPERATION_MODE_REMOVED_NOTE),
    ("operation.recovery", OPERATION_MODE_REMOVED_NOTE),
    (
        "operation.recovery_episodes_per_task",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    (
        "operation.recovery_minutes_per_task",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    ("operation.delivery_cap", OPERATION_MODE_REMOVED_NOTE),
];

const OPERATION_MODE_REMOVED_NOTE: &str = "operation mode was removed; the [operation] table \
     keeps only review_policy, review_crew, review_reviewer_starts, review_repair_cycles and \
     review_minutes";

/// The migration note for a removed key, or `None` for any other key.
pub(crate) fn removed_key_note(key: &str) -> Option<&'static str> {
    let key = if key.starts_with("semantic.") {
        "semantic"
    } else {
        key
    };
    REMOVED_CONFIG_KEYS
        .iter()
        .find(|(removed, _)| *removed == key)
        .map(|(_, note)| *note)
}

/// Registry keys `orbit config set` refuses to write, with the reason an
/// operator needs. Unlike [`REMOVED_CONFIG_KEYS`] these are live settings —
/// readable by `orbit config get`/`show` and admitted at load — they are
/// simply not the operator's to change after `orbit init` recorded them.
pub(crate) const IMMUTABLE_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "machine.id",
        "a machine identity is generated once by `orbit init` and never reused; \
         changing it would orphan every task, run, and workspace record minted under it",
    ),
    (
        "machine.task_prefix",
        "the task-id namespace is fixed for the life of this machine's task store; \
         ids already minted under it cannot be renumbered",
    ),
];

/// The refusal note for an unsettable key, or `None` for any other key.
pub(crate) fn immutable_key_note(key: &str) -> Option<&'static str> {
    IMMUTABLE_CONFIG_KEYS
        .iter()
        .find(|(immutable, _)| *immutable == key)
        .map(|(_, note)| *note)
}

/// Dotted prefix of the one table only the global `config.toml` may carry.
pub const GLOBAL_ONLY_KEY_PREFIX: &str = "machine.";

/// Whether `key` names a setting a workspace `config.toml` may not supply.
pub fn is_global_only_key(key: &str) -> bool {
    key == GLOBAL_ONLY_KEY_PREFIX.trim_end_matches('.') || key.starts_with(GLOBAL_ONLY_KEY_PREFIX)
}

/// Admit a dotted key for a write through `orbit config set`.
///
/// Everything [`admit_config_key`] accepts, minus the keys that are read-only
/// after `orbit init` recorded them.
pub fn admit_settable_config_key(key: &str) -> Result<(), OrbitError> {
    admit_config_key(key)?;
    if let Some(note) = immutable_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' is read-only: {note}"
        )));
    }
    Ok(())
}

/// Admit a dotted key for `orbit config get`/`set`.
///
/// Fixed registry keys, live `crews.<name>.<field>` keys and
/// `plugins.<ns>.<key>` keys owned by an installed plugin succeed. A
/// removed key fails with its migration note; unknown registry keys and
/// misspelled crew fields fail with suggestions. All before any document
/// mutation.
pub fn admit_config_key(key: &str) -> Result<(), OrbitError> {
    if describe(key).is_some() {
        return Ok(());
    }
    if let Some(note) = removed_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' was removed and is ignored: {note}"
        )));
    }
    if let Some(parsed) = crate::plugins::parse_plugin_field_key(key)? {
        return crate::plugins::admit_plugin_field_key(parsed);
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
    crate::crew_pools::reject_unpoolable_crew_name(name, "crew config keys")?;
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
