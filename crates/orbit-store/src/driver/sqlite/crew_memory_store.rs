//! Crew memory revisions in `crew_memory_revisions` and their audit in
//! `crew_memory_events`. Both tables are append-only (triggers refuse
//! updates and deletes); a write assigns the key's next revision number and
//! appends its event in one immediate transaction. Global-memory proposals
//! in `crew_memory_proposals` open with their event in one transaction that
//! also enforces the per-run cap.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    CrewMemoryClosedProposalRow, CrewMemoryEventRow, CrewMemoryProposalDecision,
    CrewMemoryProposalOutcome, CrewMemoryProposalRow, CrewMemoryRevisionRow,
    CrewMemoryStoreBackend,
};

use super::codec::stamp;

const REVISION_COLUMNS: &str = "workspace_id, memory_key, revision, files_json, content_digest, \
     evidence_json, writer_kind, writer_actor, writer_run_id, writer_crew, reason, created_at";

const EVENT_COLUMNS: &str = "workspace_id, memory_key, at, action, actor, revision, detail_json";

impl CrewMemoryStoreBackend for Store {
    fn append_crew_memory_revision(
        &self,
        revision: &CrewMemoryRevisionRow,
        event: &CrewMemoryEventRow,
    ) -> Result<CrewMemoryRevisionRow, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let stored = insert_revision(tx.connection(), revision)?;
            let mut event = event.clone();
            event.revision = Some(stored.revision);
            insert_event(tx.connection(), &event)?;
            Ok(stored)
        })
    }

    fn append_crew_memory_event(&self, event: &CrewMemoryEventRow) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            insert_event(tx.connection(), event)
        })
    }

    fn open_crew_memory_proposal(
        &self,
        proposal: &CrewMemoryProposalRow,
        event: &CrewMemoryEventRow,
        max_per_run: u32,
    ) -> Result<CrewMemoryProposalOutcome, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            if let Some(run_id) = &proposal.proposing_run_id {
                let existing: u32 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM crew_memory_proposals \
                         WHERE workspace_id = ?1 AND proposing_run_id = ?2",
                        params![proposal.workspace_id, run_id],
                        |row| row.get(0),
                    )
                    .map_err(sqlite_store_error)?;
                if existing >= max_per_run {
                    return Ok(CrewMemoryProposalOutcome::CapReached { existing });
                }
            }
            conn.execute(
                "INSERT INTO crew_memory_proposals (workspace_id, id, proposing_key, \
                 proposing_run_id, text, evidence_json, status, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
            insert_event(conn, event)?;
            Ok(CrewMemoryProposalOutcome::Opened)
        })
    }

    fn crew_memory_open_proposals(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CrewMemoryProposalRow>, OrbitError> {
        let conn = self.read()?;
        open_proposals(&conn, workspace_id)
    }

    fn crew_memory_closed_proposals(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<CrewMemoryClosedProposalRow>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT workspace_id, id, proposing_key, proposing_run_id, text, evidence_json, \
                 status, created_at, decided_by, decided_at, decision_reason \
                 FROM crew_memory_proposals WHERE workspace_id = ?1 AND status != 'open' \
                 ORDER BY decided_at DESC, id DESC LIMIT ?2",
            )
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

    fn expire_crew_memory_proposals(
        &self,
        workspace_id: &str,
        before: DateTime<Utc>,
        at: DateTime<Utc>,
    ) -> Result<usize, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let expired: Vec<_> = open_proposals(conn, workspace_id)?
                .into_iter()
                .filter(|proposal| proposal.created_at < before)
                .collect();
            for proposal in &expired {
                conn.execute(
                    "UPDATE crew_memory_proposals SET status = 'withdrawn', decided_by = 'system', \
                     decided_at = ?3, decision_reason = 'expired after 4 weeks' \
                     WHERE workspace_id = ?1 AND id = ?2 AND status = 'open'",
                    params![workspace_id, proposal.id, stamp(at)],
                )
                .map_err(sqlite_store_error)?;
                insert_event(
                    conn,
                    &CrewMemoryEventRow {
                        workspace_id: workspace_id.to_string(),
                        memory_key: "global".to_string(),
                        at,
                        action: "proposal_expired".to_string(),
                        actor: "system".to_string(),
                        revision: None,
                        detail_json: serde_json::json!({
                            "proposal_id": proposal.id,
                            "proposing_key": proposal.proposing_key,
                            "reason": "expired after 4 weeks",
                        })
                        .to_string(),
                    },
                )?;
            }
            Ok(expired.len())
        })
    }

    fn decide_crew_memory_proposal(
        &self,
        decision: &CrewMemoryProposalDecision,
        event: &CrewMemoryEventRow,
        revision: Option<(&CrewMemoryRevisionRow, u32)>,
    ) -> Result<Option<CrewMemoryRevisionRow>, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let changed = conn.execute(
                "UPDATE crew_memory_proposals SET status = ?3, decided_by = ?4, \
                 decided_at = ?5, decision_reason = ?6 \
                 WHERE workspace_id = ?1 AND id = ?2 AND status = 'open'",
                params![decision.workspace_id, decision.id, decision.status,
                    decision.actor, stamp(decision.at), decision.reason],
            ).map_err(sqlite_store_error)?;
            if changed != 1 {
                return Err(OrbitError::InvalidInput(format!(
                    "crew memory proposal `{}` is not open or does not exist", decision.id
                )));
            }
            let stored = if let Some((row, expected)) = revision {
                let current: u32 = conn.query_row(
                    "SELECT COALESCE(MAX(revision), 0) FROM crew_memory_revisions \
                     WHERE workspace_id = ?1 AND memory_key = ?2",
                    params![row.workspace_id, row.memory_key],
                    |row| row.get(0),
                ).map_err(sqlite_store_error)?;
                if current != expected {
                    return Err(OrbitError::InvalidInput(format!(
                        "global memory changed from revision {expected} to {current}; read it again before accepting the proposal"
                    )));
                }
                Some(insert_revision(conn, row)?)
            } else {
                None
            };
            let mut event = event.clone();
            event.revision = stored.as_ref().map(|row| row.revision);
            insert_event(conn, &event)?;
            Ok(stored)
        })
    }

    fn crew_memory_revision(
        &self,
        workspace_id: &str,
        memory_key: &str,
        revision: Option<u32>,
    ) -> Result<Option<CrewMemoryRevisionRow>, OrbitError> {
        let conn = self.read()?;
        conn.query_row(
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

    fn crew_memory_revisions(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<Vec<CrewMemoryRevisionRow>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
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

    fn crew_memory_current(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CrewMemoryRevisionRow>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
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

    fn crew_memory_events(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<Vec<CrewMemoryEventRow>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
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
}

fn open_proposals(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Vec<CrewMemoryProposalRow>, OrbitError> {
    let mut stmt = conn.prepare(
        "SELECT workspace_id, id, proposing_key, proposing_run_id, text, evidence_json, status, created_at \
         FROM crew_memory_proposals WHERE workspace_id = ?1 AND status = 'open' ORDER BY created_at, id"
    ).map_err(sqlite_store_error)?;
    stmt.query_map(params![workspace_id], hydrate_proposal)
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
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

fn insert_revision(
    conn: &Connection,
    revision: &CrewMemoryRevisionRow,
) -> Result<CrewMemoryRevisionRow, OrbitError> {
    let newest: Option<u32> = conn
        .query_row(
            "SELECT MAX(revision) FROM crew_memory_revisions \
                     WHERE workspace_id = ?1 AND memory_key = ?2",
            params![revision.workspace_id, revision.memory_key],
            |row| row.get(0),
        )
        .map_err(sqlite_store_error)?;
    let mut stored = revision.clone();
    stored.revision = newest.unwrap_or(0).saturating_add(1);
    conn.execute(
        &format!(
            "INSERT INTO crew_memory_revisions ({REVISION_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        ),
        params![
            stored.workspace_id,
            stored.memory_key,
            stored.revision,
            stored.files_json,
            stored.content_digest,
            stored.evidence_json,
            stored.writer_kind,
            stored.writer_actor,
            stored.writer_run_id,
            stored.writer_crew,
            stored.reason,
            stamp(stored.created_at),
        ],
    )
    .map_err(sqlite_store_error)?;
    Ok(stored)
}

fn insert_event(conn: &Connection, event: &CrewMemoryEventRow) -> Result<(), OrbitError> {
    conn.execute(
        &format!(
            "INSERT INTO crew_memory_events ({EVENT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
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
