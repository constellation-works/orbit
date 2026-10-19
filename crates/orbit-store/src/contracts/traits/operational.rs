use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::telemetry::{AuditEvent, ProviderLimitObservation};
use orbit_types::workflow::ProviderCrewOutage;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

use crate::contracts::friction::{
    FrictionAddParams, FrictionLandingOutcome, FrictionListFilter, FrictionRehomeOutcome,
    FrictionRehomeParams, FrictionUpdateParams, StoredFrictionRecord,
};
use crate::contracts::incident::{FailureIncidentQuery, FailureIncidentReport};
use crate::contracts::invocation::{
    ActivityInvocationMetrics, InvocationAccountingFact, InvocationAccountingQuery,
    InvocationInsertParams, InvocationQuery, InvocationRecord, ProviderLedgerEntry,
    TaskInvocationMetrics, ToolInvocationMetrics,
};
use crate::contracts::routine::{
    RoutineCursor, RoutineFireIntentParams, RoutineFireRecord, RoutineFireState,
    RoutinePauseRecord, RoutineSlotAttempts,
};
use crate::contracts::session_log::{SessionLogAppendParams, SessionLogEntry, SessionLogFilter};
use crate::contracts::v2_audit::{
    V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventRow, V2AuditEventTailPage,
};
use crate::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditRoleAggregate, AuditToolAggregate,
};

pub trait SessionLogStoreBackend: Send + Sync {
    fn append(&self, params: SessionLogAppendParams) -> Result<SessionLogEntry, OrbitError>;
    fn list(&self, filter: SessionLogFilter) -> Result<Vec<SessionLogEntry>, OrbitError>;
    fn resolve(&self, id: &str) -> Result<SessionLogEntry, OrbitError>;
}

pub trait RoutineStoreBackend: Send + Sync {
    fn routine_cursor(&self, routine_name: &str) -> Result<Option<RoutineCursor>, OrbitError>;
    fn routine_record_baseline(
        &self,
        routine_name: &str,
        baseline_at: &str,
    ) -> Result<bool, OrbitError>;
    fn routine_record_fire_intent(
        &self,
        intent: &RoutineFireIntentParams,
    ) -> Result<bool, OrbitError>;
    fn routine_mark_fire_dispatched(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<(), OrbitError>;
    fn routine_mark_fire_outcome(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        state: RoutineFireState,
        detail: Option<&str>,
    ) -> Result<(), OrbitError>;
    fn routine_latest_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError>;
    /// Newest unfinished (`intent` or `dispatched`) fire, if one is in flight.
    fn routine_blocking_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError>;
    /// Newest fire whose state is not `skipped`.
    fn routine_latest_non_skip_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError>;
    /// Highest attempt and non-skipped count for one slot.
    ///
    /// `None` when the slot has no fires. Skipped rows raise `highest`
    /// without increasing `non_skipped`.
    fn routine_slot_attempts(
        &self,
        routine_name: &str,
        slot: &str,
    ) -> Result<Option<RoutineSlotAttempts>, OrbitError>;
    fn routine_unresolved_fires(&self) -> Result<Vec<RoutineFireRecord>, OrbitError>;
    fn routine_recent_fires(
        &self,
        routine_name: &str,
        limit: usize,
    ) -> Result<Vec<RoutineFireRecord>, OrbitError>;
    fn routine_pause(&self, routine_name: &str, actor: &str) -> Result<bool, OrbitError>;
    fn routine_resume(&self, routine_name: &str) -> Result<bool, OrbitError>;
    fn routine_pauses(&self) -> Result<BTreeMap<String, RoutinePauseRecord>, OrbitError>;
}

pub trait FrictionStoreBackend: Send + Sync {
    fn add(&self, params: FrictionAddParams) -> Result<StoredFrictionRecord, OrbitError>;
    /// Insert `params`, or return the workspace record that already carries
    /// `dedupe_key`.
    ///
    /// The lookup and the insert share one immediate write transaction, so two
    /// callers that both missed an earlier read still resolve to one id. The
    /// key is the exact `dedupe-key:` line in the body; that line is what a
    /// later caller matches, and the body must contain it. The oldest match
    /// in this workspace is the reusable record.
    fn add_or_reuse(
        &self,
        dedupe_key: &str,
        params: FrictionAddParams,
    ) -> Result<StoredFrictionRecord, OrbitError>;
    fn list(&self, filter: &FrictionListFilter) -> Result<Vec<StoredFrictionRecord>, OrbitError>;
    fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError>;
    /// Workspace IDs other than this store's that already hold `id`.
    fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError>;
    fn update(
        &self,
        id: &str,
        params: FrictionUpdateParams,
    ) -> Result<StoredFrictionRecord, OrbitError>;
    /// Raise, without writing, every refusal [`Self::rehome`] can make that
    /// does not need the write lock, as it would after `edits` land. Lets a
    /// caller that applies `edits` first learn that the move will be refused.
    fn preflight_rehome(
        &self,
        id: &str,
        params: &FrictionRehomeParams,
        edits: &FrictionUpdateParams,
    ) -> Result<(), OrbitError>;
    /// Move `id` into its owning workspace on this host and resolve the
    /// source with a pointer to the new record, atomically.
    fn rehome(
        &self,
        id: &str,
        params: FrictionRehomeParams,
    ) -> Result<FrictionRehomeOutcome, OrbitError>;
    fn resolve(
        &self,
        id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError>;
    fn resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError>;
    /// Resolve as a task-completion side effect: an already-resolved record
    /// is returned untouched, and `Ok(None)` means the record does not exist.
    fn auto_resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError>;
    /// Resolve and append verified landing evidence atomically, preserving
    /// already-resolved records and concurrent body edits.
    fn auto_resolve_by_landing(
        &self,
        id: &str,
        task_id: &str,
        commit: &str,
        pr_number: Option<u64>,
        resolved_at: DateTime<Utc>,
    ) -> Result<FrictionLandingOutcome, OrbitError>;
    fn tags(&self) -> Result<Vec<String>, OrbitError>;
    fn tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError>;
    fn stats(&self, tasks: &[Task]) -> Result<Value, OrbitError>;
}

/// [ORB-14695] This host's provider usage limits, kept in the host-global
/// database because a limit belongs to the provider login on this host.
/// [ORB-16379] So does a crew outage: a crew this host's provider was
/// unavailable for.
pub trait ProviderLimitStoreBackend: Send + Sync {
    /// Record `observation` as the latest for its provider, model scope and
    /// window, unless a newer one is stored. Concurrent drains write the same
    /// rows, so an older observation never replaces a newer one. Returns
    /// whether `observation` was stored.
    fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<bool, OrbitError>;
    /// The latest observation per provider, model scope and window, newest
    /// first.
    fn provider_limits(&self) -> Result<Vec<ProviderLimitObservation>, OrbitError>;
    /// [ORB-16379] Record `outage` for its provider and crew, unless the
    /// stored one lasts at least as long. Returns whether it was stored.
    fn record_provider_crew_outage(&self, outage: &ProviderCrewOutage) -> Result<bool, OrbitError>;
    /// [ORB-16379] Every crew outage lasting past `now`.
    fn provider_crew_outages(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProviderCrewOutage>, OrbitError>;
}

pub trait InvocationStoreBackend: Send + Sync {
    /// Records one invocation under `workspace_id`, the workspace whose run
    /// produced it. The caller supplies it from its own runtime binding, never
    /// from `params`, which can arrive over the metrics ingest endpoint.
    fn insert_invocation_trace_record(
        &self,
        workspace_id: &str,
        params: &InvocationInsertParams,
    ) -> Result<(), OrbitError>;
    fn list_invocation_records(
        &self,
        filter: &InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError>;
    fn list_invocation_accounting_facts(
        &self,
        query: &InvocationAccountingQuery,
    ) -> Result<Vec<InvocationAccountingFact>, OrbitError>;
    /// Every invocation of the provider recorded at or after `since`,
    /// oldest first, across every workspace on this host [ORB-14699].
    /// `provider_names` are the names the provider goes by: its canonical
    /// name and aliases. An invocation recorded without a provider is
    /// attributed by its agent.
    fn list_provider_ledger_entries(
        &self,
        provider_names: &[String],
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderLedgerEntry>, OrbitError>;
    fn list_activity_invocation_metrics(
        &self,
    ) -> Result<Vec<ActivityInvocationMetrics>, OrbitError>;
    fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<TaskInvocationMetrics, OrbitError>;
    fn list_tool_invocation_metrics(&self) -> Result<Vec<ToolInvocationMetrics>, OrbitError>;
}

pub trait V2AuditStoreBackend: Send + Sync {
    fn insert_v2_audit_event(&self, params: &V2AuditEventInsertParams) -> Result<(), OrbitError>;
    fn list_v2_audit_events(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError>;
    fn count_v2_audit_events(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError>;
    /// Filtered total and one chronological tail page from a single read snapshot.
    ///
    /// `filter.limit` and `filter.offset` are the tail window: `offset` counts
    /// backward from the newest matching row, and the returned rows are oldest
    /// first inside that window. `oldest_first` is ignored. An offset at or
    /// past the end yields an empty page and the snapshot's total. A row that
    /// commits after the count cannot change the page relative to that total.
    fn list_v2_audit_event_tail(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<V2AuditEventTailPage, OrbitError>;
    /// Newest matching envelope rows for each run, capped independently so a
    /// busy earlier run cannot consume a page-wide LIMIT.
    fn list_v2_audit_events_for_runs_partitioned(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
        body_kind: Option<&str>,
        per_run_limit: usize,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError>;
    /// Run ids in `run_ids` that have at least one reconstructable v2 envelope
    /// row. Used to distinguish `unavailable` from `not_attempted`.
    fn list_v2_audit_run_ids_with_events(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
    ) -> Result<HashSet<String>, OrbitError>;
}

pub trait AuditEventStoreBackend: Send + Sync {
    fn insert_audit_event_record(&self, params: &AuditEventInsertParams) -> Result<(), OrbitError>;
    fn list_audit_events(&self, filter: &AuditEventFilter) -> Result<Vec<AuditEvent>, OrbitError>;
    fn list_audit_events_by_ids(
        &self,
        ids: &[i64],
        workspace_id: Option<&str>,
    ) -> Result<Vec<AuditEvent>, OrbitError>;
    fn get_audit_event(&self, id: i64) -> Result<Option<AuditEvent>, OrbitError>;
    fn get_audit_event_stats(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<(i64, i64, i64, i64, f64, i64), OrbitError>;
    fn get_audit_event_durations(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<Vec<i64>, OrbitError>;
    fn get_audit_event_hourly_buckets(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>, OrbitError>;
    fn get_audit_denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError>;
    /// Audited tool invocations, classified by how each row's identity was
    /// established [ORB-10890]. The
    /// buckets are disjoint, so authenticated-only, self-reported-only, and
    /// combined counts all come from one call.
    fn get_audit_tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditAttributionAggregate>, OrbitError>;
    fn get_audit_event_aggregates_by_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolAggregate>, OrbitError>;
    fn get_audit_event_aggregates_by_role(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditRoleAggregate>, OrbitError>;
    /// The same window as [`Self::get_audit_event_aggregates_by_role`], grouped
    /// by canonical actor instead of the raw `role` label [ORB-10888].
    fn get_audit_event_aggregates_by_actor(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditActorAggregate>, OrbitError>;
    /// Failure incidents grouped from the raw failed/denied rows in `query`'s
    /// window [ORB-10871]. A derived view: it neither mutates nor withholds
    /// any row that `list_audit_events` would return.
    fn get_failure_incidents(
        &self,
        query: &FailureIncidentQuery,
    ) -> Result<FailureIncidentReport, OrbitError>;
    fn prune_audit_events(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError>;
}
