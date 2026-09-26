//! SQL for the friction record tables (ORB-10680).
//!
//! Every predicate the file scan used to apply in Rust — workspace, status,
//! model, tag, date range, free text — is expressed here as SQL, together with
//! ordering and the `LIMIT`/`OFFSET` page. SQLite therefore decides which rows
//! exist before any body reaches Rust, which is what keeps list memory
//! proportional to the requested page rather than to the retained corpus.

use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::record::{FrictionRecord, FrictionStatus};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row};

use crate::contracts::{FrictionListFilter, StoredFrictionRecord};

/// Columns every record read selects, in decode order.
const RECORD_COLUMNS: &str = "friction_id, title, model, status, created_at, resolved_at, \
     during_task, resolved_by_task, tags_json, body, legacy_path, rehome_to";

// Counts rows this process decoded into a `StoredFrictionRecord`.
//
// Thread-local so each `#[test]` observes only its own decodes; the counter is
// how the bounded-page tests prove SQLite — not Rust — dropped the rows outside
// the requested window.
#[cfg(test)]
thread_local! {
    pub(crate) static DECODED_RECORDS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Canonical, fixed-width, lexicographically ordered timestamp encoding.
///
/// Ordering and range predicates run against the stored text directly, so the
/// encoding has to sort the same way the instants do.
pub(super) fn encode_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

pub(super) fn decode_timestamp(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("friction timestamp '{raw}': {error}")))
}

/// A `WHERE` fragment plus its bound parameters, built once and reused by the
/// list and count paths.
pub(super) struct RecordPredicate {
    pub(super) sql: String,
    pub(super) params: Vec<SqlValue>,
}

/// Builds the predicate with explicit `?N` placeholders. The free-text clause
/// repeats one bound parameter across seven columns, so anonymous `?` would
/// mis-number every parameter after it.
pub(super) fn build_predicate(workspace_id: &str, filter: &FrictionListFilter) -> RecordPredicate {
    let mut clauses = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    let bind = |params: &mut Vec<SqlValue>, value: SqlValue| -> usize {
        params.push(value);
        params.len()
    };

    let workspace_index = bind(&mut params, SqlValue::Text(workspace_id.to_string()));
    clauses.push(format!("r.workspace_id = ?{workspace_index}"));

    if let Some(model) = &filter.model {
        let index = bind(&mut params, SqlValue::Text(model.clone()));
        clauses.push(format!("r.model = ?{index}"));
    }
    if let Some(status) = filter.status {
        let index = bind(&mut params, SqlValue::Text(status.as_str().to_string()));
        clauses.push(format!("r.status = ?{index}"));
    }
    if let Some(tag) = &filter.tag {
        let index = bind(&mut params, SqlValue::Text(tag.clone()));
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM friction_record_tags t \
             WHERE t.workspace_id = r.workspace_id AND t.friction_id = r.friction_id \
             AND t.tag = ?{index})"
        ));
    }
    if let Some(from) = filter.from {
        let index = bind(&mut params, SqlValue::Text(encode_timestamp(from)));
        clauses.push(format!("r.created_at >= ?{index}"));
    }
    if let Some(to) = filter.to {
        let index = bind(&mut params, SqlValue::Text(encode_timestamp(to)));
        clauses.push(format!("r.created_at <= ?{index}"));
    }
    if let Some(query) = &filter.q {
        let needle = query.trim().to_lowercase();
        if !needle.is_empty() {
            let index = bind(&mut params, SqlValue::Text(needle));
            // Mirrors the scan predicate field for field so a saved query keeps
            // matching the same records after the cutover.
            clauses.push(format!(
                "(instr(lower(r.friction_id), ?{index}) > 0 \
                 OR instr(lower(COALESCE(r.title, '')), ?{index}) > 0 \
                 OR instr(lower(r.model), ?{index}) > 0 \
                 OR instr(r.status, ?{index}) > 0 \
                 OR instr(lower(COALESCE(r.during_task, '')), ?{index}) > 0 \
                 OR instr(lower(r.body), ?{index}) > 0 \
                 OR EXISTS (SELECT 1 FROM friction_record_tags t \
                    WHERE t.workspace_id = r.workspace_id \
                    AND t.friction_id = r.friction_id \
                    AND instr(lower(t.tag), ?{index}) > 0))"
            ));
        }
    }

    RecordPredicate {
        sql: clauses.join(" AND "),
        params,
    }
}

/// Reads one page of records. Ordering matches the retired scan
/// (`created_at`, then id) so pagination is stable across calls.
pub(super) fn list_records(
    conn: &Connection,
    workspace_id: &str,
    filter: &FrictionListFilter,
) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
    let predicate = build_predicate(workspace_id, filter);
    let mut params = predicate.params;
    // A negative LIMIT is SQLite's "no bound"; OFFSET still applies.
    params.push(SqlValue::Integer(match filter.limit {
        Some(limit) => i64::try_from(limit).unwrap_or(i64::MAX),
        None => -1,
    }));
    let limit_index = params.len();
    params.push(SqlValue::Integer(
        i64::try_from(filter.offset).unwrap_or(i64::MAX),
    ));
    let offset_index = params.len();
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM friction_records r \
         WHERE {} ORDER BY r.created_at ASC, r.friction_id ASC \
         LIMIT ?{limit_index} OFFSET ?{offset_index}",
        predicate.sql
    );

    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(decode_record(row))
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;

    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|error| OrbitError::Store(error.to_string()))??);
    }
    Ok(records)
}

pub(super) fn show_record(
    conn: &Connection,
    workspace_id: &str,
    id: &str,
) -> Result<Option<StoredFrictionRecord>, OrbitError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {RECORD_COLUMNS} FROM friction_records r \
             WHERE r.workspace_id = ?1 AND r.friction_id = ?2"
        ))
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let mut rows = statement
        .query(rusqlite::params![workspace_id, id])
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    match rows
        .next()
        .map_err(|error| OrbitError::Store(error.to_string()))?
    {
        Some(row) => Ok(Some(decode_record(row)?)),
        None => Ok(None),
    }
}

/// Workspace IDs other than `workspace_id` that already hold `friction_id`.
///
/// Friction IDs are workspace-local (ORB-11078). This lookup is the
/// ownership probe that distinguishes a dangling local miss from a
/// cross-workspace ID that must not count as coverage.
pub(super) fn foreign_owners_of(
    conn: &Connection,
    workspace_id: &str,
    friction_id: &str,
) -> Result<Vec<String>, OrbitError> {
    let mut statement = conn
        .prepare(
            "SELECT workspace_id FROM friction_records \
             WHERE friction_id = ?1 AND workspace_id != ?2 \
             ORDER BY workspace_id",
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = statement
        .query_map(rusqlite::params![friction_id, workspace_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let mut owners = Vec::new();
    for row in rows {
        owners.push(row.map_err(|error| OrbitError::Store(error.to_string()))?);
    }
    Ok(owners)
}

pub(crate) use crate::driver::sqlite::friction_write::{next_month_seq, upsert_record};

fn decode_record(row: &Row<'_>) -> Result<StoredFrictionRecord, OrbitError> {
    #[cfg(test)]
    DECODED_RECORDS.with(|count| count.set(count.get() + 1));

    let get_text = |index: usize| -> Result<String, OrbitError> {
        row.get::<_, String>(index)
            .map_err(|error| OrbitError::Store(error.to_string()))
    };
    let get_optional = |index: usize| -> Result<Option<String>, OrbitError> {
        row.get::<_, Option<String>>(index)
            .map_err(|error| OrbitError::Store(error.to_string()))
    };

    let id = get_text(0)?;
    let status_raw = get_text(3)?;
    let status = FrictionStatus::from_str(&status_raw).map_err(|error| {
        OrbitError::Store(format!(
            "friction record {id} has status '{status_raw}': {error}"
        ))
    })?;
    let tags: Vec<String> = serde_json::from_str(&get_text(8)?)
        .map_err(|error| OrbitError::Store(format!("friction record {id} tags: {error}")))?;
    let resolved_at = get_optional(5)?
        .map(|raw| decode_timestamp(&raw))
        .transpose()?;

    Ok(StoredFrictionRecord {
        record: FrictionRecord {
            id,
            title: get_optional(1)?,
            model: get_text(2)?,
            created_at: decode_timestamp(&get_text(4)?)?,
            status,
            tags,
            resolved_at,
            during_task: get_optional(6)?,
            resolved_by_task: get_optional(7)?,
            rehome_to: get_optional(11)?,
            body: get_text(9)?,
        },
        path: get_optional(10)?.map(PathBuf::from),
    })
}
