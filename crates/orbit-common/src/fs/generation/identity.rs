//! What makes two Orbit executables safe to run against one authority.
//!
//! A [`CompatibilityIdentity`] is compiled into every binary: for each state
//! ledger it names the newest version the binary produces and the newest
//! migration an older binary could not keep writing through
//! (`writer_floor`) or reading (`reader_floor`). Feature schema ledgers fail
//! closed on any newer version, so they must match exactly.
//!
//! The authority records the [`Envelope`] of every identity admitted since it
//! last had no participant. A newcomer is compatible with the envelope when
//! no migration either side lacks would break the other's reads or writes.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One state ledger, as a binary compiled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerCompatibility {
    /// Newest version this binary produces.
    pub version: u32,
    /// Newest migration at or below `version` an older binary cannot keep
    /// writing through (0 when every migration keeps older writers safe).
    pub writer_floor: u32,
    /// Newest migration at or below `version` an older binary cannot read.
    pub reader_floor: u32,
}

impl LedgerCompatibility {
    /// Summarize a migration registry of `(version, write_safe, read_safe)`.
    pub fn from_registry<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (u32, bool, bool)>,
    {
        let mut ledger = Self {
            version: 0,
            writer_floor: 0,
            reader_floor: 0,
        };
        for (version, write_safe, read_safe) in entries {
            ledger.version = ledger.version.max(version);
            if !write_safe {
                ledger.writer_floor = ledger.writer_floor.max(version);
            }
            if !read_safe {
                ledger.reader_floor = ledger.reader_floor.max(version);
            }
        }
        ledger
    }
}

/// The state compatibility a binary was compiled with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityIdentity {
    /// The SQLite store schema ledger.
    pub store_schema: LedgerCompatibility,
    /// The `.orbit/` workspace layout ledger.
    pub workspace_layout: LedgerCompatibility,
    /// Feature-owned schema ledgers by name, at the version this binary
    /// migrates them to.
    #[serde(default)]
    pub features: BTreeMap<String, u32>,
}

impl fmt::Display for CompatibilityIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "store schema {} (writers from {}, readers from {}), workspace layout {} \
             (writers from {}, readers from {})",
            self.store_schema.version,
            self.store_schema.writer_floor,
            self.store_schema.reader_floor,
            self.workspace_layout.version,
            self.workspace_layout.writer_floor,
            self.workspace_layout.reader_floor,
        )?;
        for (feature, version) in &self.features {
            write!(f, ", {feature} {version}")?;
        }
        Ok(())
    }
}

/// Whether a participant writes Orbit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// May migrate and write any state.
    Write,
    /// Observes state without writing it.
    ReadOnly,
}

/// Everything admitted to one ledger since the authority last had no
/// participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LedgerEnvelope {
    min_version: u32,
    max_version: u32,
    max_reader_floor: u32,
    max_writer_floor: u32,
    /// Oldest version among participants that write, if any did.
    writer_min_version: Option<u32>,
}

impl LedgerEnvelope {
    fn of(ledger: LedgerCompatibility, access: Access) -> Self {
        Self {
            min_version: ledger.version,
            max_version: ledger.version,
            max_reader_floor: ledger.reader_floor,
            max_writer_floor: ledger.writer_floor,
            writer_min_version: (access == Access::Write).then_some(ledger.version),
        }
    }

    fn widened(self, ledger: LedgerCompatibility, access: Access) -> Self {
        Self {
            min_version: self.min_version.min(ledger.version),
            max_version: self.max_version.max(ledger.version),
            max_reader_floor: self.max_reader_floor.max(ledger.reader_floor),
            max_writer_floor: self.max_writer_floor.max(ledger.writer_floor),
            writer_min_version: match (self.writer_min_version, access) {
                (Some(min), Access::Write) => Some(min.min(ledger.version)),
                (None, Access::Write) => Some(ledger.version),
                (min, Access::ReadOnly) => min,
            },
        }
    }

    /// Why `ledger` cannot join, if it cannot.
    ///
    /// Reads must survive in both directions: nothing a newer member applied
    /// may break an older one's reads. Rows written by the oldest writer must
    /// stay correct for everyone newer, and a newcomer that writes must be
    /// write-safe for every newer member.
    fn refusal(&self, ledger: LedgerCompatibility, access: Access) -> Option<String> {
        if ledger.reader_floor > self.min_version {
            return Some(format!(
                "migration v{} cannot be read by a live participant at version {}",
                ledger.reader_floor, self.min_version
            ));
        }
        if self.max_reader_floor > ledger.version {
            return Some(format!(
                "a live participant carries migration v{}, which version {} cannot read",
                self.max_reader_floor, ledger.version
            ));
        }
        if let Some(writer_min) = self.writer_min_version
            && ledger.writer_floor > writer_min
        {
            return Some(format!(
                "a live writer at version {writer_min} predates migration v{}, which older \
                 writers do not keep",
                ledger.writer_floor
            ));
        }
        if access == Access::Write && self.max_writer_floor > ledger.version {
            return Some(format!(
                "a live participant carries migration v{}, which a writer at version {} does \
                 not keep",
                self.max_writer_floor, ledger.version
            ));
        }
        None
    }
}

/// The identities an authority admitted since it last had no participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Envelope {
    store_schema: LedgerEnvelope,
    workspace_layout: LedgerEnvelope,
    #[serde(default)]
    features: BTreeMap<String, u32>,
}

impl Envelope {
    pub(super) fn of(identity: &CompatibilityIdentity, access: Access) -> Self {
        Self {
            store_schema: LedgerEnvelope::of(identity.store_schema, access),
            workspace_layout: LedgerEnvelope::of(identity.workspace_layout, access),
            features: identity.features.clone(),
        }
    }

    pub(super) fn widened(&self, identity: &CompatibilityIdentity, access: Access) -> Self {
        Self {
            store_schema: self.store_schema.widened(identity.store_schema, access),
            workspace_layout: self
                .workspace_layout
                .widened(identity.workspace_layout, access),
            features: self.features.clone(),
        }
    }

    /// Why `identity` cannot join the live participants, if it cannot.
    pub(super) fn refusal(
        &self,
        identity: &CompatibilityIdentity,
        access: Access,
    ) -> Option<String> {
        if let Some(reason) = self.store_schema.refusal(identity.store_schema, access) {
            return Some(format!("store schema: {reason}"));
        }
        if let Some(reason) = self
            .workspace_layout
            .refusal(identity.workspace_layout, access)
        {
            return Some(format!("workspace layout: {reason}"));
        }
        if self.features != identity.features {
            return Some(format!(
                "feature schemas differ (live {}, this binary {})",
                describe_features(&self.features),
                describe_features(&identity.features)
            ));
        }
        None
    }

    /// Whether `identity` is at least as new as every live participant in
    /// every ledger — an upgrade the live participants should yield to, as
    /// opposed to an older binary that must not displace newer ones.
    pub(super) fn is_superseded_by(&self, identity: &CompatibilityIdentity) -> bool {
        identity.store_schema.version >= self.store_schema.max_version
            && identity.workspace_layout.version >= self.workspace_layout.max_version
            && self.features.iter().all(|(feature, version)| {
                identity
                    .features
                    .get(feature)
                    .is_some_and(|candidate| candidate >= version)
            })
    }
}

fn describe_features(features: &BTreeMap<String, u32>) -> String {
    if features.is_empty() {
        return "none".to_string();
    }
    features
        .iter()
        .map(|(feature, version)| format!("{feature} {version}"))
        .collect::<Vec<_>>()
        .join(", ")
}
