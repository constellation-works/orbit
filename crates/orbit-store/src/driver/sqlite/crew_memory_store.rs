//! Crew memory revisions in `crew_memory_revisions` and their audit in
//! `crew_memory_events`. Both tables are append-only (triggers refuse
//! updates and deletes); a write assigns the key's next revision number and
//! appends its event in one immediate transaction.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{CrewMemoryEventRow, CrewMemoryRevisionRow, CrewMemoryStoreBackend};

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
            let conn = tx.connection();
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
            let mut event = event.clone();
            event.revision = Some(stored.revision);
            insert_event(conn, &event)?;
            Ok(stored)
        })
    }

    fn append_crew_memory_event(&self, event: &CrewMemoryEventRow) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            insert_event(tx.connection(), event)
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
