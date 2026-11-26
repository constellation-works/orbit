//! Concrete repository for fleet notices (`notices` and `notice_events`).
//!
//! Reached only through [`Snapshot::notice`] (read-only) and [`Tx::notice`]
//! (read-write inside [`crate::Store::write`]). Domain rules (dedupe,
//! authority, rate limits, report preservation on close) belong to the calling
//! operation; this repository owns SQL queries and row mapping only.

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::contract::{NoticeEventRow, NoticePosterEventFilter, NoticeRow, NoticeScopeFilter};
use crate::driver::sqlite::codec::stamp;
use crate::store::{Snapshot, Tx};

const NOTICE_COLUMNS: &str = "workspace_id, id, scope, kind, dedupe_key, author_class, \
     author_crew, author_task_id, linked_task_id, close_on_linked_done, expires_at, opened_at, \
     updated_at, closed_at, revision, record_json";

const EVENT_COLUMNS: &str = "workspace_id, notice_id, at, action, poster_class, poster_crew, \
     poster_task_id, detail_json";

/// Concrete notice repository scoped to a [`Snapshot`] (read-only) or a [`Tx`]
/// (read-write).
pub struct NoticeRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> NoticeRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> NoticeRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert a new notice row.
    pub fn put(&self, row: &NoticeRow) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO notices ({NOTICE_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
                ),
                params![
                    row.workspace_id,
                    row.id,
                    row.scope,
                    row.kind,
                    row.dedupe_key,
                    row.author_class,
                    row.author_crew,
                    row.author_task_id,
                    row.linked_task_id,
                    row.close_on_linked_done,
                    stamp(row.expires_at),
                    stamp(row.opened_at),
                    stamp(row.updated_at),
                    row.closed_at.map(stamp),
                    row.revision,
                    row.record_json,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Update an unclosed notice matching `expected_revision`. Returns whether
    /// a row was updated.
    pub fn replace_open(
        &self,
        row: &NoticeRow,
        expected_revision: u32,
    ) -> Result<bool, OrbitError> {
        let changed = self
            .conn
            .execute(
                "UPDATE notices SET scope = ?3, kind = ?4, expires_at = ?5, \
                 updated_at = ?6, revision = ?7, record_json = ?8, \
                 linked_task_id = ?9, close_on_linked_done = ?10 \
                 WHERE id = ?1 AND revision = ?2 AND closed_at IS NULL",
                params![
                    row.id,
                    expected_revision,
                    row.scope,
                    row.kind,
                    stamp(row.expires_at),
                    stamp(row.updated_at),
                    row.revision,
                    row.record_json,
                    row.linked_task_id,
                    row.close_on_linked_done,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(changed > 0)
    }

    /// Mark an unclosed notice at `expected_revision` as closed with
    /// `closed_at` and `record_json`. Returns whether a row was updated.
    pub fn close(
        &self,
        id: &str,
        expected_revision: u32,
        closed_at: DateTime<Utc>,
        record_json: &str,
    ) -> Result<bool, OrbitError> {
        let changed = self
            .conn
            .execute(
                "UPDATE notices SET closed_at = ?3, record_json = ?4 \
                 WHERE id = ?1 AND revision = ?2 AND closed_at IS NULL",
                params![id, expected_revision, stamp(closed_at), record_json],
            )
            .map_err(sqlite_store_error)?;
        Ok(changed > 0)
    }

    /// Append one row to `notice_events`.
    pub fn put_event(&self, event: &NoticeEventRow) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO notice_events ({EVENT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ),
                params![
                    event.workspace_id,
                    event.notice_id,
                    stamp(event.at),
                    event.action,
                    event.poster_class,
                    event.poster_crew,
                    event.poster_task_id,
                    event.detail_json,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Retention: delete a workspace's notices that stopped being in force
    /// before `cutoff`, together with their event history.
    pub fn purge_inactive(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        let cutoff = stamp(cutoff);
        let stale = "SELECT id FROM notices WHERE workspace_id = ?1 \
             AND ((closed_at IS NOT NULL AND closed_at < ?2) OR expires_at < ?2)";
        self.conn
            .execute(
                &format!("DELETE FROM notice_events WHERE notice_id IN ({stale})"),
                params![workspace_id, cutoff],
            )
            .map_err(sqlite_store_error)?;
        self.conn
            .execute(
                &format!("DELETE FROM notices WHERE id IN ({stale})"),
                params![workspace_id, cutoff],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}

impl<Access> NoticeRepository<'_, Access> {
    /// One notice by id.
    pub fn get(&self, id: &str) -> Result<Option<NoticeRow>, OrbitError> {
        self.conn
            .query_row(
                &format!("SELECT {NOTICE_COLUMNS} FROM notices WHERE id = ?1"),
                params![id],
                hydrate_notice,
            )
            .optional()
            .map_err(sqlite_store_error)
    }

    /// Notices selected by `scope`, newest update first.
    pub fn list(&self, scope: NoticeScopeFilter<'_>) -> Result<Vec<NoticeRow>, OrbitError> {
        match scope {
            NoticeScopeFilter::AllWorkspaces => query_rows(
                self.conn,
                &format!("SELECT {NOTICE_COLUMNS} FROM notices ORDER BY updated_at DESC, id"),
                [],
            ),
            NoticeScopeFilter::Workspace(workspace_id) => query_rows(
                self.conn,
                &format!(
                    "SELECT {NOTICE_COLUMNS} FROM notices WHERE workspace_id = ?1 \
                     ORDER BY updated_at DESC, id"
                ),
                params![workspace_id],
            ),
            NoticeScopeFilter::WorkspaceAndGlobal(workspace_id) => query_rows(
                self.conn,
                &format!(
                    "SELECT {NOTICE_COLUMNS} FROM notices \
                     WHERE workspace_id = ?1 OR scope = 'global' \
                     ORDER BY updated_at DESC, id"
                ),
                params![workspace_id],
            ),
        }
    }

    /// Every stored-open notice of `workspace_id`, plus every stored-open
    /// `global` notice in this store, newest update first.
    pub fn list_unclosed(&self, workspace_id: &str) -> Result<Vec<NoticeRow>, OrbitError> {
        query_rows(
            self.conn,
            &format!(
                "SELECT {NOTICE_COLUMNS} FROM notices WHERE closed_at IS NULL \
                 AND (workspace_id = ?1 OR scope = 'global') ORDER BY updated_at DESC, id"
            ),
            params![workspace_id],
        )
    }

    /// Stored-open notices linked to `task_id`, oldest opened first.
    pub fn list_linked(&self, task_id: &str) -> Result<Vec<NoticeRow>, OrbitError> {
        query_rows(
            self.conn,
            &format!(
                "SELECT {NOTICE_COLUMNS} FROM notices WHERE linked_task_id = ?1 \
                 AND closed_at IS NULL ORDER BY opened_at, id"
            ),
            params![task_id],
        )
    }

    /// One notice's event history, oldest first.
    pub fn events(&self, notice_id: &str) -> Result<Vec<NoticeEventRow>, OrbitError> {
        query_events(
            self.conn,
            &format!("SELECT {EVENT_COLUMNS} FROM notice_events WHERE notice_id = ?1 ORDER BY seq"),
            params![notice_id],
        )
    }

    /// Post events (`created`, `updated`, `corroborated`) matching `filter`,
    /// ordered by insertion sequence.
    pub fn list_poster_events(
        &self,
        filter: &NoticePosterEventFilter<'_>,
    ) -> Result<Vec<NoticeEventRow>, OrbitError> {
        query_events(
            self.conn,
            &format!(
                "SELECT {EVENT_COLUMNS} FROM notice_events WHERE at >= ?1 \
                 AND action IN ('created', 'updated', 'corroborated') \
                 AND ((?2 IS NOT NULL AND poster_task_id = ?2) \
                      OR (?3 IS NOT NULL AND poster_class = 'orchestrator' \
                          AND poster_crew = ?3)) ORDER BY seq"
            ),
            params![
                stamp(filter.since),
                filter.poster_task_id,
                filter.poster_crew
            ],
        )
    }

    /// The latest event (by insertion sequence) for each stored-open notice in
    /// `workspace_id`.
    pub fn list_latest_events(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<NoticeEventRow>, OrbitError> {
        query_events(
            self.conn,
            &format!(
                "SELECT {EVENT_COLUMNS} FROM notice_events WHERE seq IN (\
                 SELECT (SELECT seq FROM notice_events WHERE notice_id = notices.id \
                         ORDER BY seq DESC LIMIT 1) \
                 FROM notices WHERE workspace_id = ?1 AND closed_at IS NULL)"
            ),
            params![workspace_id],
        )
    }
}

fn query_rows(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<NoticeRow>, OrbitError> {
    let mut statement = conn.prepare(sql).map_err(sqlite_store_error)?;
    statement
        .query_map(params, hydrate_notice)
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

fn query_events(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<NoticeEventRow>, OrbitError> {
    let mut statement = conn.prepare(sql).map_err(sqlite_store_error)?;
    statement
        .query_map(params, hydrate_event)
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

fn hydrate_notice(row: &Row<'_>) -> rusqlite::Result<NoticeRow> {
    Ok(NoticeRow {
        workspace_id: row.get(0)?,
        id: row.get(1)?,
        scope: row.get(2)?,
        kind: row.get(3)?,
        dedupe_key: row.get(4)?,
        author_class: row.get(5)?,
        author_crew: row.get(6)?,
        author_task_id: row.get(7)?,
        linked_task_id: row.get(8)?,
        close_on_linked_done: row.get(9)?,
        expires_at: crate::parse_timestamp(&row.get::<_, String>(10)?)?,
        opened_at: crate::parse_timestamp(&row.get::<_, String>(11)?)?,
        updated_at: crate::parse_timestamp(&row.get::<_, String>(12)?)?,
        closed_at: row
            .get::<_, Option<String>>(13)?
            .map(|at| crate::parse_timestamp(&at))
            .transpose()?,
        revision: row.get(14)?,
        record_json: row.get(15)?,
    })
}

fn hydrate_event(row: &Row<'_>) -> rusqlite::Result<NoticeEventRow> {
    Ok(NoticeEventRow {
        workspace_id: row.get(0)?,
        notice_id: row.get(1)?,
        at: crate::parse_timestamp(&row.get::<_, String>(2)?)?,
        action: row.get(3)?,
        poster_class: row.get(4)?,
        poster_crew: row.get(5)?,
        poster_task_id: row.get(6)?,
        detail_json: row.get(7)?,
    })
}
