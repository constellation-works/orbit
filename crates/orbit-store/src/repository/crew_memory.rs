//! Concrete repository for crew memory: revisions in `crew_memory_revisions`,
//! their audit in `crew_memory_events` and global-memory proposals in
//! `crew_memory_proposals`.
//!
//! Reached only through [`Snapshot::crew_memory`] (read-only) and
//! [`Tx::crew_memory`] (read-write inside [`crate::Store::write`]). The
//! revision and event tables are append-only (triggers refuse updates and
//! deletes). Revision numbering, the per-run proposal cap, decision refusals
//! and stale-read checks belong to the calling operation; this repository
//! owns SQL and row mapping only.

use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::contracts::{
    CrewMemoryClosedProposalRow, CrewMemoryEventRow, CrewMemoryProposalDecision,
    CrewMemoryProposalRow, CrewMemoryRevisionRow,
};
use crate::driver::sqlite::codec::stamp;
use crate::store::{Snapshot, Tx};

const REVISION_COLUMNS: &str = "workspace_id, memory_key, revision, files_json, content_digest, \
     evidence_json, writer_kind, writer_actor, writer_run_id, writer_crew, reason, created_at";

const EVENT_COLUMNS: &str = "workspace_id, memory_key, at, action, actor, revision, detail_json";

const PROPOSAL_COLUMNS: &str =
    "workspace_id, id, proposing_key, proposing_run_id, text, evidence_json, status, created_at";

/// Concrete crew memory repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct CrewMemoryRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> CrewMemoryRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> CrewMemoryRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert `revision` as given, including its `revision` number.
    pub fn put_revision(&self, revision: &CrewMemoryRevisionRow) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO crew_memory_revisions ({REVISION_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
                ),
                params![
                    revision.workspace_id,
                    revision.memory_key,
                    revision.revision,
                    revision.files_json,
                    revision.content_digest,
                    revision.evidence_json,
                    revision.writer_kind,
                    revision.writer_actor,
                    revision.writer_run_id,
                    revision.writer_crew,
                    revision.reason,
                    stamp(revision.created_at),
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Append one audit event.
    pub fn put_event(&self, event: &CrewMemoryEventRow) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO crew_memory_events ({EVENT_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                ),
                params![
                    event.workspace_id,
                    event.memory_key,
                    stamp(event.at),
                    event.action,
                    event.actor,
                    event.revision,
                    event.detail_json,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Insert one proposal.
    pub fn put_proposal(&self, proposal: &CrewMemoryProposalRow) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO crew_memory_proposals ({PROPOSAL_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ),
                params![
                    proposal.workspace_id,
                    proposal.id,
                    proposal.proposing_key,
                    proposal.proposing_run_id,
                    proposal.text,
                    proposal.evidence_json,
                    proposal.status,
                    stamp(proposal.created_at),
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Record `decision` on its proposal while that proposal is still `open`.
    /// Returns whether a row was updated.
    pub fn decide_proposal(
        &self,
        decision: &CrewMemoryProposalDecision,
    ) -> Result<bool, OrbitError> {
        let changed = self
            .conn
            .execute(
                "UPDATE crew_memory_proposals SET status = ?3, decided_by = ?4, \
                 decided_at = ?5, decision_reason = ?6 \
                 WHERE workspace_id = ?1 AND id = ?2 AND status = 'open'",
                params![
                    decision.workspace_id,
                    decision.id,
                    decision.status,
                    decision.actor,
                    stamp(decision.at),
                    decision.reason,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(changed == 1)
    }
}

impl<Access> CrewMemoryRepository<'_, Access> {
    /// The newest revision number of `memory_key`, or 0 when it has none.
    pub fn latest_revision_number(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<u32, OrbitError> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) FROM crew_memory_revisions \
                 WHERE workspace_id = ?1 AND memory_key = ?2",
                params![workspace_id, memory_key],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)
    }

    /// One revision of `memory_key`: `revision`, or the newest when `None`.
    pub fn revision(
        &self,
        workspace_id: &str,
        memory_key: &str,
        revision: Option<u32>,
    ) -> Result<Option<CrewMemoryRevisionRow>, OrbitError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {REVISION_COLUMNS} FROM crew_memory_revisions \
                     WHERE workspace_id = ?1 AND memory_key = ?2 \
                     AND (?3 IS NULL OR revision = ?3) ORDER BY revision DESC LIMIT 1"
                ),
                params![workspace_id, memory_key, revision],
                hydrate_revision,
            )
            .optional()
            .map_err(sqlite_store_error)
    }

    /// Every revision of `memory_key`, newest first.
    pub fn revisions(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<Vec<CrewMemoryRevisionRow>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {REVISION_COLUMNS} FROM crew_memory_revisions \
                 WHERE workspace_id = ?1 AND memory_key = ?2 ORDER BY revision DESC"
            ))
            .map_err(sqlite_store_error)?;
        stmt.query_map(params![workspace_id, memory_key], hydrate_revision)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// The newest revision of every key in the workspace, by key.
    pub fn current(&self, workspace_id: &str) -> Result<Vec<CrewMemoryRevisionRow>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {REVISION_COLUMNS} FROM crew_memory_revisions AS r \
                 WHERE workspace_id = ?1 AND revision = (\
                     SELECT MAX(revision) FROM crew_memory_revisions \
                     WHERE workspace_id = r.workspace_id AND memory_key = r.memory_key) \
                 ORDER BY memory_key"
            ))
            .map_err(sqlite_store_error)?;
        stmt.query_map(params![workspace_id], hydrate_revision)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// The audit events of `memory_key`, oldest first.
    pub fn events(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<Vec<CrewMemoryEventRow>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM crew_memory_events \
                 WHERE workspace_id = ?1 AND memory_key = ?2 ORDER BY seq"
            ))
            .map_err(sqlite_store_error)?;
        stmt.query_map(params![workspace_id, memory_key], hydrate_event)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// How many proposals `run_id` opened in the workspace.
    pub fn count_run_proposals(&self, workspace_id: &str, run_id: &str) -> Result<u32, OrbitError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM crew_memory_proposals \
                 WHERE workspace_id = ?1 AND proposing_run_id = ?2",
                params![workspace_id, run_id],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)
    }

    /// Open proposals, oldest first (then id).
    pub fn open_proposals(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CrewMemoryProposalRow>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {PROPOSAL_COLUMNS} FROM crew_memory_proposals \
                 WHERE workspace_id = ?1 AND status = 'open' ORDER BY created_at, id"
            ))
            .map_err(sqlite_store_error)?;
        stmt.query_map(params![workspace_id], hydrate_proposal)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// Decided proposals, newest decision first, at most `limit`.
    pub fn closed_proposals(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<CrewMemoryClosedProposalRow>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {PROPOSAL_COLUMNS}, decided_by, decided_at, decision_reason \
                 FROM crew_memory_proposals WHERE workspace_id = ?1 AND status != 'open' \
                 ORDER BY decided_at DESC, id DESC LIMIT ?2"
            ))
            .map_err(sqlite_store_error)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        stmt.query_map(params![workspace_id, limit], |row| {
            let decided_at = row
                .get::<_, Option<String>>(9)?
                .map(|at| crate::parse_timestamp(&at))
                .transpose()?;
            Ok(CrewMemoryClosedProposalRow {
                proposal: hydrate_proposal(row)?,
                decided_by: row.get(8)?,
                decided_at,
                decision_reason: row.get(10)?,
            })
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
    }
}

/// The first eight columns of a `crew_memory_proposals` row.
fn hydrate_proposal(row: &Row<'_>) -> rusqlite::Result<CrewMemoryProposalRow> {
    Ok(CrewMemoryProposalRow {
        workspace_id: row.get(0)?,
        id: row.get(1)?,
        proposing_key: row.get(2)?,
        proposing_run_id: row.get(3)?,
        text: row.get(4)?,
        evidence_json: row.get(5)?,
        status: row.get(6)?,
        created_at: crate::parse_timestamp(&row.get::<_, String>(7)?)?,
    })
}

fn hydrate_revision(row: &Row<'_>) -> rusqlite::Result<CrewMemoryRevisionRow> {
    Ok(CrewMemoryRevisionRow {
        workspace_id: row.get(0)?,
        memory_key: row.get(1)?,
        revision: row.get(2)?,
        files_json: row.get(3)?,
        content_digest: row.get(4)?,
        evidence_json: row.get(5)?,
        writer_kind: row.get(6)?,
        writer_actor: row.get(7)?,
        writer_run_id: row.get(8)?,
        writer_crew: row.get(9)?,
        reason: row.get(10)?,
        created_at: crate::parse_timestamp(&row.get::<_, String>(11)?)?,
    })
}

fn hydrate_event(row: &Row<'_>) -> rusqlite::Result<CrewMemoryEventRow> {
    Ok(CrewMemoryEventRow {
        workspace_id: row.get(0)?,
        memory_key: row.get(1)?,
        at: crate::parse_timestamp(&row.get::<_, String>(2)?)?,
        action: row.get(3)?,
        actor: row.get(4)?,
        revision: row.get(5)?,
        detail_json: row.get(6)?,
    })
}
