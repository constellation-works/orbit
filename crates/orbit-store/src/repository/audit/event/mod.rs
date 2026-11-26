//! Concrete repository for command audit events (`audit_events`).
//!
//! Reached through [`Snapshot::audit_event`] (read-only) and
//! [`Tx::audit_event`] (read-write inside [`crate::Store::write`]).
//!
//! L-0009: callers should reach audit data via `orbit audit list --json` —
//! `<workspace>/.orbit/orbit.db` (and -shm/-wal siblings) is an abandoned
//! leftover from pre-two-root binaries, not a mirror of the canonical global
//! `~/.orbit/orbit.db`. The CLI and runtime always use the global store.
//!
//! `row` hydrates full event rows; `insert` writes them (and owns the narrow
//! migration-free `append_at_path` path for foreign-generation readers and
//! checkout-rebind recovery); `queries` reads by id or filter and prunes;
//! `stats` and `aggregates` back the reporting surfaces.

mod aggregates;
mod insert;
mod queries;
mod row;
mod stats;

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEvent;
use rusqlite::Connection;
use rusqlite::types::ToSql;

use crate::contract::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditInvocationFields, AuditRoleAggregate, AuditToolAggregate, AuditToolModelAggregate,
};
use crate::store::{Snapshot, Tx};

pub(crate) use insert::append_at_path;
pub(crate) use row::{AUDIT_EVENT_COLUMNS, audit_event_from_row};

type SqlParams = Vec<Box<dyn ToSql>>;

/// Concrete command audit-event repository scoped to a [`Snapshot`] (read-only)
/// or a [`Tx`] (read-write).
pub struct AuditEventRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> AuditEventRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> AuditEventRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert one canonical audit row inside the caller's write transaction.
    pub fn insert(&self, params: &AuditEventInsertParams) -> Result<(), OrbitError> {
        insert::insert_on_connection(self.conn, params, AuditInvocationFields::default())
    }

    /// Insert one canonical audit row with the transport-neutral invocation
    /// fields carried by a tool session.
    pub fn insert_with_invocation(
        &self,
        params: &AuditEventInsertParams,
        invocation: AuditInvocationFields<'_>,
    ) -> Result<(), OrbitError> {
        insert::insert_on_connection(self.conn, params, invocation)
    }

    /// Delete audit rows with `timestamp < older_than`.
    pub fn prune(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError> {
        queries::prune(self.conn, older_than)
    }
}

impl<Access> AuditEventRepository<'_, Access> {
    /// Whether a durable audit event already records this execution.
    /// Recovery uses the unique execution key to make audit replay idempotent.
    pub fn execution_exists(&self, execution_id: &str) -> Result<bool, OrbitError> {
        queries::execution_exists(self.conn, execution_id)
    }

    /// Audit rows matching `filter`, newest first.
    pub fn list(&self, filter: &AuditEventFilter) -> Result<Vec<AuditEvent>, OrbitError> {
        queries::list(self.conn, filter)
    }

    /// One audit row by its database id.
    pub fn get(&self, id: i64) -> Result<Option<AuditEvent>, OrbitError> {
        queries::get(self.conn, id)
    }

    /// Requested audit rows, newest first, optionally scoped to a workspace.
    /// Missing or pruned IDs are omitted.
    pub fn list_by_ids(
        &self,
        ids: &[i64],
        workspace_id: Option<&str>,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        queries::list_by_ids(self.conn, ids, workspace_id)
    }

    /// Status counts, average duration and max duration for the `since`/`tool` window.
    pub fn stats(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<(i64, i64, i64, i64, f64, i64), OrbitError> {
        stats::stats(self.conn, since, tool)
    }

    /// `COUNT(*)` for the same `since` population as [`Self::stats`].
    pub fn count(&self, since: Option<&DateTime<Utc>>) -> Result<i64, OrbitError> {
        stats::count(self.conn, since)
    }

    /// Ascending duration samples for the `since`/`tool` window.
    pub fn durations(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<Vec<i64>, OrbitError> {
        stats::durations(self.conn, since, tool)
    }

    /// Hourly count buckets `(rfc3339_hour_start, count)` at or after `since`.
    pub fn hourly_buckets(&self, since: &DateTime<Utc>) -> Result<Vec<(String, i64)>, OrbitError> {
        stats::hourly_buckets(self.conn, since)
    }

    /// Capability and policy denials counted once per invocation and operation.
    pub fn denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        aggregates::denials_by_operation(self.conn, since)
    }

    /// Audited tool invocations classified by how each row's identity was established.
    pub fn tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditAttributionAggregate>, OrbitError> {
        aggregates::tool_call_counts_by_attribution(self.conn, since)
    }

    /// Per-tool aggregate of audit events with `timestamp >= since`.
    pub fn aggregates_by_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolAggregate>, OrbitError> {
        aggregates::aggregates_by_tool(self.conn, since)
    }

    /// Callable tool calls at or after `since`, grouped by tool and server-derived model.
    pub fn aggregates_by_tool_model(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolModelAggregate>, OrbitError> {
        aggregates::aggregates_by_tool_model(self.conn, since)
    }

    /// Per-role aggregate of audit events with `timestamp >= since`.
    pub fn aggregates_by_role(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditRoleAggregate>, OrbitError> {
        aggregates::aggregates_by_role(self.conn, since)
    }

    /// Per-canonical-actor aggregate of audit events with `timestamp >= since`.
    pub fn aggregates_by_actor(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditActorAggregate>, OrbitError> {
        aggregates::aggregates_by_actor(self.conn, since)
    }
}

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
