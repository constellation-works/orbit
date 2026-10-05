//! Generated-index reads behind bounded task listing.
//!
//! Listing validates the index against every registered envelope first
//! (`repository::task::v2::index`), then lets SQL answer the predicates the
//! composite indexes cover — status, priority, job run, tags, continuation —
//! together with ordering and `LIMIT`, so a page costs the freshness scan plus
//! the selected rows rather than every envelope in the workspace.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{TaskStatus, task_id_prefix};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params_from_iter};

use super::partition_id::validate_partition_id;
use super::store::TaskRegistryStore;
use super::util::TERMINAL_STATUSES;
use crate::contracts::{IndexedTaskRow, TaskIndexFilter, TaskIndexSelection};

/// Bound at most this many ids per `IN (...)` list; well under SQLite's
/// default variable limit.
const ID_CHUNK: usize = 500;

impl TaskRegistryStore {
    /// Every index row of a workspace with the fields listing filters and
    /// orders by, for the freshness scan to compare against envelopes.
    pub fn indexed_task_rows_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<BTreeMap<String, IndexedTaskRow>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT task_id, status, priority, job_run_id, created_at, updated_at
                 FROM task_bundle_index
                 WHERE workspace_id = ?1",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([&partition_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    IndexedTaskRow {
                        status: row.get(1)?,
                        priority: row.get(2)?,
                        job_run_id: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                        tags: BTreeSet::new(),
                    },
                ))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut indexed = rows
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut tag_stmt = conn
            .prepare("SELECT task_id, tag FROM task_bundle_tags WHERE workspace_id = ?1")
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let tag_rows = tag_stmt
            .query_map([&partition_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        for tag_row in tag_rows {
            let (task_id, tag) = tag_row.map_err(|e| OrbitError::Store(e.to_string()))?;
            if let Some(row) = indexed.get_mut(&task_id) {
                row.tags.insert(tag);
            }
        }
        Ok(indexed)
    }

    /// Ids the index selects for `filter`, newest first with task ID ascending
    /// for ties; with `terminal_last`, tasks in a terminal status follow the
    /// rest in that same order. `limit` bounds the ids; the total is the number
    /// of rows the filter matched regardless of it.
    pub fn indexed_task_selection(
        &self,
        partition_id: &str,
        filter: &TaskIndexFilter,
        terminal_last: bool,
        limit: Option<usize>,
    ) -> Result<TaskIndexSelection, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let (predicate, mut values) = index_filter_sql(&partition_id, filter);
        let conn = self.read()?;

        let total = match limit {
            Some(_) => {
                let count: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM task_bundle_index WHERE {predicate}"),
                        params_from_iter(values.iter()),
                        |row| row.get(0),
                    )
                    .map_err(|e| OrbitError::Store(e.to_string()))?;
                Some(usize::try_from(count).map_err(|e| OrbitError::Store(e.to_string()))?)
            }
            None => None,
        };

        let mut sql = format!("SELECT task_id FROM task_bundle_index WHERE {predicate} ORDER BY ");
        if terminal_last {
            sql.push_str("(status IN (");
            push_placeholders(&mut sql, TERMINAL_STATUSES.len());
            sql.push_str(")) ASC, ");
            values.extend(
                TERMINAL_STATUSES
                    .iter()
                    .map(|status| Value::Text(status.to_string())),
            );
        }
        sql.push_str("created_at DESC, task_id ASC");
        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            values.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        }
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let ids = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let total = total.unwrap_or(ids.len());
        Ok(TaskIndexSelection { ids, total })
    }

    /// Status projection for one listing: every indexed task in
    /// `partition_id` plus each of `targets` wherever it is registered, so
    /// dependency and relation labels resolve across workspaces without
    /// projecting the whole registry.
    ///
    /// Prefix knowledge is derived from a projection's keys
    /// (`TaskReferenceIndex`), so a missing target under a prefix this
    /// workspace does not use is represented by one arbitrary indexed task of
    /// that prefix. The label then stays `missing`, as it is under the global
    /// projection, instead of becoming "not verifiable here".
    pub fn task_status_index_for(
        &self,
        partition_id: &str,
        targets: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut statuses = BTreeMap::new();

        let mut stmt = conn
            .prepare("SELECT task_id, status FROM task_bundle_index WHERE workspace_id = ?1")
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([&partition_id], decode_status_row)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        collect_statuses(rows, &mut statuses)?;

        let missing = targets
            .iter()
            .filter(|target| !statuses.contains_key(*target))
            .cloned()
            .collect::<Vec<_>>();
        for chunk in missing.chunks(ID_CHUNK) {
            let mut sql =
                String::from("SELECT task_id, status FROM task_bundle_index WHERE task_id IN (");
            push_placeholders(&mut sql, chunk.len());
            sql.push(')');
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let rows = stmt
                .query_map(params_from_iter(chunk.iter()), decode_status_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            collect_statuses(rows, &mut statuses)?;
        }

        let mut known_prefixes = statuses
            .keys()
            .filter_map(|id| task_id_prefix(id))
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        for target in &missing {
            if statuses.contains_key(target) {
                continue;
            }
            let Some(prefix) = task_id_prefix(target) else {
                continue;
            };
            if !known_prefixes.insert(prefix.to_owned()) {
                continue;
            }
            if let Some((task_id, status)) = representative_of_prefix(&conn, prefix)? {
                statuses.insert(task_id, status);
            }
        }
        Ok(statuses)
    }
}

/// `WHERE` predicate and its bound values for `filter` within one workspace.
fn index_filter_sql(partition_id: &str, filter: &TaskIndexFilter) -> (String, Vec<Value>) {
    let mut sql = String::from("workspace_id = ?");
    let mut values = vec![Value::Text(partition_id.to_owned())];
    if !filter.statuses.is_empty() {
        sql.push_str(" AND status IN (");
        push_placeholders(&mut sql, filter.statuses.len());
        sql.push(')');
        values.extend(
            filter
                .statuses
                .iter()
                .map(|status| Value::Text(status.to_string())),
        );
    }
    if let Some(priority) = filter.priority {
        sql.push_str(" AND priority = ?");
        values.push(Value::Text(priority.to_string()));
    }
    if let Some(job_run_id) = &filter.job_run_id {
        sql.push_str(" AND job_run_id = ?");
        values.push(Value::Text(job_run_id.clone()));
    }
    for tag in &filter.tags {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM task_bundle_tags t
                WHERE t.task_id = task_bundle_index.task_id AND t.tag = ?)",
        );
        values.push(Value::Text(tag.clone()));
    }
    if let Some((created_at, task_id)) = &filter.scan_before {
        // The freshness scan proved each row's `created_at` is the envelope's
        // own `to_rfc3339()`, so the stored strings order like the instants.
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND task_id > ?))");
        let created_at = created_at.to_rfc3339();
        values.push(Value::Text(created_at.clone()));
        values.push(Value::Text(created_at));
        values.push(Value::Text(task_id.clone()));
    }
    if !filter.excluded_ids.is_empty() {
        sql.push_str(" AND task_id NOT IN (");
        push_placeholders(&mut sql, filter.excluded_ids.len());
        sql.push(')');
        values.extend(filter.excluded_ids.iter().cloned().map(Value::Text));
    }
    (sql, values)
}

fn push_placeholders(sql: &mut String, count: usize) {
    for index in 0..count {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
    }
}

fn decode_status_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String)> {
    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
}

fn collect_statuses(
    rows: impl Iterator<Item = rusqlite::Result<(String, String)>>,
    statuses: &mut BTreeMap<String, TaskStatus>,
) -> Result<(), OrbitError> {
    for row in rows {
        let (task_id, raw_status) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
        statuses.insert(
            task_id.clone(),
            parse_indexed_status(&task_id, &raw_status)?,
        );
    }
    Ok(())
}

fn parse_indexed_status(task_id: &str, raw_status: &str) -> Result<TaskStatus, OrbitError> {
    raw_status.parse::<TaskStatus>().map_err(|e| {
        OrbitError::Store(format!(
            "invalid indexed status '{raw_status}' for task '{task_id}': {e}"
        ))
    })
}

/// One indexed task whose id starts with `<prefix>-`, if any. Task ids are
/// `<prefix>-<digits>` and `.` follows `-` in byte order, so the half-open
/// range covers exactly that prefix on the primary key.
fn representative_of_prefix(
    conn: &Connection,
    prefix: &str,
) -> Result<Option<(String, TaskStatus)>, OrbitError> {
    let row = conn
        .query_row(
            "SELECT task_id, status FROM task_bundle_index
             WHERE task_id >= ?1 AND task_id < ?2
             LIMIT 1",
            [format!("{prefix}-"), format!("{prefix}.")],
            decode_status_row,
        )
        .optional()
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    row.map(|(task_id, raw_status)| {
        let status = parse_indexed_status(&task_id, &raw_status)?;
        Ok((task_id, status))
    })
    .transpose()
}
