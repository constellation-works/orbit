use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEvent;
use orbit_types::tool::StoredTool;

use crate::contracts::{AuditEventFilter, AuditEventInsertParams};
use crate::contracts::{
    AuditEventStoreBackend, TaskReservationCheckParams, TaskReservationCheckResult,
    TaskReservationListResult, TaskReservationOwnedConflictsParams,
    TaskReservationOwnedConflictsResult, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseByOwnerResult, TaskReservationReleaseParams,
    TaskReservationReleaseResult, TaskReservationReserveParams, TaskReservationReserveResult,
    TaskReservationStoreBackend, ToolStoreBackend, WorkspaceClaimAcquireParams,
    WorkspaceClaimAcquireResult, WorkspaceClaimCheckParams, WorkspaceClaimCheckResult,
    WorkspaceClaimReleaseParams, WorkspaceClaimReleaseResult, WorkspaceClaimStatusResult,
};
use crate::scope::{ScopeStrategy, ScopedStore, resolve};
use crate::{ActiveTaskReservation, Store};

#[derive(Clone)]
pub(crate) struct SqliteToolStoreBackend {
    pub(crate) store: Store,
}

impl ToolStoreBackend for SqliteToolStoreBackend {
    fn list_tools(&self) -> Result<Vec<StoredTool>, OrbitError> {
        self.store.list_tools()
    }

    fn get_tool(&self, name: &str) -> Result<Option<StoredTool>, OrbitError> {
        self.store.get_tool(name)
    }

    fn insert_tool(&self, tool: &StoredTool) -> Result<(), OrbitError> {
        self.store.with_transaction(|tx| tx.insert_tool(tool))
    }

    fn delete_tool(&self, name: &str) -> Result<bool, OrbitError> {
        self.store.with_transaction(|tx| tx.delete_tool(name))
    }

    fn set_tool_enabled(&self, name: &str, enabled: bool) -> Result<bool, OrbitError> {
        self.store
            .with_transaction(|tx| tx.set_tool_enabled(name, enabled))
    }
}

#[derive(Clone)]
pub(crate) struct SqliteAuditEventStoreBackend {
    pub(crate) store: Store,
}

impl AuditEventStoreBackend for SqliteAuditEventStoreBackend {
    fn insert_audit_event_record(&self, params: &AuditEventInsertParams) -> Result<(), OrbitError> {
        self.store.insert_audit_event_record(params)
    }

    fn list_audit_events(&self, filter: &AuditEventFilter) -> Result<Vec<AuditEvent>, OrbitError> {
        self.store.list_audit_events(filter)
    }

    fn get_audit_event(&self, id: i64) -> Result<Option<AuditEvent>, OrbitError> {
        // Audit events use the GlobalOnly strategy per `CLAUDE.md`. The key is
        // stringified so the canonical `resolve` helper can handle it; the
        // ScopedStore impl parses it back inside `get_global`.
        resolve::<AuditEvent, _>(self, &id.to_string())
    }

    fn get_audit_event_stats(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<(i64, i64, i64, i64, f64, i64), OrbitError> {
        self.store.get_audit_event_stats(since, tool)
    }

    fn get_audit_event_durations(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<Vec<i64>, OrbitError> {
        self.store.get_audit_event_durations(since, tool)
    }

    fn get_audit_event_durations_null_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<i64>, OrbitError> {
        self.store.get_audit_event_durations_null_tool(since)
    }

    fn get_audit_event_hourly_buckets(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.store.get_audit_event_hourly_buckets(since)
    }

    fn get_audit_denials_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.store.get_audit_denials_by_role(since)
    }

    fn get_audit_denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.store.get_audit_denials_by_operation(since)
    }

    fn get_audit_tool_call_counts_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<crate::AuditToolCallCountsByRole>, OrbitError> {
        self.store.get_audit_tool_call_counts_by_role(since)
    }

    fn get_audit_tool_call_counts_by_surface_and_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<crate::AuditToolCallCountsBySurfaceAndRole>, OrbitError> {
        self.store
            .get_audit_tool_call_counts_by_surface_and_role(since)
    }

    fn get_audit_tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<crate::AuditAttributionAggregate>, OrbitError> {
        self.store.get_audit_tool_call_counts_by_attribution(since)
    }

    fn get_audit_top_tool_calls(
        &self,
        since: Option<&DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<crate::AuditTopToolCall>, OrbitError> {
        self.store.get_audit_top_tool_calls(since, limit)
    }

    fn get_audit_event_aggregates_by_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<crate::AuditToolAggregate>, OrbitError> {
        self.store.get_audit_event_aggregates_by_tool(since)
    }

    fn get_audit_event_aggregates_by_role(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<crate::AuditRoleAggregate>, OrbitError> {
        self.store.get_audit_event_aggregates_by_role(since)
    }

    fn get_audit_event_aggregates_by_actor(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<crate::AuditActorAggregate>, OrbitError> {
        self.store.get_audit_event_aggregates_by_actor(since)
    }

    fn get_failure_incidents(
        &self,
        query: &crate::FailureIncidentQuery,
    ) -> Result<crate::FailureIncidentReport, OrbitError> {
        self.store.get_failure_incidents(query)
    }

    fn prune_audit_events(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError> {
        self.store.prune_audit_events(older_than)
    }
}

impl ScopedStore<AuditEvent> for SqliteAuditEventStoreBackend {
    type Err = OrbitError;

    fn strategy(&self) -> ScopeStrategy {
        ScopeStrategy::GlobalOnly
    }

    fn get_workspace(&self, _key: &str) -> Result<Option<AuditEvent>, OrbitError> {
        Ok(None)
    }

    fn get_global(&self, key: &str) -> Result<Option<AuditEvent>, OrbitError> {
        let id = key
            .parse::<i64>()
            .map_err(|e| OrbitError::Store(format!("invalid audit event id '{key}': {e}")))?;
        self.store.get_audit_event(id)
    }
}

#[derive(Clone)]
pub(crate) struct SqliteTaskReservationStoreBackend {
    pub(crate) store: Store,
}

impl TaskReservationStoreBackend for SqliteTaskReservationStoreBackend {
    fn inspect_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<ActiveTaskReservation>, OrbitError> {
        self.store
            .inspect_active_task_reservations(workspace_orbit_dir, workspace_id)
    }

    fn list_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<TaskReservationListResult, OrbitError> {
        self.store
            .list_active_task_reservations(workspace_orbit_dir, workspace_id)
    }

    fn check_task_reservation_conflicts(
        &self,
        params: TaskReservationCheckParams,
    ) -> Result<TaskReservationCheckResult, OrbitError> {
        self.store.check_task_reservation_conflicts(&params)
    }

    fn reserve_task_reservation(
        &self,
        params: TaskReservationReserveParams,
    ) -> Result<TaskReservationReserveResult, OrbitError> {
        self.store.reserve_task_reservation(&params)
    }

    fn release_task_reservation(
        &self,
        params: TaskReservationReleaseParams,
    ) -> Result<TaskReservationReleaseResult, OrbitError> {
        self.store.release_task_reservation(&params)
    }

    fn release_task_reservations_by_owner_run_id(
        &self,
        params: TaskReservationReleaseByOwnerParams,
    ) -> Result<TaskReservationReleaseByOwnerResult, OrbitError> {
        self.store
            .release_task_reservations_by_owner_run_id(&params)
    }

    fn list_owned_task_reservation_conflicts(
        &self,
        params: TaskReservationOwnedConflictsParams,
    ) -> Result<TaskReservationOwnedConflictsResult, OrbitError> {
        self.store.list_owned_task_reservation_conflicts(&params)
    }

    fn acquire_workspace_claim(
        &self,
        params: WorkspaceClaimAcquireParams,
    ) -> Result<WorkspaceClaimAcquireResult, OrbitError> {
        self.store.acquire_workspace_claim(&params)
    }

    fn release_workspace_claim(
        &self,
        params: WorkspaceClaimReleaseParams,
    ) -> Result<WorkspaceClaimReleaseResult, OrbitError> {
        self.store.release_workspace_claim(&params)
    }

    fn show_workspace_claim(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<WorkspaceClaimStatusResult, OrbitError> {
        self.store
            .show_workspace_claim(workspace_orbit_dir, workspace_id)
    }

    fn check_workspace_claim(
        &self,
        params: WorkspaceClaimCheckParams,
    ) -> Result<WorkspaceClaimCheckResult, OrbitError> {
        self.store.check_workspace_claim(&params)
    }
}
