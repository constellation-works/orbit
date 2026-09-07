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
//! Three security-sensitive settings are replace-only when a workspace file
//! exists: `execution.codex.sandbox`, `execution.codex.approval_policy`, and
//! `execution.env.pass`. An omitted replace-only setting uses its built-in default
//! rather than inheriting a machine-specific global policy.
//!
//! # Role
//!
//! A leaf above `orbit-common`: this crate performs no runtime composition and
//! depends on no higher layer. In particular it does not know about
//! `orbit-core` path discovery (callers supply an explicit [`ConfigRoots`]),
//! about `orbit-engine` (PR settings are exposed as config-owned
//! [`PrSettings`] and translated at composition time), or about a terminal
//! (host detection and interactive prompting belong to the CLI init adapter,
//! which hands this crate an explicit [`ConfigSeed`]).
//!
//! # Module map
//!
//! - `roots` — the explicit two-root input ([`ConfigRoots`]).
//! - `raw` — private serde schema for the parts of the document that are not
//!   fixed registry keys (crew tables and retired-key migration guards).
//! - `registry` — the fixed-key registry and its admitted [`ConfigSnapshot`].
//! - `layering` — document reading, per-key merge, replace-only rules, and
//!   source provenance.
//! - `operation` — typed operation-mode preferences and their layered
//!   preset-reset resolution [ORB-11332].
//! - `resolved` — the consumer-facing [`ResolvedConfig`] views.
//! - `persistence` — artifact path resolution from the two roots.
//! - `store` — comment-preserving [`ConfigStore`] edits and atomic save.
//! - `seed` — rendering and writing a fresh default `config.toml`.

mod crew_pools;
mod layering;
pub mod operation;
mod persistence;
mod raw;
mod registry;
mod resolved;
mod roots;
mod seed;
mod store;

use std::path::PathBuf;

use orbit_common::OrbitError;

pub use crew_pools::{ComplexityCrewPools, canonical_crew_pool};
pub use layering::{
    ConfigValueSource, ConfigValueSourceKind, EffectiveConfig, EffectiveConfigValue,
    load_effective_config,
};
pub use operation::{
    CompletionPreference, DeliveryCap, OPERATION_POLICY_VERSION, OperationField,
    OperationFieldSource, OperationLayer, OperationLayerSource, OperationPolicy, OperationPreset,
    PreparationPreference, PromotionPreference, RecoveryPreference, ReviewPolicy,
};
pub use persistence::PersistenceConfig;
pub use raw::CrewSeed;
pub use registry::{
    CONFIG_KEY_REGISTRY, ConfigKeyDescriptor, ConfigSnapshot, admit_config_key,
    describe as describe_config_key,
};
pub use resolved::{CodexExecutionPolicy, ExecutionEnvPolicy, PrSettings, ResolvedConfig};
pub use roots::ConfigRoots;
pub use seed::{ConfigSeed, seed_default_config};
pub use store::{ConfigScope, ConfigStore, WorkspaceInitMode};

/// Validate the effective (workspace-over-global) `config.toml` without
/// exposing the internal [`ResolvedConfig`] shape. Used by the workspace
/// doctor in `orbit-cmd` [ORB-10016].
pub fn validate_layered_config(roots: &ConfigRoots) -> Result<(), OrbitError> {
    ResolvedConfig::load(roots).map(|_| ())
}

/// Store-database path resolved from the layered config. Used by the
/// runtime-less `orbit migrate --dry-run` inspection in `orbit-cmd`
/// [ORB-10016].
pub fn resolved_audit_db_path(roots: &ConfigRoots) -> Result<PathBuf, OrbitError> {
    Ok(ResolvedConfig::load(roots)?.persistence.audit_db)
}

#[cfg(test)]
mod tests;
