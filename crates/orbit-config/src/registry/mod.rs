//! Admission registry for fixed `config.toml` settings.
//!
//! Each setting is declared once in `define_config_settings!`. That row
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

use crate::memory_limit::MemoryLimit;
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
    /// `machine.*` — this machine's identity and host-level worker limits.
    /// Global-only: a workspace `config.toml` may neither supply nor override
    /// it.
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
            Self::Machine => "who this machine is and how it bounds workers (global config only)",
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

mod keys;
mod settings;

pub use keys::{
    GLOBAL_ONLY_KEY_PREFIX, admit_config_key, admit_settable_config_key, config_key_options,
    describe, is_global_only_key,
};
pub(crate) use keys::{
    REMOVED_CONFIG_KEYS, is_machine_identity_key, parse_crew_field_key, removed_key_note,
};
pub(crate) use settings::read_optional;
#[cfg(test)]
pub(crate) use settings::resolve_default_crew;
pub use settings::{
    CONFIG_KEY_REGISTRY, ConfigSnapshot, MachineSettings, WorkerContainmentSettings,
};
