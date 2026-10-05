//! Parameter and projection types for the SQLite friction store (ORB-10680).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_types::record::{FrictionRecord, FrictionStatus};

/// Everything `orbit.friction.add` needs to allocate and persist a record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrictionAddParams {
    pub model: String,
    /// The record's handle. Callers pass the author's title, or `None` to let
    /// the store derive one from the body.
    pub title: Option<String>,
    pub body: String,
    pub tags: Vec<String>,
    pub during_task: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Predicate and page for a friction read.
///
/// Every field here is pushed into SQL. The store never decodes a row the
/// caller did not ask for, so peak memory tracks `limit`, not corpus size.
#[derive(Debug, Clone, Default)]
pub struct FrictionListFilter {
    pub model: Option<String>,
    pub status: Option<FrictionStatus>,
    pub tag: Option<String>,
    /// Substring match across id, title, model, status, `during_task`, tags,
    /// and body. Matched case-insensitively inside SQLite.
    pub q: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Maximum rows to decode. `None` means "every match".
    pub limit: Option<usize>,
    /// Rows to skip before decoding, applied by SQLite.
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct FrictionUpdateParams {
    pub status: Option<FrictionStatus>,
    pub tags: Option<Vec<String>>,
    /// `Some(Some(title))` sets the stored title; `Some(None)` clears it and
    /// restores derivation from the body.
    pub title: Option<Option<String>>,
    pub body: Option<String>,
    pub resolved_by_task: Option<String>,
    /// `Some(Some(workspace))` records the owning workspace (curation's
    /// `rehome_required` disposition); `Some(None)` clears it.
    pub rehome_to: Option<Option<String>>,
    pub updated_at: DateTime<Utc>,
}

/// Where `orbit.friction.rehome` moves a record, resolved by the caller.
///
/// Both workspaces share this host's store, so the move is one transaction:
/// the owning workspace gains a copy and the source record is resolved with a
/// pointer to it, or neither happens.
#[derive(Debug, Clone)]
pub struct FrictionRehomeParams {
    /// The owning workspace's friction partition, its `workspace_id()`.
    pub target_workspace_id: String,
    /// The owning workspace's friction root; its tag taxonomy lives there.
    pub target_files_root: PathBuf,
    /// Registered name of the owning workspace, recorded in `rehome_to` and in
    /// the source's forwarding note.
    pub target_label: String,
    /// Registered name of the source workspace, recorded in the moved copy's
    /// provenance note.
    pub source_label: String,
    pub rehomed_at: DateTime<Utc>,
}

/// Both halves of a completed re-home.
#[derive(Debug, Clone)]
pub struct FrictionRehomeOutcome {
    /// The source record, now resolved and pointing at `target`.
    pub source: StoredFrictionRecord,
    /// The copy in the owning workspace, under an ID allocated there.
    pub target: StoredFrictionRecord,
    /// Source tags the owning workspace's taxonomy does not define.
    pub dropped_tags: Vec<String>,
}

/// Persisted friction record wrapper. The identity in `record.model` is
/// per-invocation actual execution (sourced from the friction add call site).
#[derive(Debug, Clone)]
pub struct StoredFrictionRecord {
    pub record: FrictionRecord,
    /// The legacy Markdown file this record was imported from, retained as
    /// read-only rollback evidence for one release.
    ///
    /// `None` for every record written after the SQLite cutover — the wire
    /// projection reports `null` rather than inventing a path that no reader
    /// could open (ADR-0345).
    pub path: Option<PathBuf>,
}

/// Friction count for one reporting model label over a caller-chosen window.
///
/// The scoreboard consumes this instead of the record slice it used to scan:
/// the row count is bounded by distinct model labels, not by corpus size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrictionReportedCount {
    pub model: String,
    pub count: u64,
}

/// Normalize tags against the owning workspace taxonomy before publication.
pub fn normalize_friction_tags(
    raw_tags: Vec<String>,
    taxonomy: &std::collections::BTreeSet<String>,
) -> Result<Vec<String>, orbit_common::OrbitError> {
    let mut tags = std::collections::BTreeSet::new();
    for raw in raw_tags {
        let value = raw.trim().to_ascii_lowercase();
        if !value.is_empty() {
            tags.insert(value);
        }
    }
    if tags.is_empty() {
        tags.insert("other".to_string());
    }
    let invalid = tags
        .iter()
        .filter(|tag| !taxonomy.contains(*tag))
        .cloned()
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(orbit_common::OrbitError::InvalidInput(format!(
            "unknown friction tag(s): {}. valid tags: {}",
            invalid.join(", "),
            taxonomy.iter().cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(tags.into_iter().collect())
}
