//! Audit-event SQL queries backing the `orbit audit list` CLI.
//!
//! L-0009: callers should reach audit data via `orbit audit list --json` —
//! `<workspace>/.orbit/orbit.db` (and -shm/-wal siblings) is an abandoned
//! leftover from pre-two-root binaries, not a mirror of the canonical global
//! `~/.orbit/orbit.db`. The CLI and runtime always use the global store.
//!
//! `row` hydrates full event rows; `insert` writes them; `queries` reads by id
//! or filter and prunes; `stats` and `aggregates` back the reporting surfaces;
//! `incident` groups failures.

mod aggregates;
pub mod incident;
mod insert;
mod queries;
mod row;
mod stats;

use chrono::{DateTime, Utc};
use rusqlite::types::ToSql;

use row::{AUDIT_EVENT_COLUMNS, audit_event_from_row};

type SqlParams = Vec<Box<dyn ToSql>>;

/// Shared `timestamp >= since` then `tool_name = tool` predicates and their
/// bind parameters, so list, counts and duration samples select one population.
/// Callers append further predicates numbering binds from `params.len() + 1`.
fn since_tool_filter(
    since: Option<&DateTime<Utc>>,
    tool: Option<&str>,
) -> (Vec<String>, SqlParams) {
    let mut conditions = Vec::new();
    let mut params: SqlParams = Vec::new();
    if let Some(since) = since {
        conditions.push(format!("timestamp >= ?{}", params.len() + 1));
        params.push(Box::new(since.to_rfc3339()));
    }
    if let Some(tool) = tool {
        conditions.push(format!("tool_name = ?{}", params.len() + 1));
        params.push(Box::new(tool.to_string()));
    }
    (conditions, params)
}

/// `WHERE a AND b`, or the empty string when there are no predicates.
fn where_clause(conditions: &[String]) -> String {
    if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    }
}

fn param_refs(params: &SqlParams) -> Vec<&dyn ToSql> {
    params.iter().map(|b| b.as_ref()).collect()
}
