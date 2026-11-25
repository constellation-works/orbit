use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEvent;
use std::collections::HashSet;

use crate::contracts::invocation::{
    ActivityInvocationMetrics, InvocationAccountingFact, InvocationAccountingQuery,
    InvocationInsertParams, InvocationQuery, InvocationRecord, ProviderLedgerEntry,
    TaskInvocationMetrics, ToolInvocationMetrics,
};
use crate::contracts::v2_audit::{
    V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventRow, V2AuditEventTailPage,
    V2AuditKindForwardPayloads, V2AuditKindForwardQuery,
};
use crate::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditRoleAggregate, AuditToolAggregate, AuditToolModelAggregate,
};

pub trait InvocationStoreBackend: Send + Sync {
    /// Record immutable launch identity before a provider starts.
    fn begin_agent_execution(
        &self,
        workspace: &str,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        identity: &orbit_types::telemetry::AgentExecutionIdentity,
    ) -> Result<i64, OrbitError>;
    /// Launch identities of one workspace-qualified run in launch order.
    fn agent_executions(
        &self,
        workspace: &str,
        run_id: &str,
    ) -> Result<Vec<crate::contracts::AgentExecutionRecord>, OrbitError>;
    /// Launch identities in a workspace and date window, in launch order.
    /// Unlike completed-invocation reads, this keeps launches whose
    /// invocation never completed.
    fn list_agent_executions(
        &self,
        query: &crate::contracts::AgentExecutionQuery,
    ) -> Result<Vec<crate::contracts::AgentExecutionRecord>, OrbitError>;

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
    /// Latest recorded model per run for one activity in one workspace.
    /// Reads only invocation identity/model columns, without task/tool hydration.
    /// A latest invocation with no model stays unknown, even if an older one
    /// recorded a model. Runs with no invocation are absent.
    fn latest_activity_models(
        &self,
        workspace_id: &str,
        activity_id: &str,
        run_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, OrbitError>;
    fn list_invocation_accounting_facts(
        &self,
        query: &InvocationAccountingQuery,
    ) -> Result<Vec<InvocationAccountingFact>, OrbitError>;
    /// Every invocation of the provider recorded at or after `since`,
    /// oldest first, across every workspace on this host [ORB-14699].
    /// `provider_names` are the names the provider goes by: its canonical
    /// name and aliases. An invocation recorded without a provider is
    /// attributed by its agent. `account` names a crew account [ORB-17288];
    /// `None` reads the default login's invocations, those recorded without
    /// an account.
    fn list_provider_ledger_entries(
        &self,
        provider_names: &[String],
        account: Option<&str>,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderLedgerEntry>, OrbitError>;
    /// Activity aggregates for one workspace partition, all time. Rows
    /// recorded under another workspace, and legacy rows without an
    /// attributed workspace, never match. The provider ledger
    /// ([`Self::list_provider_ledger_entries`]) stays host-wide by contract.
    fn list_activity_invocation_metrics(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ActivityInvocationMetrics>, OrbitError>;
    fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<TaskInvocationMetrics, OrbitError>;
    /// Tool aggregates for one workspace partition, all time. Rows recorded
    /// under another workspace, and legacy rows without an attributed
    /// workspace, never match.
    fn list_tool_invocation_metrics(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ToolInvocationMetrics>, OrbitError>;
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
    /// Payload text for a kind-filtered forward scan, from one read snapshot.
    ///
    /// The window is the oldest `query.scan_limit` rows for the workspace,
    /// run and source. Payloads come back only for `canonical_event_type` and
    /// for event types outside `known_event_types`. `past_scan_limit` is
    /// whether a further raw row exists; it does not depend on which payloads
    /// were returned.
    fn list_v2_audit_kind_forward_payloads(
        &self,
        query: &V2AuditKindForwardQuery<'_>,
    ) -> Result<V2AuditKindForwardPayloads, OrbitError>;
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
    /// Event count for `since`, without duration samples. Same population as
    /// the total returned by [`Self::get_audit_event_stats`] when `tool` is
    /// unset.
    fn get_audit_event_count(&self, since: Option<&DateTime<Utc>>) -> Result<i64, OrbitError>;
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
    /// Callable tool calls in the same window, grouped by tool and
    /// server-derived model. Does not change
    /// [`Self::get_audit_event_aggregates_by_tool`].
    fn get_audit_event_aggregates_by_tool_model(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolModelAggregate>, OrbitError>;
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
    fn prune_audit_events(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError>;
}
