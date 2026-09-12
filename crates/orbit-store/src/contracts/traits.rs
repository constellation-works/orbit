use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::{Crew, OrbitId};
use orbit_types::policy::PolicyDef;
use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskPriority, TaskStatus, normalize_task_tags, task_matches_tags,
};
use orbit_types::telemetry::AuditEvent;
use orbit_types::tool::StoredTool;
use orbit_types::workflow::{
    ExecutorDef, JobRun, JobRunStartOutcome, JobRunState, KnowledgeRunMetrics, PipelineState,
    RunStateUpdate,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::friction::{
    FrictionAddParams, FrictionListFilter, FrictionReportedCount, FrictionUpdateParams,
    StoredFrictionRecord,
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

pub trait JobRunStoreBackend: Send + Sync {
    /// Exact retry children; missing evidence cannot be replaced by a time-window scan.
    fn job_run_retries(&self, _run_id: &str, _limit: usize) -> Result<Vec<JobRun>, OrbitError> {
        Err(OrbitError::Store("retry lineage lookup unavailable".into()))
    }

    fn automation_job_for_key(&self, _key: &str) -> Result<Option<String>, OrbitError> {
        Err(OrbitError::Store(
            "automation action lookup unavailable".into(),
        ))
    }
    fn insert_automation_job_run(
        &self,
        _job_id: &str,
        _input: serde_json::Value,
        _key: &str,
    ) -> Result<JobRun, OrbitError> {
        Err(OrbitError::Store(
            "automation job admission unavailable".into(),
        ))
    }

    fn list_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError>;
    fn list_job_runs_filtered(&self, query: &JobRunQuery) -> Result<Vec<JobRun>, OrbitError>;
    /// Number of runs matching `query`, ignoring its `limit`.
    fn count_job_runs_filtered(&self, query: &JobRunQuery) -> Result<u64, OrbitError>;
    /// Every recorded `duration_ms` among runs matching `query`, ignoring
    /// its `limit`.
    fn list_job_run_durations_filtered(&self, query: &JobRunQuery) -> Result<Vec<u64>, OrbitError>;
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError>;
    fn list_pending_or_running_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError>;
    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError>;
    /// Atomically admit and link a child run unless its parent has stopped
    /// admissions.
    ///
    /// The parent-state read, child insert, and parent dispatch checkpoint are
    /// committed in one backend transaction. That commit is the
    /// cross-process linearization point shared with an admissions-stop
    /// update: a stop that commits first makes this return
    /// [`ChildJobRunAdmissionOutcome::AdmissionsStopped`], while a child that
    /// commits first is already linked when stop acknowledges.
    fn admit_child_job_run(
        &self,
        params: &ChildJobRunAdmissionParams,
    ) -> Result<ChildJobRunAdmissionOutcome, OrbitError>;
    /// [ORB-10965] Apply a `Start` event to a run, atomically and idempotently.
    ///
    /// Scheduling is at-least-once, so this is the single point that decides
    /// which of several competing or repeated deliveries owns execution. The
    /// read of the current state and the write of the new one happen in one
    /// immediate transaction, so exactly one caller can observe
    /// [`JobRunStartOutcome::Started`].
    ///
    /// A redelivery from the owner already recorded on the run is a no-op:
    /// [`JobRunStartOutcome::AlreadyStarted`], with `started_at`, the owner
    /// identity, and every checkpoint left untouched. A delivery from a
    /// *different* owner loses to the incumbent and fails with
    /// [`OrbitError::JobRunStartConflict`]. Genuinely illegal transitions (a
    /// `Start` from any state other than `pending`, `running`, or terminal)
    /// still fail with [`OrbitError::JobRunStateTransition`].
    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError>;
    /// [ORB-10070] Record `pid` (+ its start-time identity token) as the owner
    /// of a still-`pending` run so orphan reconciliation can distinguish a
    /// queued run with a live worker from one whose worker died. Returns
    /// `false` without writing when the run is missing or no longer pending.
    fn claim_pending_job_run_owner(&self, run_id: &str, pid: u32) -> Result<bool, OrbitError>;
    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError>;
    fn record_job_run_knowledge_metrics(
        &self,
        run_id: &str,
        metrics: KnowledgeRunMetrics,
    ) -> Result<bool, OrbitError>;
    fn record_job_run_crew(&self, run_id: &str, crew: &Crew) -> Result<bool, OrbitError>;
    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError>;
    fn repair_terminal_job_run_timing(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError>;
    fn list_all_pending_or_running_runs(&self) -> Result<Vec<JobRun>, OrbitError>;
    fn archive_job_run(&self, run_id: &str) -> Result<String, OrbitError>;
    fn delete_job_run(&self, run_id: &str) -> Result<String, OrbitError>;
    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;
    /// Pipeline state for a list page in one query per chunk, not one per run.
    ///
    /// Missing runs and unreadable JSON are omitted / `None` rather than failing
    /// the page: the MCP list surface degrades those rows the same way a
    /// per-run `read_run_state` error already does.
    fn read_run_states(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError>;
    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError>;
    /// [ORB-11253] Read-modify-write a run's pipeline state in one immediate
    /// transaction.
    ///
    /// The plain read/write pair cannot express a change that must survive
    /// another writer: the run's state is one document that the engine
    /// (checkpoints, child dispatches) and operator run controls both mutate,
    /// so two interleaved read-modify-write cycles silently drop whichever
    /// change landed in between. `update` also receives the run's current
    /// [`JobRunState`], so a caller that must not mutate a finished run can
    /// refuse inside the same transaction that would otherwise have written.
    /// An `Err` from `update` rolls the transaction back, leaving the stored
    /// state exactly as it was.
    fn update_run_state(
        &self,
        run_id: &str,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError>;
}

/// Durable inputs for one parent-authorized child admission.
#[derive(Debug, Clone)]
pub struct ChildJobRunAdmissionParams {
    pub parent_run_id: String,
    pub parent_step_id: Option<String>,
    pub job_id: String,
    pub action: String,
    pub blocking: bool,
    pub attempt: u32,
    pub scheduled_at: DateTime<Utc>,
    pub input: Option<Value>,
    /// The operation-mode grant this admission must still satisfy, when the
    /// parent was admitted under one [ORB-11332]. `None` keeps the
    /// pre-existing stop-only guard.
    pub authority: Option<ChildAdmissionAuthority>,
}

/// The grant-bound facts a child admission is rechecked against inside the
/// admission transaction: grant validity at `now`, exact revision, finite
/// scope, an unclaimed task, and free leaf capacity [ORB-11332].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildAdmissionAuthority {
    pub grant_id: String,
    pub grant_revision: u32,
    /// The task this child would carry. `Some` only for a leaf child: it is
    /// checked against the grant scope and against live claims of the same
    /// job. Nested children (gate, PR) inherit their parent's claim.
    pub task_id: Option<String>,
    /// Effective ceiling on live runs of the child's job. `Some` only for a
    /// leaf child; nested children are not counted as leaf capacity.
    pub leaf_ceiling: Option<u32>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChildJobRunAdmissionOutcome {
    Admitted(Box<JobRun>),
    AdmissionsStopped,
    /// The grant-bound recheck refused the child; `reason` is one of
    /// `grant_missing`, `grant_revision_changed`, `grant_expired`,
    /// `grant_stopped`, `grant_revoked`, `outside_grant_scope`,
    /// `task_claimed`, or `capacity_saturated`.
    Refused {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct JobRunStepParams {
    pub step_index: usize,
    pub target_type: orbit_types::workflow::JobTargetType,
    pub target_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub agent_response_json: Option<Value>,
    pub state: JobRunState,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

pub trait ToolStoreBackend: Send + Sync {
    fn list_tools(&self) -> Result<Vec<StoredTool>, OrbitError>;
    fn get_tool(&self, name: &str) -> Result<Option<StoredTool>, OrbitError>;
    fn insert_tool(&self, tool: &StoredTool) -> Result<(), OrbitError>;
    fn delete_tool(&self, name: &str) -> Result<bool, OrbitError>;
    fn set_tool_enabled(&self, name: &str, enabled: bool) -> Result<bool, OrbitError>;
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
