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

use orbit_common::OrbitError;
use orbit_types::workflow::CODEX_PROVIDER_SANDBOX_MODES;

const DEFAULT_WORKFLOW_BASE_BRANCH: &str = "main";
/// Approval policies `execution.codex.approval_policy` admits.
const CODEX_APPROVAL_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
const SECURITY_ALERT_SEVERITIES: &[&str] = &["low", "moderate", "high", "critical"];
const DEFAULT_WORKFLOW_CREW: &str = "opus";
const LEGACY_DEFAULT_WORKFLOW_CREW: &str = "claude";
const CONSTELLATION_DEFAULT_PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";

/// Live `[crews.<name>]` fields addressable as `crews.<name>.<field>`.
///
/// The crew name is not known at compile time, so these are not registry
/// rows. `orbit config keys` still lists only the fixed settings.
pub(crate) const CREW_CONFIG_FIELDS: &[&str] = &[
    "description",
    "effort",
    "enabled",
    "model",
    "provider",
    "tags",
];

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
    /// `review.*` and `operation.review_crew` — before-PR review and the
    /// automatic-review crew. After-landing review is an auto-task flag, not
    /// a key.
    Review,
    /// Everything else: `automation.*`, `runtime.*`, `tasks.*`,
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
        ConfigSection::Review,
        ConfigSection::Housekeeping,
    ];

    /// Section heading shown by `orbit config show`.
    pub fn title(self) -> &'static str {
        match self {
            Self::Machine => "Machine (machine.*)",
            Self::Delivery => "Delivery (workflow.*)",
            Self::Crews => "Crews (crews.*)",
            Self::Execution => "Execution (execution.*)",
            Self::Review => "Review (review.*, operation.review_crew)",
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
            Self::Review => "before-PR review and the automatic-review crew",
            Self::Housekeeping => "logs, ids, plugins, and PR links",
        }
    }

    /// Dotted prefix every key in the section shares, when there is one.
    /// `Review` and `Housekeeping` span several prefixes and have none, so
    /// their rows print the full key.
    pub fn key_prefix(self) -> Option<&'static str> {
        match self {
            Self::Machine => Some("machine"),
            Self::Delivery => Some("workflow"),
            Self::Crews => Some("crews"),
            Self::Execution => Some("execution"),
            Self::Review => None,
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
            Self::Review => "review",
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

pub(crate) use keys::{
    DEPRECATED_CONFIG_KEYS, REMOVED_CONFIG_KEYS, is_compatibility_config_key,
    is_machine_identity_key, parse_crew_field_key,
};
pub use keys::{
    GLOBAL_ONLY_KEY_PREFIX, admit_config_key, admit_settable_config_key, config_key_options,
    describe, immutable_key_note, is_global_only_key,
};
pub(crate) use settings::read_optional;
pub(crate) use settings::resolve_percent;
pub use settings::{
    AutoConcurrencySettings, CONFIG_KEY_REGISTRY, ConfigSnapshot, MachineSettings,
    MidRunSteeringSettings, ResourceThrottleSettings, WorkerContainmentSettings,
};
