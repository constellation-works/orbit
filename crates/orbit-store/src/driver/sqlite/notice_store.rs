//! Fleet notices in `notices` and their history in `notice_events`.
//!
//! A post is one immediate transaction: it reads the snapshot core's decision
//! needs, runs the decision, and applies the plan it returns, so two posts of
//! one key cannot both create a notice.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    NoticeEventRow, NoticePostDecision, NoticePostPlan, NoticePostQuery, NoticePostSnapshot,
    NoticeRow, NoticeStoreBackend,
};

use super::codec::stamp;

const NOTICE_COLUMNS: &str = "workspace_id, id, scope, kind, dedupe_key, author_class, \
     author_crew, author_task_id, linked_task_id, close_on_linked_done, expires_at, opened_at, \
     updated_at, closed_at, revision, record_json";

const EVENT_COLUMNS: &str = "workspace_id, notice_id, at, action, poster_class, poster_crew, \
     poster_task_id, detail_json";

impl NoticeStoreBackend for Store {
    fn post_notice(
        &self,
        query: &NoticePostQuery<'_>,
        decide: &NoticePostDecision<'_>,
    ) -> Result<NoticeRow, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let snapshot = NoticePostSnapshot {
                unclosed: query_rows(
                    conn,
                    &format!(
                        "SELECT {NOTICE_COLUMNS} FROM notices WHERE closed_at IS NULL \
                         AND (workspace_id = ?1 OR scope = 'global') ORDER BY updated_at DESC, id"
                    ),
                    params![query.workspace_id],
                )?,
                poster_events: query_events(
                    conn,
                    &format!(
                        "SELECT {EVENT_COLUMNS} FROM notice_events WHERE at >= ?1 \
                         AND action IN ('created', 'updated', 'corroborated') \
                         AND ((?2 IS NOT NULL AND poster_task_id = ?2) \
                              OR (?3 IS NOT NULL AND poster_class = 'orchestrator' \
                                  AND poster_crew = ?3)) ORDER BY seq"
                    ),
                    params![
                        stamp(query.events_since),
                        query.poster_task_id,
                        query.poster_crew
                    ],
                )?,
                latest_events: query_events(
                    conn,
                    &format!(
                        "SELECT {EVENT_COLUMNS} FROM notice_events WHERE seq IN (\
                         SELECT (SELECT seq FROM notice_events WHERE notice_id = notices.id \
                                 ORDER BY seq DESC LIMIT 1) \
                         FROM notices WHERE workspace_id = ?1 AND closed_at IS NULL)"
                    ),
                    params![query.workspace_id],
                )?,
            };
            let plan = decide(&snapshot)?;
            if !matches!(plan, NoticePostPlan::Unchanged { .. })
                && let Some(cutoff) = query.purge_before
            {
                purge_inactive(conn, query.workspace_id, &stamp(cutoff))?;
            }
            match plan {
                NoticePostPlan::Unchanged { row } => Ok(row),
                NoticePostPlan::Insert { row, event } => {
                    insert_row(conn, &row)?;
                    insert_event(conn, &event)?;
                    Ok(row)
                }
                NoticePostPlan::Update {
                    row,
                    expected_revision,
                    event,
                } => {
                    let changed = conn
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
                    if changed == 0 {
                        return Err(OrbitError::Store(format!(
                            "notice {} changed inside its posting transaction",
                            row.id
                        )));
                    }
                    insert_event(conn, &event)?;
                    Ok(row)
                }
            }
        })
    }

    fn close_notice(
        &self,
        row: &NoticeRow,
        expected_revision: u32,
        event: &NoticeEventRow,
    ) -> Result<bool, OrbitError> {
        let Some(closed_at) = row.closed_at else {
            return Err(OrbitError::Store(format!(
                "closing notice {} without a close time",
                row.id
            )));
        };
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            // Read inside the write lock. A corroboration that committed after
            // the caller snapped the row still matches `revision`, and its
            // `reports` must survive the replacement of `record_json`.
            let stored_json = conn
                .query_row(
                    "SELECT record_json FROM notices \
                     WHERE id = ?1 AND revision = ?2 AND closed_at IS NULL",
                    params![row.id, expected_revision],
                    |stored| stored.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_store_error)?;
            let Some(stored_json) = stored_json else {
                return Ok(false);
            };
            let record_json = closing_record_with_stored_reports(&stored_json, &row.record_json)?;
            let changed = conn
                .execute(
                    "UPDATE notices SET closed_at = ?3, record_json = ?4 \
                     WHERE id = ?1 AND revision = ?2 AND closed_at IS NULL",
                    params![row.id, expected_revision, stamp(closed_at), record_json],
                )
                .map_err(sqlite_store_error)?;
            if changed == 0 {
                return Ok(false);
            }
            insert_event(conn, event)?;
            Ok(true)
        })
    }

    fn notice_row(&self, id: &str) -> Result<Option<NoticeRow>, OrbitError> {
        let conn = self.read()?;
        conn.query_row(
            &format!("SELECT {NOTICE_COLUMNS} FROM notices WHERE id = ?1"),
            params![id],
            hydrate_notice,
        )
        .optional()
        .map_err(sqlite_store_error)
    }

    fn notice_rows(
        &self,
        workspace_ids: Option<&[String]>,
        include_global: bool,
    ) -> Result<Vec<NoticeRow>, OrbitError> {
        let conn = self.read()?;
        let rows = query_rows(
            &conn,
            &format!("SELECT {NOTICE_COLUMNS} FROM notices ORDER BY updated_at DESC, id"),
            [],
        )?;
        Ok(rows
            .into_iter()
            .filter(|row| {
                workspace_ids.is_none_or(|ids| ids.contains(&row.workspace_id))
                    || (include_global && row.scope == "global")
            })
            .collect())
    }

    fn notices_linked_to(&self, task_id: &str) -> Result<Vec<NoticeRow>, OrbitError> {
        let conn = self.read()?;
        query_rows(
            &conn,
            &format!(
                "SELECT {NOTICE_COLUMNS} FROM notices WHERE linked_task_id = ?1 \
                 AND closed_at IS NULL ORDER BY opened_at, id"
            ),
            params![task_id],
        )
    }

    fn notice_events(&self, notice_id: &str) -> Result<Vec<NoticeEventRow>, OrbitError> {
        let conn = self.read()?;
        query_events(
            &conn,
            &format!("SELECT {EVENT_COLUMNS} FROM notice_events WHERE notice_id = ?1 ORDER BY seq"),
            params![notice_id],
        )
    }
}

/// The closing document, with `reports` taken from the row locked for the
/// close. A post can change `reports` without moving `revision`.
fn closing_record_with_stored_reports(
    stored_json: &str,
    closing_json: &str,
) -> Result<String, OrbitError> {
    let mut closing: serde_json::Value = serde_json::from_str(closing_json).map_err(|error| {
        OrbitError::Store(format!("closing notice document is unreadable: {error}"))
    })?;
    let stored: serde_json::Value = serde_json::from_str(stored_json).map_err(|error| {
        OrbitError::Store(format!("stored notice document is unreadable: {error}"))
    })?;
    let Some(document) = closing.as_object_mut() else {
        return Err(OrbitError::Store(
            "closing notice document is not an object".into(),
        ));
    };
    if let Some(reports) = stored.get("reports").cloned() {
        document.insert("reports".to_string(), reports);
    }
    serde_json::to_string(&closing).map_err(|error| {
        OrbitError::Store(format!("closing notice document is unreadable: {error}"))
    })
}

/// Retention: a notice and its history go once it stopped being in force
/// before `cutoff`. Purge is the only delete.
fn purge_inactive(conn: &Connection, workspace_id: &str, cutoff: &str) -> Result<(), OrbitError> {
    let stale = "SELECT id FROM notices WHERE workspace_id = ?1 \
         AND ((closed_at IS NOT NULL AND closed_at < ?2) OR expires_at < ?2)";
    conn.execute(
        &format!("DELETE FROM notice_events WHERE notice_id IN ({stale})"),
        params![workspace_id, cutoff],
    )
    .map_err(sqlite_store_error)?;
    conn.execute(
        &format!("DELETE FROM notices WHERE id IN ({stale})"),
        params![workspace_id, cutoff],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

fn insert_row(conn: &Connection, row: &NoticeRow) -> Result<(), OrbitError> {
    conn.execute(
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

fn insert_event(conn: &Connection, event: &NoticeEventRow) -> Result<(), OrbitError> {
    conn.execute(
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
