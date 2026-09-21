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
//! - `operation` — typed `[operation]` review preferences and their layered
//!   resolution [ORB-11333].
//! - `resolved` — the consumer-facing [`ResolvedConfig`] views.
//! - `persistence` — artifact path resolution from the two roots.
//! - `store` — comment-preserving [`ConfigStore`] edits and atomic save.
//! - `seed` — rendering and writing a fresh default `config.toml`.

mod crew_pools;
mod layering;
pub mod operation;
mod persistence;
pub mod plugins;
mod raw;
mod registry;
mod resolved;
mod roots;
mod seed;
mod store;

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::open_read_only_no_follow;
use orbit_common::security::redaction::redact_home_dir;

pub use crew_pools::{
    CanonicalCrewPool, ComplexityCrewPools, CrewPoolEntry, canonical_crew_pool,
    canonical_crew_pool_entries,
};
pub use layering::{
    ConfigValueSource, ConfigValueSourceKind, ConfigValueState, EffectiveConfig,
    EffectiveConfigValue, ShadowReason, ShadowedConfigValue, load_effective_config,
};
pub use operation::{
    OPERATION_POLICY_VERSION, OperationField, OperationLayer, OperationLayerSource,
    OperationPolicy, ReviewPolicy,
};
pub use persistence::PersistenceConfig;
pub use plugins::{
    PLUGIN_CONFIG_PREFIX, PluginConfigSchema, PluginFieldKey, parse_plugin_field_key,
    plugin_config_schema, register_plugin_config_schemas, registered_plugin_namespaces,
    validate_plugin_sections,
};
pub use registry::{
    CONFIG_KEY_REGISTRY, ConfigKeyDescriptor, ConfigSection, ConfigSnapshot,
    GLOBAL_ONLY_KEY_PREFIX, MachineSettings, admit_config_key, admit_settable_config_key,
    config_key_options, describe as describe_config_key, is_global_only_key,
};
pub use resolved::{
    CodexExecutionPolicy, ExecutionEnvPolicy, IgnoredCrewProperty, PrSettings, ResolvedConfig,
};
pub use roots::ConfigRoots;
pub use seed::{ConfigSeed, seed_default_config};
pub use store::{ConfigScope, ConfigStore, WorkspaceInitMode};

const MACHINE_SETTINGS_FILE: &str = "config.toml";

/// Read the `[machine]` table from the global `config.toml` at `global_root`.
///
/// Deliberately narrower than [`ResolvedConfig::load`]: this machine's identity
/// is resolved on every runtime open and by `orbit init` before the rest of the
/// document is known to admit, so an unrelated problem elsewhere in the file
/// must not make Orbit forget who it is. A missing file has no identity.
pub fn load_machine_settings(global_root: &Path) -> Result<MachineSettings, OrbitError> {
    let Some(path) = validated_machine_settings_path(global_root)? else {
        return Ok(MachineSettings::default());
    };
    let mut file = match open_read_only_no_follow(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MachineSettings::default());
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to read runtime config '{}': {error}",
                redact_home_dir(&path.display().to_string())
            )));
        }
    };
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read runtime config '{}': {error}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    let document = toml::from_str::<toml::Value>(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {error}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    MachineSettings::admit(&document, &path)
}

fn machine_settings_path_error(message: &str, path: &Path) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "{message}: {}",
        redact_home_dir(&path.display().to_string())
    ))
}

/// Resolve the global `config.toml` before any identity read.
///
/// Callers pass a selected Orbit global root. The root is canonicalized so
/// directory aliases collapse, then the fixed filename is rejoined. The
/// reconstructed path is prefix-checked with `Path::starts_with` in this
/// function (CodeQL `rust/path-injection` SafeAccessCheck on the receiver)
/// so the leaf metadata probe and later open/read sinks only see a
/// prefix-checked value. A missing root or missing file is treated as no
/// identity. A present leaf that is a symlink or non-file is refused.
fn validated_machine_settings_path(global_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_root = match global_root.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to canonicalize machine settings directory '{}': {error}",
                redact_home_dir(&global_root.display().to_string())
            )));
        }
    };
    let candidate = canonical_root.join(MACHINE_SETTINGS_FILE);
    // `Path::starts_with` is CodeQL's rust/path-injection SafeAccessCheck
    // BarrierGuard on the receiver; a helper wrapping it is not.
    if !candidate.starts_with(&canonical_root) {
        return Err(machine_settings_path_error(
            "machine settings path escapes its parent",
            &candidate,
        ));
    }

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(machine_settings_path_error(
            "machine settings path must not be a symlink",
            &candidate,
        )),
        Ok(metadata) if !metadata.is_file() => Err(machine_settings_path_error(
            "machine settings path must be a regular file",
            &candidate,
        )),
        Ok(_) => Ok(Some(candidate)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect runtime config '{}': {error}",
            redact_home_dir(&candidate.display().to_string())
        ))),
    }
}

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
