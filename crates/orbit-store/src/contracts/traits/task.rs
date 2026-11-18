use crate::contracts::params::*;
use crate::contracts::{TaskCompletionByComplexity, TaskDefectFiling, TaskTerminalTransition};
use orbit_common::OrbitError;
use orbit_types::identity::OrbitId;
use orbit_types::task::{
    ExternalRef, Task, TaskEnvelopeV2, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub trait TaskStoreBackend: Send + Sync {
    /// Persisted decision projections for this partition.
    fn task_decision_snapshots(
        &self,
    ) -> Result<BTreeMap<String, crate::contracts::TaskDecisionSnapshot>, OrbitError>;

    /// Record decision projections and their request owners.
    fn record_task_decision_snapshots(
        &self,
        _records: &[crate::contracts::TaskDecisionSnapshot],
    ) -> Result<bool, OrbitError>;

    /// Registered owner of a previously observed request, if any.
    fn decision_request_task_id(&self, _request_id: &str) -> Result<Option<String>, OrbitError>;

    /// Accepted handoff for a claim, if one was accepted.
    fn find_accepted_handoff(
        &self,
        _claim_id: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::AcceptedHandoff>, OrbitError>;

    /// Landing start requests for this owner.
    fn landing_start_requests(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingStartRequest>, OrbitError>;

    /// Every handoff this owner accepted.
    fn accepted_handoffs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::AcceptedHandoff>, OrbitError>;

    /// Candidate the task's latest claim settlement kept, if any.
    fn kept_claim_candidate(
        &self,
        _task_id: &str,
        _machine_id: &str,
    ) -> Result<Option<crate::contracts::KeptClaimCandidate>, OrbitError>;

    /// Owner landing attempts, one per handoff.
    fn landing_attempts(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingAttempt>, OrbitError>;

    /// Owner landing attempt for one handoff.
    fn landing_attempt(
        &self,
        _handoff_id: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingAttempt>, OrbitError>;

    /// Durable operator retry receipt for one handoff.
    fn landing_retry_receipt(
        &self,
        _handoff_id: &str,
        _request_key: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingRetryReceipt>, OrbitError>;

    /// Current or archived attempt for one handoff.
    fn landing_attempt_record(
        &self,
        _handoff_id: &str,
        _attempt: u32,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingAttempt>, OrbitError>;

    /// Internal lifecycle seam; unavailable backends fail closed.
    fn mutate_execution_claim(
        &self,
        context: Option<&crate::contracts::ClaimInvocation>,
        mutation_id: &str,
        mutation: &crate::contracts::ClaimMutation,
    ) -> Result<crate::contracts::ClaimMutationResult, OrbitError> {
        self.mutate_execution_claim_outcome(context, mutation_id, mutation)
            .map(|outcome| outcome.result)
    }
    /// Apply a claim mutation and report whether it committed or replayed.
    fn mutate_execution_claim_outcome(
        &self,
        _context: Option<&crate::contracts::ClaimInvocation>,
        _mutation_id: &str,
        _mutation: &crate::contracts::ClaimMutation,
    ) -> Result<crate::contracts::ClaimMutationOutcome, OrbitError>;
    /// Refuse a worker invocation whose claim lost its authority.
    fn verify_worker_claim(
        &self,
        _context: &crate::contracts::ClaimInvocation,
    ) -> Result<(), OrbitError>;
    /// Mid-run steering records for a claimed leaf.
    fn worker_claim_steering(
        &self,
        _context: &crate::contracts::ClaimInvocation,
    ) -> Result<crate::contracts::ClaimSteeringReading, OrbitError>;
    /// Steering watermark the task's most recent claim recorded.
    fn task_claim_steering(
        &self,
        _task_id: &str,
    ) -> Result<Option<orbit_types::workflow::SteeringWatermark>, OrbitError>;
    /// Upsert one executor's census row.
    fn record_executor_census(
        &self,
        _entry: &crate::contracts::ExecutorCensusEntry,
    ) -> Result<(), OrbitError>;
    /// Census rows live at the given time.
    fn live_executor_census(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::contracts::ExecutorCensusEntry>, OrbitError>;
    /// Every recorded census row, live or not.
    fn recorded_executor_census(
        &self,
    ) -> Result<Vec<crate::contracts::ExecutorCensusEntry>, OrbitError>;
    /// Inspect execution claims.
    fn inspect_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError>;
    /// Inspect claims whose phase protects a footprint.
    fn inspect_live_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError>;
    /// Inspect one task's claims.
    fn inspect_task_execution_claims(
        &self,
        _task_id: &str,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError>;
    /// Logical storage diagnostic for admission and mutation receipts.
    fn inspect_admission_receipt_storage(
        &self,
    ) -> Result<crate::contracts::AdmissionReceiptStorage, OrbitError>;
    /// Settle an interrupted commit, then read execution claims.
    fn resolve_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError>;
    /// Settle an interrupted commit, then read footprint-protecting claims.
    fn resolve_live_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError>;
    /// Settle an interrupted commit, then read one task's claims.
    fn resolve_task_execution_claims(
        &self,
        _task_id: &str,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError>;
    /// One claim and its lifecycle state, whatever its phase.
    fn execution_claim_state(
        &self,
        _claim_id: &str,
    ) -> Result<Option<crate::contracts::ClaimInspection>, OrbitError>;
    /// Record operator-observed leaf-run evidence for a settled claim.
    fn record_hydrated_leaf_evidence(
        &self,
        _claim_id: &str,
        _report: crate::contracts::LeafRunReport,
    ) -> Result<crate::contracts::LeafEvidenceWrite, OrbitError>;
    /// Record a follower's late report of a settled claim's leaf effort.
    fn record_late_leaf_report(
        &self,
        _claim_id: &str,
        _machine_id: &str,
        _run_id: &str,
        _effort: Option<String>,
        _crew: Option<String>,
        _model: Option<String>,
    ) -> Result<crate::contracts::LateLeafReportWrite, OrbitError>;
    /// Retain one task-branch CI receipt, or queue it for a protecting claim.
    fn record_deferred_branch_observation(
        &self,
        _observation: &crate::contracts::DeferredBranchObservation,
    ) -> Result<crate::contracts::BranchObservationOutcome, OrbitError>;
    /// Admit an owner-local drain wave through the pull admission section.
    fn admit_owner_wave(
        &self,
        candidates: &[crate::contracts::OwnerWaveCandidate],
    ) -> Result<Vec<crate::contracts::OwnerWaveAdmission>, OrbitError>;
    /// Repairs still waiting for a repair leaf, with take eligibility.
    fn pending_repairs(
        &self,
        _owner: &crate::contracts::AdmissionIdentity,
    ) -> Result<Vec<crate::contracts::PendingRepair>, OrbitError>;
    /// Reconcile one admission receipt without creating or binding anything.
    fn lookup_admission(
        &self,
        _identity: &crate::contracts::AdmissionIdentity,
        _request_id: &str,
    ) -> Result<crate::contracts::AdmissionLookup, OrbitError>;

    /// Run `op` as one read section sharing a single boundary acquisition.
    fn in_read_section(
        &self,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError>;

    /// Select metadata before hydration, retaining all-envelope index validation.
    fn task_candidates(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<crate::contracts::TaskCandidates, OrbitError>;
    /// Observe settled task metadata without coordination or recovery.
    fn observe_task_candidates(
        &self,
        _filter: &crate::contracts::TaskListFilter,
        _limit: usize,
    ) -> Result<crate::contracts::TaskCandidates, OrbitError>;
    /// Observe indexed page keys without coordination or recovery.
    fn observe_task_candidate_keys(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<Option<crate::contracts::TaskCandidateKeys>, OrbitError>;
    /// Observe several indexed pages in request order through one freshness
    /// validation. If the index cannot serve all filters, return `None` so
    /// the caller can use envelope selection. A freshness failure applies to
    /// the whole batch, while an indexed query failure belongs to its page.
    fn observe_task_candidate_key_pages(
        &self,
        _pages: &[(crate::contracts::TaskListFilter, usize)],
    ) -> Result<Option<Vec<Result<crate::contracts::TaskCandidateKeys, OrbitError>>>, OrbitError>
    {
        Ok(None)
    }
    /// Select a fully indexed page from the generated index alone, reading no
    /// envelope; `None` when the filter or the index cannot answer that way.
    fn task_candidate_keys(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<Option<crate::contracts::TaskCandidateKeys>, OrbitError>;
    /// Every registered bundle id in newest-first listing order.
    fn registered_task_bundle_ids(&self) -> Result<Option<Vec<String>>, OrbitError>;
    fn query_task_rows(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
        residual: crate::contracts::TaskResidualFilter<'_>,
        hydration: crate::contracts::TaskListHydration,
    ) -> Result<crate::contracts::TaskPage, OrbitError>;
    /// Direct reads remain strict; list reads tolerate concurrent creation/deletion.
    fn get_task_row(
        &self,
        id: &str,
        list_read: bool,
    ) -> Result<Option<crate::contracts::TaskRow>, OrbitError>;
    /// Read one task row bypassing the listed-row cache.
    fn get_task_row_uncached(
        &self,
        id: &str,
    ) -> Result<Option<crate::contracts::TaskRow>, OrbitError>;
    /// Envelopes of the listed `ids`, one keyed read per id: no index
    /// validation and no body documents. An id with no settled bundle is absent.
    fn task_envelopes_for_ids(
        &self,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<TaskEnvelopeV2>, OrbitError>;
    fn create_task(&self, params: TaskCreateParams) -> Result<Task, OrbitError>;
    /// Create a task under a durable key, reporting whether it replayed.
    fn create_task_idempotent(
        &self,
        _params: TaskCreateParams,
        _key: &str,
    ) -> Result<(Task, bool), OrbitError>;

    /// Published task for an automation action key, if readable.
    fn automation_task_for_key(&self, _key: &str) -> Result<Option<Task>, OrbitError>;

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError>;
    /// Status projection for every task.
    fn task_status_index(&self) -> Result<BTreeMap<OrbitId, TaskStatus>, OrbitError>;
    /// Bounded status projection for one task's dependency targets.
    fn task_status_index_for(
        &self,
        _workspace_id: &str,
        _targets: &BTreeSet<String>,
    ) -> Result<BTreeMap<OrbitId, TaskStatus>, OrbitError>;
    /// Every task with its status and last update, for change watching.
    fn task_change_stamps(&self)
    -> Result<Arc<Vec<crate::contracts::TaskChangeStamp>>, OrbitError>;
    /// Bodyless scan identity inputs for every task.
    fn task_metadata_scan_stamps(
        &self,
    ) -> Result<Vec<crate::contracts::TaskChangeStamp>, OrbitError>;
    /// Which of the given ids carry a tag.
    fn task_ids_with_tag(
        &self,
        tag: &str,
        task_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError>;
    /// List tasks carrying all of the given tags.
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError>;
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
    /// Resolve one task through the owner its registry binds it to.
    fn registered_task(&self, id: &str) -> Result<RegisteredTaskResolution, OrbitError>;
    /// Status history of a dependency target through its registered owner.
    fn registered_task_history(
        &self,
        _id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError>;
    /// Workspace partition the registry binds an id to, if any.
    fn registered_task_workspace(&self, _id: &str) -> Result<Option<String>, OrbitError>;
    fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError>;
    /// Search tasks by query, filtered to those carrying all tags.
    fn search_tasks_filtered(&self, query: &str, tags: &[String]) -> Result<Vec<Task>, OrbitError>;
    /// Stream tasks matching a query to `visit` until it returns false.
    fn search_tasks_visit(
        &self,
        query: &str,
        tags: &[String],
        statuses: Option<&[TaskStatus]>,
        admit: &dyn Fn(&Task) -> bool,
        visit: &mut dyn FnMut(Task) -> bool,
    ) -> Result<(), OrbitError>;
    fn delete_task(&self, id: &str) -> Result<bool, OrbitError>;

    /// Run `op` while holding a task's write lock.
    fn with_task_write_lock(
        &self,
        _id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError>;

    /// Run an operator comment-only update under a task's write lock.
    fn with_task_comment_write_lock(
        &self,
        _id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError>;

    /// Read a stable dependency snapshot while another task is mutated.
    fn with_task_read_lock(
        &self,
        _id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError>;

    /// Atomically apply a freshness-guarded task mutation and its receipt.
    fn apply_atomic_task_mutation(
        &self,
        _id: &str,
        _params: &AtomicTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError>;

    /// Create a task using an existing permanent allocation action key.
    fn create_desktop_task(
        &self,
        _params: TaskCreateParams,
        _key: &str,
        _digest: &str,
    ) -> Result<(Task, bool), OrbitError>;
    /// Reconcile an accepted guarded creation.
    fn lookup_desktop_creation(
        &self,
        _key: &str,
        _digest: &str,
    ) -> Result<Option<Task>, OrbitError>;
    /// Recover only the identity of a committed guarded creation.
    fn lookup_committed_creation_id(
        &self,
        _key: &str,
        _digest: &str,
    ) -> Result<Option<String>, OrbitError>;
    /// Coherently read a task without claiming write authority.
    fn read_desktop_task(&self, _id: &str) -> Result<DesktopTaskRead, OrbitError>;
    /// Content revision including documents, history, comments and artifact manifest.
    fn desktop_task_revision(&self, _id: &str) -> Result<String, OrbitError>;
    /// Commit one locked bundle mutation for edits, comments and review state.
    fn apply_desktop_task_mutation(
        &self,
        _id: &str,
        _params: &DesktopTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError>;

    /// Status counts per complexity bucket from the task index.
    fn task_completion_by_complexity(&self) -> Result<Vec<TaskCompletionByComplexity>, OrbitError>;

    /// Complexity bucket per task id from the index.
    fn task_complexity_by_id(&self) -> Result<BTreeMap<OrbitId, String>, OrbitError>;

    /// Terminal tasks whose index `updated_at` is at or after `since`.
    fn task_terminal_transitions(
        &self,
        _since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TaskTerminalTransition>, OrbitError>;

    /// `bug` tasks whose index `created_at` is at or after `since`.
    fn task_defect_filings(
        &self,
        _since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TaskDefectFiling>, OrbitError>;
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
