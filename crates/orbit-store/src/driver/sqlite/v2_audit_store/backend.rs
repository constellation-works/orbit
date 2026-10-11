use std::collections::HashSet;

use orbit_common::OrbitError;

use crate::Store;

use crate::contracts::{
    V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventRow, V2AuditEventTailPage,
};

impl crate::contracts::V2AuditStoreBackend for Store {
    fn insert_v2_audit_event(&self, params: &V2AuditEventInsertParams) -> Result<(), OrbitError> {
        Self::insert_v2_audit_event(self, params)
    }

    fn list_v2_audit_events(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        Self::list_v2_audit_events(self, filter)
    }

    fn count_v2_audit_events(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError> {
        Self::count_v2_audit_events(self, filter)
    }

    fn list_v2_audit_event_tail(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<V2AuditEventTailPage, OrbitError> {
        Self::list_v2_audit_event_tail(self, filter)
    }

    fn list_v2_audit_events_for_runs_partitioned(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
        body_kind: Option<&str>,
        per_run_limit: usize,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        Self::list_v2_audit_events_for_runs_partitioned(
            self,
            workspace_id,
            run_ids,
            source,
            body_kind,
            per_run_limit,
        )
    }

    fn list_v2_audit_run_ids_with_events(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
    ) -> Result<HashSet<String>, OrbitError> {
        Self::list_v2_audit_run_ids_with_events(self, workspace_id, run_ids, source)
    }
}
