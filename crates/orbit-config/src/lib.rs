#![deny(clippy::print_stderr, clippy::print_stdout)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! Orbit's `config.toml` owner: schema admission, layered resolution, source
//! provenance, resolved views, comment-preserving mutation, validation,
//! atomic persistence, and default-config seeding.
//!
//! Orbit config is split across two TOML files:
//! - `~/.orbit/config.toml` — global defaults (agent, env passthrough, execution policy)
//! - `.orbit/config.toml` — workspace-local overrides
//!
//! Ordinary settings inherit per key: workspace values override global values, global values fill omissions, and built-in defaults fill remaining gaps.
//! Nested tables layer recursively; a scalar, array, or registered table value in
//! the workspace file replaces the matching global value. Named crew fields also
//! layer recursively, so a workspace can override one model without restating the
//! crew or registry.
//!
//! The `[machine]` table — this machine's generated `id`, operator-chosen
//! `name`, and immutable `task_prefix` — is global-only. A workspace file that
//! supplies it is refused at load, and `orbit config set` refuses it without
//! `--global`. `machine.id` and `machine.task_prefix` are never settable:
//! `orbit init` writes them once.
//!
//! Three security-sensitive settings are replace-only when a workspace file
//! exists: `execution.codex.sandbox`, `execution.codex.approval_policy`, and
//! `execution.env.pass`. An omitted replace-only setting uses its built-in default
//! rather than inheriting a machine-specific global policy.
//!
//! The `[plugin_enablement]` table — per-workspace plugin toggles — is
//! workspace-only (refused in the global file) and is not a policy layer: a
//! workspace file holding only that table leaves the replace-only settings
//! inheriting from global.
//!
//! # Role
//!
//! A leaf above `orbit-common`: this crate performs no runtime composition and
//! depends on no higher layer. In particular it does not know about
//! `orbit-core` path discovery (callers supply an explicit [`ConfigRoots`]),
//! about `orbit-engine` (PR settings are exposed as config-owned
//! `PrSettings` and translated at composition time), or about a terminal
//! (host detection and interactive prompting belong to the CLI init adapter,
//! which hands this crate an explicit [`ConfigSeed`]).
//!
//! # Module map
//!
//! - `roots` — the explicit two-root input ([`ConfigRoots`]).
//! - `raw` — private serde schema for the parts of the document that are not
//!   fixed registry keys (crew tables and retired-key migration guards).
//! - `registry` — the fixed-key registry and its admitted [`ConfigSnapshot`],
//!   with the snapshot's projections into the domain modules' typed settings.
//!   It sits above the domain admission modules (`aliases`, `crew`,
//!   `operation`, `provider_limit`, `seeding`) whose admitters its key
//!   table calls; none of them names it.
//! - `config_document` — pure TOML path access and the typed dotted-path read
//!   shared below admission.
//! - `config_io` — no-follow `config.toml` reads and path admission, below
//!   loading, diagnostics and the store.
//! - `config_toml` — the shared read-and-parse of one admitted file into a
//!   TOML document, above `config_io` and below policy admission.
//! - `layering` — document reading, per-key merge, replace-only rules, and
//!   source provenance.
//! - `lock_modes` — the ordered `workflow.lock_modes` footprint lock-mode
//!   entries owner admission applies to selector overlaps.
//! - `memory_limit` — the typed worker memory limit ([`MemoryLimit`]) and
//!   its one parser [ORB-12913].
//! - `crew::task_types` — the ordered `workflow.task_types` list and the one
//!   task-type classification type-pool routing and the scorecard share.
//! - `crew::pools` — weighted pool grammar and admission.
//! - `regression_layers` — the ordered `workflow.regression_layers` path
//!   classifier codebase regression analytics read a fix's layer through.
//! - `provider_limit` — provider/account admission policy, with threshold and draw
//!   policy in `policy`, budget declarations in `budget`, and shared targets in `target`.
//! - `operation` — typed review preferences (`[review]` and
//!   `workflow.purpose_crews.review`, one crew or a pool) and their layered
//!   resolution [ORB-11333] [ORB-13992] [ORB-15195].
//! - `aliases` — read-time translation of deprecated spellings into their
//!   replacement keys [ORB-16775].
//! - `resolved` — the consumer-facing [`ResolvedConfig`] views, assembled in
//!   `resolved/config.rs` with crew admission, compatibility checks and environment
//!   projection in sibling named modules.
//! - `persistence` — artifact path resolution from the two roots.
//! - `plugin_enablement` — the workspace `[plugin_enablement]` toggles.
//! - `tag_instructions` — trusted tag instructions and activity corpus mappings.
//! - `experiment_keys` — typed workflow keys admitted for task-local experiments.
//! - `review_skip_tags` — the `[workflow.review_skip_tags.<tag>]` review-skip mapping.
//! - `completion_hold` — admission of the `[[completion.hold]]` rules.
//! - `store` — comment-preserving [`ConfigStore`] edits, the per-file write lock
//!   and an atomic save that refuses to replace a file changed since the store
//!   read it.
//! - `seed` — rendering and writing a fresh default `config.toml`.

mod aliases;
mod completion_hold;
mod config_document;
mod config_io;
mod config_toml;
mod crew;
mod crew_defaults;
mod experiment_keys;
mod human_crew_seed;
mod layering;
mod lock_modes;
mod memory_limit;
mod operation;
mod persistence;
mod plugin_enablement;
mod plugins;
mod provider_limit;
mod raw;
mod registry;
mod regression_layers;
mod resolved;
mod review_skip_tags;
mod roots;
mod seed;
mod seeding;
mod store;
mod tag_instructions;
mod unknown;

pub use unknown::{UnknownConfigKey, unknown_config_keys_at, unknown_config_keys_present};

use crew::{auto_effort as crew_auto_effort, pools as crew_pools, task_types as crew_task_types};

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

pub use aliases::{WorkerContainmentMode, deprecated_key_rewrites, translate_deprecated_keys};
pub use crew_auto_effort::{
    AUTO_EFFORT_BANDS_KEY, AUTO_EFFORT_EXPLORE_SHARE, AUTO_EFFORT_POOLS, AutoEffortBand,
    AutoEffortPolicy,
};
pub use crew_pools::{
    ComplexityCrewPools, CrewPoolEntry, PurposeCrewPools, TYPE_COMPLEXITY_CREWS_KEY,
    TypeComplexityCrewPools, canonical_crew_pool, canonical_crew_pool_entries,
    type_complexity_crews_key,
};
pub use crew_task_types::{
    LEGACY_TASK_TYPES_KEY, OTHER_TYPE, TASK_TYPES_KEY, TaskTypeDefinition, classify_task_type,
};
pub use experiment_keys::ExperimentConfigKey;
pub use human_crew_seed::{
    AgentCrewNames, HumanCrewSeedOutcome, human_crew_seed_collides, normalize_human_crew_name,
    os_user_human_crew_name, seed_operator_human_crew,
};
pub use layering::{
    ConfigFallback, ConfigValueSourceKind, ConfigValueState, EffectiveConfigValue, ShadowReason,
    load_effective_config,
};
pub use lock_modes::LOCK_MODES_KEY;
pub use memory_limit::{MemoryLimit, MemoryUnit};
pub use operation::{
    OPERATION_POLICY_VERSION, OperationField, OperationLayerSource, OperationPolicy,
    PRE_LANDING_REVIEW_CREW_KEY, REVIEW_CREW_KEY, REVIEW_MINUTES_KEY, REVIEW_TIMING_KEY,
    ReviewCrewPool, ReviewCrewSetting, ReviewTimingSetting,
};
pub use persistence::PersistenceConfig;
pub use plugin_enablement::{
    MisplacedGlobalPluginEnablement, global_plugin_enablement_fix,
    load_workspace_plugin_enablement, misplaced_global_plugin_enablement, plugin_enablement_key,
    workspace_config_sets_policy,
};
pub use plugins::{
    PluginConfigSchema, plugin_config_schema_generation, register_plugin_config_schemas,
};
pub use provider_limit::{
    CrewSelection, PROVIDER_LIMIT_MAX_USED_PCT, ProviderLimitBudget, ProviderLimitBudgetUnit,
    ProviderLimitExplicitCrews, ProviderLimitPolicy, ProviderLimitTarget, ProviderLimitThreshold,
    USAGE_PRESSURE_CAP, USAGE_PRESSURE_STRENGTH, UsagePressurePolicy,
};
pub use registry::{
    AUTO_CONCURRENCY_ADMIT_MEMORY_PERCENT, AUTO_CONCURRENCY_HEAD_MAX_WAIT_MINUTES,
    AUTO_CONCURRENCY_HEAD_STUCK_PASSES, AUTO_CONCURRENCY_HORIZON_CAP_MINUTES,
    AUTO_CONCURRENCY_PROFILE_WINDOW, AutoConcurrencySettings, CONFIG_KEY_REGISTRY,
    ConfigAllowedValues, ConfigKeyDescriptor, ConfigKeyTier, ConfigSection, ConfigSnapshot,
    ExperimentSettings, MachineSettings, MidRunSteeringSettings, ResourceThrottleSettings,
    WorkerContainmentSettings, admit_config_key, admit_settable_config_key, config_key_options,
    describe as describe_config_key, immutable_key_note, is_global_only_key,
};
pub use regression_layers::RegressionLayerDefinition;
pub use resolved::{
    CodexExecutionPolicy, CompatibilityConfigKey, CompatibilityConfigKeyDisposition,
    ExecutionEnvPolicy, PrSettings, RETIRED_SYSTEM_CREW, RETIRED_SYSTEM_CREW_WARNING,
    ResolvedConfig, compatibility_config_keys_at, compatibility_config_keys_present,
    disabled_crew_message, executor_host_os, names_retired_system_crew,
};
pub use review_skip_tags::{REVIEW_SKIP_TAGS_KEY, ReviewSkipSettings};
pub use roots::ConfigRoots;
pub use seed::{ConfigSeed, seed_default_config, seed_new_machine_defaults, seed_workspace_review};
pub use seeding::{
    DEMOTED_DEFAULT_AUTO_TASKS, RETIRED_DEFAULT_AUTO_TASKS, RETIRED_DEFAULT_ROUTINES,
    SEEDING_AUTO_TASKS_KEY, SEEDING_ROUTINES_KEY, SHIPPED_DEFAULT_AUTO_TASKS,
    SHIPPED_DEFAULT_ROUTINES, SeedingPolicy, WorkspaceSeedingPolicies,
    load_workspace_seeding_policies,
};
pub use store::{ConfigScope, ConfigStore, WorkspaceInitMode};
pub use tag_instructions::{
    AGENT_CORPUS_KEY, TAG_INSTRUCTIONS_KEY, TagInstructionSetting, TagInstructionSettings,
};

/// Read the `[machine]` table from the global `config.toml` at `global_root`.
///
/// Deliberately narrower than [`ResolvedConfig::load`]: this machine's identity
/// is resolved on every runtime open and by `orbit init` before the rest of the
/// document is known to admit, so an unrelated problem elsewhere in the file
/// must not make Orbit forget who it is. A missing file has no identity.
pub fn load_machine_settings(global_root: &Path) -> Result<MachineSettings, OrbitError> {
    let path = global_root.join("config.toml");
    match config_toml::read_config_toml(&path)? {
        Some(document) => MachineSettings::admit(&document, &path),
        None => Ok(MachineSettings::default()),
    }
}

/// Validate the effective (workspace-over-global) `config.toml` without
/// exposing the internal [`ResolvedConfig`] shape. Used by the workspace
/// doctor in `orbit-cmd` [ORB-10016].
/// Store-database path resolved from the layered config. Used by the
/// runtime-less `orbit migrate --dry-run` inspection in `orbit-cmd`
/// [ORB-10016].
pub fn resolved_audit_db_path(roots: &ConfigRoots) -> Result<PathBuf, OrbitError> {
    Ok(ResolvedConfig::load(roots)?.persistence.audit_db)
}

#[cfg(test)]
mod tests;
