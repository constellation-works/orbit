//! Concrete repository for v2 workflow audit envelopes (`v2_audit_events`).
//!
//! Reached only through [`Snapshot::v2_audit`] (read-only) and [`Tx::v2_audit`]
//! (read-write inside [`crate::Store::write`]). Multi-statement reads (`tail`,
//! `kind_forward_payloads`) inherit the `BEGIN DEFERRED` snapshot that
//! [`crate::Store::read`] opens.

mod query;
mod read;
mod rows;
mod write;

#[cfg(test)]
mod rendezvous;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::marker::PhantomData;

use orbit_common::OrbitError;
use rusqlite::Connection;

use crate::contracts::{
    V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventRow, V2AuditEventTailPage,
    V2AuditKindForwardPayloads, V2AuditKindForwardQuery,
};
use crate::store::{Snapshot, Tx};

pub(crate) use read::V2AuditRunEventRow;

/// Concrete v2 audit-event repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct V2AuditRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> V2AuditRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> V2AuditRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert one v2 audit event (redacting its payload and ignoring duplicates
    /// by `(workspace_id, event_id)`).
    pub fn insert(&self, params: &V2AuditEventInsertParams) -> Result<(), OrbitError> {
        write::insert(self.conn, params)
    }
}

impl<Access> V2AuditRepository<'_, Access> {
    /// V2 audit rows matching `filter`.
    pub fn list(&self, filter: &V2AuditEventFilter) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        read::list(self.conn, filter)
    }

    /// Count of v2 audit rows matching `filter`.
    pub fn count(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError> {
        read::count(self.conn, filter)
    }

    /// Filtered total and chronological tail page from the caller's read snapshot.
    pub fn tail(&self, filter: &V2AuditEventFilter) -> Result<V2AuditEventTailPage, OrbitError> {
        read::tail(self.conn, filter)
    }

    /// Newest matching envelope rows for each run, independently capped.
    pub fn list_for_runs_partitioned(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
        body_kind: Option<&str>,
        per_run_limit: usize,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        read::list_for_runs_partitioned(
            self.conn,
            workspace_id,
            run_ids,
            source,
            body_kind,
            per_run_limit,
        )
    }

    /// Run ids among `run_ids` that have at least one reconstructable envelope.
    pub fn run_ids_with_events(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
    ) -> Result<HashSet<String>, OrbitError> {
        read::run_ids_with_events(self.conn, workspace_id, run_ids, source)
    }

    /// Payload text for one bounded kind-forward scan from the caller's read snapshot.
    pub fn kind_forward_payloads(
        &self,
        query: &V2AuditKindForwardQuery<'_>,
    ) -> Result<V2AuditKindForwardPayloads, OrbitError> {
        read::kind_forward_payloads(self.conn, query)
    }

    /// Distinct run ids in `workspace_id` that have at least one row with `event_type`.
    pub(crate) fn run_ids_for_event_type(
        &self,
        workspace_id: &str,
        event_type: &str,
    ) -> Result<Vec<String>, OrbitError> {
        read::run_ids_for_event_type(self.conn, workspace_id, event_type)
    }

    /// Slim v2 audit rows for one run matching `event_types`, ordered by `id`.
    pub(crate) fn list_run_events_by_types(
        &self,
        workspace_id: &str,
        run_id: &str,
        event_types: &[&str],
    ) -> Result<Vec<V2AuditRunEventRow>, OrbitError> {
        read::list_run_events_by_types(self.conn, workspace_id, run_id, event_types)
    }
}
