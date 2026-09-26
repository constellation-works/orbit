use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::OrbitId;
use orbit_types::plugin::InstalledPlugin;
use orbit_types::policy::PolicyDef;
use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskPriority, TaskStatus, normalize_task_tags, task_matches_tags,
};
use orbit_types::telemetry::AuditEvent;
use orbit_types::tool::StoredTool;
use orbit_types::workflow::ExecutorDef;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::friction::{
    FrictionAddParams, FrictionListFilter, FrictionRehomeOutcome, FrictionRehomeParams,
    FrictionReportedCount, FrictionUpdateParams, StoredFrictionRecord,
};
use super::invocation::{
    ActivityInvocationMetrics, AgentInvocationMetrics, InvocationAccountingFact,
    InvocationAccountingQuery, InvocationInsertParams, InvocationQuery, InvocationRecord,
    TaskInvocationMetrics, ToolInvocationMetrics,
};
use super::params::*;
use super::routine::{
    RoutineCursor, RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutinePauseRecord,
};
use super::session_log::{SessionLogAppendParams, SessionLogEntry, SessionLogFilter};
use super::v2_audit::{V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventRow};

use crate::contracts::incident::{FailureIncidentQuery, FailureIncidentReport};
use crate::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditRoleAggregate, AuditToolAggregate, AuditToolCallCountsByRole,
    AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall, TaskCompletionByComplexity,
};

pub trait TaskStoreBackend: Send + Sync {
    /// The claim's accepted handoff, or `None` when none was accepted.
    fn find_accepted_handoff(
        &self,
        _claim_id: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::AcceptedHandoff>, OrbitError> {
        Err(OrbitError::Store("typed handoff unavailable".into()))
    }

    fn landing_start_requests(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingStartRequest>, OrbitError> {
        Err(OrbitError::Store("handoff outbox unavailable".into()))
    }

    /// The owner's landing attempts, one per handoff. Read-only inspection.
    fn landing_attempts(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingAttempt>, OrbitError> {
        Err(OrbitError::Store("landing attempts unavailable".into()))
    }

    /// Internal lifecycle seam; unavailable backends fail closed.
    fn mutate_execution_claim(
        &self,
        _context: Option<&super::ClaimInvocation>,
        _mutation_id: &str,
        _mutation: &super::ClaimMutation,
    ) -> Result<super::ClaimMutationResult, OrbitError> {
        Err(OrbitError::Store("claim lifecycle unavailable".into()))
    }
    fn inspect_execution_claims(&self) -> Result<Vec<super::ClaimInspection>, OrbitError> {
        Err(OrbitError::Store("claim inspection unavailable".into()))
    }
    /// Repairing claim read: settles an interrupted commit before reading.
    /// Unavailable backends fail closed.
    fn resolve_execution_claims(&self) -> Result<Vec<super::ClaimInspection>, OrbitError> {
        Err(OrbitError::Store("claim inspection unavailable".into()))
    }
    /// Read-only receipt reconciliation. Creates no receipt, binds no run, and
    /// grants no execution authority; the caller authorizes the identity.
    fn lookup_admission(
        &self,
        _identity: &super::AdmissionIdentity,
        _request_id: &str,
    ) -> Result<super::AdmissionLookup, OrbitError> {
        Err(OrbitError::Store("admission lookup unavailable".into()))
    }

    /// Select metadata before hydration, retaining all-envelope index validation.
    fn task_candidates(
        &self,
        filter: &super::TaskListFilter,
        limit: usize,
    ) -> Result<super::TaskCandidates, OrbitError>;
    fn query_task_rows(
        &self,
        filter: &super::TaskListFilter,
        limit: usize,
        residual: super::TaskResidualFilter<'_>,
    ) -> Result<super::TaskPage, OrbitError>;
    /// Direct reads remain strict; list reads tolerate concurrent creation/deletion.
    fn get_task_row(&self, id: &str, list_read: bool)
    -> Result<Option<super::TaskRow>, OrbitError>;
    fn create_task(&self, params: TaskCreateParams) -> Result<Task, OrbitError>;
    /// Durable key admission for automation, sharing ordinary bundle creation.
    fn create_task_idempotent(
        &self,
        _params: TaskCreateParams,
        _key: &str,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Store(
            "idempotent task creation unavailable".into(),
        ))
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError>;
    fn task_status_index(&self) -> Result<BTreeMap<OrbitId, TaskStatus>, OrbitError> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .map(|task| (task.id, task.status))
            .collect())
    }
    /// Return the bounded status projection needed to label one task's
    /// dependency and relation targets. Backends with a workspace-aware
    /// registry override this; the default preserves compatibility for small
    /// test and legacy backends by falling back to their global projection.
    fn task_status_index_for(
        &self,
        _workspace_id: &str,
        _targets: &BTreeSet<String>,
    ) -> Result<BTreeMap<OrbitId, TaskStatus>, OrbitError> {
        self.task_status_index()
    }
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        let required_tags = normalize_task_tags(tags.to_vec());
        let mut tasks = self.list_tasks()?;
        if !required_tags.is_empty() {
            tasks.retain(|task| task_matches_tags(task, &required_tags));
        }
        Ok(tasks)
    }
    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError>;
    fn get_task(&self, id: &str) -> Result<Option<Task>, OrbitError>;
    /// Resolve one task through the owner this machine's coordination registry
    /// has registered for it, instead of only within the caller's workspace.
    ///
    /// Task ids are globally unique and a dependency may legitimately name a
    /// task owned by another workspace on this machine, so a dependency read
    /// has to follow ownership the same way the registry-wide status
    /// projection already does ([`Self::task_status_index`]). The read is
    /// authority-preserving: it never widens what the caller may write, never
    /// writes to the owner's partition, and never reaches another host.
    ///
    /// The default resolves within this backend alone — a backend without an
    /// ownership registry has exactly one authority — so an id it cannot find
    /// is [`RegisteredTaskResolution::Missing`].
    fn registered_task(&self, id: &str) -> Result<RegisteredTaskResolution, OrbitError> {
        Ok(match self.get_task(id)? {
            Some(task) => RegisteredTaskResolution::Resolved(Box::new(task)),
            None => RegisteredTaskResolution::Missing,
        })
    }
    fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError>;
    fn search_tasks_filtered(&self, query: &str, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        let required_tags = normalize_task_tags(tags.to_vec());
        let mut tasks = self.search_tasks(query)?;
        if !required_tags.is_empty() {
            tasks.retain(|task| task_matches_tags(task, &required_tags));
        }
        Ok(tasks)
    }
    fn delete_task(&self, id: &str) -> Result<bool, OrbitError>;

    /// Run `op` while holding this task's write lock.
    ///
    /// A caller that reads a task, decides something from that snapshot, and
    /// then writes needs the read and the write to be one critical section;
    /// locking only the write lets a concurrent update land in between and be
    /// overwritten (ORB-10988). The lock is re-entrant within a thread, so the
    /// per-write locking the backend already does still applies underneath.
    ///
    /// The default is a no-op passthrough for backends with no per-task lock.
    fn with_task_write_lock(
        &self,
        _id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        op()
    }

    /// Atomically apply a freshness-guarded task mutation and its durable
    /// idempotency receipt. Backends that cannot provide one commit point must
    /// reject the operation rather than emulate it with partial writes.
    fn apply_atomic_task_mutation(
        &self,
        _id: &str,
        _params: &AtomicTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError> {
        Err(OrbitError::Store(
            "atomic task mutation is not supported by this backend".to_string(),
        ))
    }

    /// Status counts per complexity bucket from the generated task index.
    /// Default is empty; the v2 store answers from SQLite without bundle reads.
    fn task_completion_by_complexity(&self) -> Result<Vec<TaskCompletionByComplexity>, OrbitError> {
        Ok(Vec::new())
    }

    /// `task_id →` complexity bucket (`low`/`medium`/`hard`/`unset`) from the
    /// generated index. Used to facet invocation metrics without YAML reads.
    fn task_complexity_by_id(&self) -> Result<BTreeMap<OrbitId, String>, OrbitError> {
        Ok(BTreeMap::new())
    }
}

/// How one task id resolved against this machine's task ownership registry.
///
/// Every variant fails closed: only [`Self::Resolved`] carries a body, and
/// neither of the other two may be read as a satisfied prerequisite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredTaskResolution {
    /// A workspace registered on this machine owns the id, and its bundle was
    /// read from the path the registry binds it to.
    Resolved(Box<Task>),
    /// The id is well formed and belongs to a task-id prefix this registry has
    /// never issued or registered, so its authority is another host's. This
    /// machine cannot read the body and must not invent one.
    ForeignAuthority,
    /// The id belongs to a prefix this registry knows, but no readable task is
    /// bound to it here — deleted, never published, or not yet imported.
    Missing,
}

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
    fn list(&self, filter: &FrictionListFilter) -> Result<Vec<StoredFrictionRecord>, OrbitError>;
    fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError>;
    /// Workspace IDs other than this store's that already hold `id`.
    fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError>;
    fn update(
        &self,
        id: &str,
        params: FrictionUpdateParams,
    ) -> Result<StoredFrictionRecord, OrbitError>;
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
    fn tags(&self) -> Result<Vec<String>, OrbitError>;
    fn tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError>;
    fn reported_by_model(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<FrictionReportedCount>, OrbitError>;
    fn stats(&self, tasks: &[Task]) -> Result<Value, OrbitError>;
}

pub trait InvocationStoreBackend: Send + Sync {
    fn insert_invocation_trace_record(
        &self,
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
    fn list_activity_invocation_metrics(
        &self,
    ) -> Result<Vec<ActivityInvocationMetrics>, OrbitError>;
    fn list_agent_invocation_metrics(&self) -> Result<Vec<AgentInvocationMetrics>, OrbitError>;
    fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<TaskInvocationMetrics, OrbitError>;
    fn list_top_task_invocation_metrics(
        &self,
        limit: usize,
    ) -> Result<Vec<TaskInvocationMetrics>, OrbitError>;
    fn list_tool_invocation_metrics(&self) -> Result<Vec<ToolInvocationMetrics>, OrbitError>;
    /// Insert-only change signal for the token scoreboard skip path.
    ///
    /// `Some(n)` is `MAX(id)` over `invocations` (`0` when the table is empty).
    /// `None` means the backend cannot cheaply detect changes; callers rewrite.
    fn invocation_scoreboard_watermark(&self) -> Result<Option<u64>, OrbitError> {
        Ok(None)
    }
}

pub trait V2AuditStoreBackend: Send + Sync {
    fn insert_v2_audit_event(&self, params: &V2AuditEventInsertParams) -> Result<(), OrbitError>;
    fn list_v2_audit_events(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError>;
    fn count_v2_audit_events(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError>;
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

pub trait TaskDocumentStoreBackend: Send + Sync {
    fn update_task_document(
        &self,
        id: &str,
        params: TaskDocumentUpdateParams,
    ) -> Result<(), OrbitError>;
}

pub trait TaskHistoryStoreBackend: Send + Sync {
    fn get_task_comments(&self, id: &str) -> Result<Option<Vec<TaskComment>>, OrbitError>;
    fn get_task_history(&self, id: &str) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError>;
    fn update_task_history(
        &self,
        id: &str,
        params: TaskHistoryUpdateParams,
    ) -> Result<(), OrbitError>;
}

pub trait TaskArtifactStoreBackend: Send + Sync {
    fn get_task_artifact_manifest(
        &self,
        _id: &str,
    ) -> Result<Option<Vec<ArtifactManifestFileV2>>, OrbitError> {
        Err(OrbitError::Store(
            "task artifact manifest read is not supported by this backend".to_string(),
        ))
    }
    fn get_task_artifacts(&self, id: &str) -> Result<Option<Vec<TaskArtifact>>, OrbitError>;
    fn get_task_artifact(
        &self,
        _id: &str,
        _path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        Err(OrbitError::Store(
            "task artifact read is not supported by this backend".to_string(),
        ))
    }
    fn upsert_task_artifacts(
        &self,
        id: &str,
        params: TaskArtifactUpdateParams,
    ) -> Result<(), OrbitError>;
}

pub trait TaskReservationStoreBackend: Send + Sync {
    /// Read active reservations without expiring or otherwise mutating rows.
    fn inspect_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<ActiveTaskReservation>, OrbitError>;

    fn list_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<TaskReservationListResult, OrbitError>;

    fn check_task_reservation_conflicts(
        &self,
        params: TaskReservationCheckParams,
    ) -> Result<TaskReservationCheckResult, OrbitError>;

    fn reserve_task_reservation(
        &self,
        params: TaskReservationReserveParams,
    ) -> Result<TaskReservationReserveResult, OrbitError>;

    fn release_task_reservation(
        &self,
        params: TaskReservationReleaseParams,
    ) -> Result<TaskReservationReleaseResult, OrbitError>;

    fn release_task_reservations_by_owner_run_id(
        &self,
        params: TaskReservationReleaseByOwnerParams,
    ) -> Result<TaskReservationReleaseByOwnerResult, OrbitError>;

    fn list_owned_task_reservation_conflicts(
        &self,
        params: TaskReservationOwnedConflictsParams,
    ) -> Result<TaskReservationOwnedConflictsResult, OrbitError>;

    /// Take the exclusive workspace claim [ADR-0352, ORB-10709], or report the
    /// incumbent that refused it.
    fn acquire_workspace_claim(
        &self,
        params: WorkspaceClaimAcquireParams,
    ) -> Result<WorkspaceClaimAcquireResult, OrbitError>;

    /// Release the claim with its token, or force-release it.
    fn release_workspace_claim(
        &self,
        params: WorkspaceClaimReleaseParams,
    ) -> Result<WorkspaceClaimReleaseResult, OrbitError>;

    /// The active claim after lazy expiry, or `None` when unclaimed.
    fn show_workspace_claim(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<WorkspaceClaimStatusResult, OrbitError>;

    /// Whether a presented token satisfies the active claim. The comparison
    /// stays inside the store so a refusal never has to carry the incumbent's
    /// token back out.
    fn check_workspace_claim(
        &self,
        params: WorkspaceClaimCheckParams,
    ) -> Result<WorkspaceClaimCheckResult, OrbitError>;
}

pub trait ToolStoreBackend: Send + Sync {
    fn list_tools(&self) -> Result<Vec<StoredTool>, OrbitError>;
    fn get_tool(&self, name: &str) -> Result<Option<StoredTool>, OrbitError>;
    fn insert_tool(&self, tool: &StoredTool) -> Result<(), OrbitError>;
    fn delete_tool(&self, name: &str) -> Result<bool, OrbitError>;
    fn set_tool_enabled(&self, name: &str, enabled: bool) -> Result<bool, OrbitError>;
}

/// Host-local plugin records: what is installed, where, and whether it is
/// enabled. Never synced; the workspace pin file is the versioned half.
pub trait PluginStoreBackend: Send + Sync {
    fn list_plugins(&self) -> Result<Vec<InstalledPlugin>, OrbitError>;
    fn get_plugin(&self, name: &str) -> Result<Option<InstalledPlugin>, OrbitError>;
    fn upsert_plugin(&self, plugin: &InstalledPlugin) -> Result<(), OrbitError>;
    fn delete_plugin(&self, name: &str) -> Result<bool, OrbitError>;
    /// Record the Orbit version whose conformance run this plugin passed.
    fn set_plugin_certification(
        &self,
        name: &str,
        orbit_version: Option<&str>,
    ) -> Result<bool, OrbitError>;
    fn set_plugin_enabled(
        &self,
        name: &str,
        enabled: bool,
        grants: &[String],
    ) -> Result<bool, OrbitError>;
}

pub trait AuditEventStoreBackend: Send + Sync {
    fn insert_audit_event_record(&self, params: &AuditEventInsertParams) -> Result<(), OrbitError>;
    fn list_audit_events(&self, filter: &AuditEventFilter) -> Result<Vec<AuditEvent>, OrbitError>;
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
    fn get_audit_event_durations_null_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<i64>, OrbitError>;
    fn get_audit_event_hourly_buckets(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>, OrbitError>;
    fn get_audit_denials_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError>;
    fn get_audit_denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError>;
    fn get_audit_tool_call_counts_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditToolCallCountsByRole>, OrbitError>;
    fn get_audit_tool_call_counts_by_surface_and_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditToolCallCountsBySurfaceAndRole>, OrbitError>;
    /// The same tool-call rows as [`Self::get_audit_tool_call_counts_by_role`],
    /// classified by how each row's identity was established [ORB-10890]. The
    /// buckets are disjoint, so authenticated-only, self-reported-only, and
    /// combined counts all come from one call.
    fn get_audit_tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditAttributionAggregate>, OrbitError>;
    fn get_audit_top_tool_calls(
        &self,
        since: Option<&DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<AuditTopToolCall>, OrbitError>;
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

pub trait ExecutorDefStoreBackend: Send + Sync {
    fn list_executor_defs(&self) -> Result<Vec<ExecutorDef>, OrbitError>;
    fn get_executor_def(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError>;
    fn upsert_executor_def(&self, def: &ExecutorDef) -> Result<(), OrbitError>;
}

pub trait PolicyDefStoreBackend: Send + Sync {
    fn list_policy_defs(&self) -> Result<Vec<PolicyDef>, OrbitError>;
    fn get_policy_def(&self, name: &str) -> Result<Option<PolicyDef>, OrbitError>;
    fn upsert_policy_def(&self, def: &PolicyDef) -> Result<(), OrbitError>;
}
