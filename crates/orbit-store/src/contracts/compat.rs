//! Forward-compatibility contract shared by Orbit's two state ledgers
//! (ORB-12434).
//!
//! Both ledgers — the `.orbit/` workspace layout registry and the SQLite
//! schema ledger ([`crate::maintenance::migration`]) — used to refuse *any* state whose
//! recorded version exceeded the running binary's supported version. That
//! made every version bump a flag day: a stale binary anywhere on a host
//! failed every command, including write-free ones.
//!
//! The version number alone cannot answer "is this safe to read?", because a
//! binary knows nothing about migrations that shipped after it. So the
//! *newer* binary leaves the answer behind: whenever it applies a migration
//! it records a [`CompatibilityRecord`] listing the migrations it considers
//! [`MigrationCompatibility::Breaking`]. An older binary reads that record
//! and decides:
//!
//! - no breaking migration above its supported version → open read-only
//!   ([`ForwardCompatibleOpen`]);
//! - otherwise → refuse, naming the first breaking migration it lacks
//!   ([`CompatibilityRefusal`]).
//!
//! The record is conservative by construction. A missing, stale, corrupt, or
//! future-format record refuses exactly as before, so state written by
//! binaries that predate this contract keeps the old behaviour.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Record format this binary writes. A reader that meets a higher format
/// refuses rather than guessing; format 1 is therefore extended only by
/// adding optional fields (unknown fields are ignored on decode).
pub const COMPATIBILITY_RECORD_FORMAT: u32 = 1;

/// What a shipped migration means for a binary that does not have it.
///
/// This is a claim about *readers*, made by the migration's author:
///
/// - [`Self::Additive`] — the migration only adds state (new tables,
///   columns, indexes, files) or rewrites state into a shape older binaries
///   already understand. A binary without this migration still reads the
///   state correctly and ignores what it does not know.
/// - [`Self::Breaking`] — the migration removes, renames, or reinterprets
///   state that an older binary reads or writes. Older binaries must refuse.
///
/// When in doubt, declare [`Self::Breaking`]: the cost is the old flag-day
/// behaviour, while a wrong `Additive` claim hands an old binary a store it
/// misreads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCompatibility {
    Additive,
    Breaking,
}

impl MigrationCompatibility {
    pub fn is_breaking(self) -> bool {
        matches!(self, Self::Breaking)
    }
}

/// Which versioned state a compatibility decision is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateComponent {
    WorkspaceLayout,
    StoreSchema,
}

impl StateComponent {
    /// Human label used in diagnostics and reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkspaceLayout => "workspace layout",
            Self::StoreSchema => "store schema",
        }
    }
}

impl fmt::Display for StateComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One breaking migration, as recorded for binaries that lack it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakingMigration {
    pub version: u32,
    pub name: String,
}

/// What a newer binary leaves behind so older binaries can decide whether
/// they may still read this state.
///
/// `version` is the ledger version the record describes: a record older than
/// the recorded state version says nothing about the migrations in between,
/// so readers treat it as stale and refuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityRecord {
    /// Record format ([`COMPATIBILITY_RECORD_FORMAT`]).
    pub format: u32,
    /// Ledger version this record describes.
    pub version: u32,
    /// Every breaking migration at or below `version`, ascending.
    #[serde(default)]
    pub breaking: Vec<BreakingMigration>,
}

impl CompatibilityRecord {
    /// Build the record describing `version` from the writing binary's full
    /// registry, as `(version, name, compatibility)` triples in apply order.
    pub fn for_registry<'a, I>(version: u32, registry: I) -> Self
    where
        I: IntoIterator<Item = (u32, &'a str, MigrationCompatibility)>,
    {
        let breaking = registry
            .into_iter()
            .filter(|(entry_version, _, compat)| compat.is_breaking() && *entry_version <= version)
            .map(|(entry_version, name, _)| BreakingMigration {
                version: entry_version,
                name: name.to_string(),
            })
            .collect();
        Self {
            format: COMPATIBILITY_RECORD_FORMAT,
            version,
            breaking,
        }
    }

    /// Lowest binary-supported version that may still read this state: the
    /// newest breaking migration it carries (0 when it carries none).
    pub fn min_reader_version(&self) -> u32 {
        self.breaking
            .iter()
            .map(|entry| entry.version)
            .max()
            .unwrap_or(0)
    }

    /// Serialize for storage (single-line JSON).
    pub fn encode(&self) -> Result<String, orbit_common::OrbitError> {
        serde_json::to_string(self).map_err(|error| {
            orbit_common::OrbitError::Migration(format!(
                "cannot serialize forward-compatibility record: {error}"
            ))
        })
    }

    /// Parse a stored record. A parse failure is a refusal reason, not an
    /// error: the caller already knows the state is newer and only needs to
    /// explain why it will not open it.
    pub fn decode(raw: &str) -> Result<Self, CompatibilityRefusal> {
        serde_json::from_str::<Self>(raw)
            .map_err(|error| CompatibilityRefusal::CorruptRecord(error.to_string()))
    }
}

/// A newer state this binary may open, read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardCompatibleOpen {
    pub component: StateComponent,
    /// Version recorded in the state.
    pub state_version: u32,
    /// Newest version this binary supports.
    pub supported_version: u32,
    /// Newest breaking migration the state carries.
    pub min_reader_version: u32,
}

impl fmt::Display for ForwardCompatibleOpen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} version {} is newer than this binary's supported version {}, \
             but only by additive migrations; opened read-only",
            self.component, self.state_version, self.supported_version
        )
    }
}

/// Why a newer state cannot be opened at all, even read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatibilityRefusal {
    /// Written by a binary that predates this contract (or by one that never
    /// applied a migration to this state).
    NoRecord,
    /// The record describes an older version than the state carries, so the
    /// migrations in between are unclassified.
    StaleRecord { recorded: u32 },
    /// A record format this binary does not understand.
    UnknownRecordFormat { format: u32 },
    /// The record is present but unreadable.
    CorruptRecord(String),
    /// The first breaking migration this binary lacks.
    Breaking(BreakingMigration),
}

impl fmt::Display for CompatibilityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRecord => f.write_str(
                "it records no forward-compatibility metadata, so this binary cannot tell \
                 whether the newer migrations are additive",
            ),
            Self::StaleRecord { recorded } => write!(
                f,
                "its forward-compatibility record only describes version {recorded}, \
                 leaving the newer migrations unclassified"
            ),
            Self::UnknownRecordFormat { format } => write!(
                f,
                "its forward-compatibility record uses format {format}, which this binary \
                 does not understand"
            ),
            Self::CorruptRecord(detail) => write!(
                f,
                "its forward-compatibility record is unreadable ({detail})"
            ),
            Self::Breaking(entry) => write!(
                f,
                "migration v{} ({}) is a breaking change this binary does not have",
                entry.version, entry.name
            ),
        }
    }
}

/// Decide whether a state recorded at `state_version` may be opened
/// read-only by a binary supporting `supported_version`.
///
/// Call only when `state_version > supported_version`; a state at or below
/// the supported version opens normally and never consults a record.
pub fn evaluate_newer_state(
    component: StateComponent,
    state_version: u32,
    supported_version: u32,
    record: Option<CompatibilityRecord>,
) -> Result<ForwardCompatibleOpen, CompatibilityRefusal> {
    debug_assert!(state_version > supported_version);
    let Some(record) = record else {
        return Err(CompatibilityRefusal::NoRecord);
    };
    if record.format != COMPATIBILITY_RECORD_FORMAT {
        return Err(CompatibilityRefusal::UnknownRecordFormat {
            format: record.format,
        });
    }
    if record.version < state_version {
        return Err(CompatibilityRefusal::StaleRecord {
            recorded: record.version,
        });
    }
    if let Some(missing) = record
        .breaking
        .iter()
        .filter(|entry| entry.version > supported_version)
        .min_by_key(|entry| entry.version)
    {
        return Err(CompatibilityRefusal::Breaking(missing.clone()));
    }
    Ok(ForwardCompatibleOpen {
        component,
        state_version,
        supported_version,
        min_reader_version: record.min_reader_version(),
    })
}
