use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, Task, TaskHistoryEntry, TaskPriority, TaskStatus};

use super::TaskV2Store;
use crate::contracts::{RegisteredTaskResolution, TaskCreateParams};

/// The task operations callers reach on the concrete store: each enters the
/// commit boundary or the claim section it needs, then delegates.
impl TaskV2Store {
    /// Accepted handoff for a claim, if one was accepted.
    pub fn find_accepted_handoff(
        &self,
        claim_id: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::AcceptedHandoff>, OrbitError> {
        self.claim_boundary()?.find_accepted_handoff(claim_id)
    }

    /// Landing start requests for this owner.
    pub fn landing_start_requests(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingStartRequest>, OrbitError> {
        self.claim_boundary()?.landing_start_requests()
    }

    /// Every handoff this owner accepted.
    pub fn accepted_handoffs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::AcceptedHandoff>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.accepted_handoffs(),
            None => Ok(Vec::new()),
        }
    }

    /// Owner landing attempts, one per handoff.
    pub fn landing_attempts(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingAttempt>, OrbitError> {
        self.claim_boundary()?.landing_attempts()
    }

    /// Owner landing attempt for one handoff.
    pub fn landing_attempt(
        &self,
        handoff_id: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingAttempt>, OrbitError> {
        self.claim_boundary()?.landing_attempt(handoff_id)
    }

    /// Durable operator retry receipt for one handoff.
    pub fn landing_retry_receipt(
        &self,
        handoff_id: &str,
        request_key: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingRetryReceipt>, OrbitError> {
        self.claim_boundary()?
            .landing_retry_receipt(handoff_id, request_key)
    }

    /// Current or archived attempt for one handoff.
    pub fn landing_attempt_record(
        &self,
        handoff_id: &str,
        attempt: u32,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingAttempt>, OrbitError> {
        self.claim_boundary()?
            .landing_attempt_record(handoff_id, attempt)
    }

    /// Candidate the task's latest claim settlement kept, if any.
    pub fn kept_claim_candidate(
        &self,
        task_id: &str,
        machine_id: &str,
    ) -> Result<Option<crate::contracts::KeptClaimCandidate>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.kept_claim_candidate(task_id, machine_id),
            None => Ok(None),
        }
    }

    /// Apply a claim mutation and report whether it committed or replayed.
    pub fn mutate_execution_claim_outcome(
        &self,
        context: Option<&crate::contracts::ClaimInvocation>,
        mutation_id: &str,
        mutation: &crate::contracts::ClaimMutation,
    ) -> Result<crate::contracts::ClaimMutationOutcome, OrbitError> {
        self.claim_boundary()?
            .mutate_execution_claim_outcome(context, mutation_id, mutation)
    }

    /// Refuse a worker invocation whose claim lost its authority.
    pub fn verify_worker_claim(
        &self,
        context: &crate::contracts::ClaimInvocation,
    ) -> Result<(), OrbitError> {
        self.claim_boundary()?.verify_worker_claim(context)
    }

    /// Mid-run steering records for a claimed leaf.
    pub fn worker_claim_steering(
        &self,
        context: &crate::contracts::ClaimInvocation,
    ) -> Result<crate::contracts::ClaimSteeringReading, OrbitError> {
        self.claim_boundary()?.worker_claim_steering(context)
    }

    /// Steering watermark the task's most recent claim recorded.
    pub fn task_claim_steering(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_types::workflow::SteeringWatermark>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.task_claim_steering(task_id),
            None => Ok(None),
        }
    }

    /// Upsert one executor's census row.
    pub fn record_executor_census(
        &self,
        entry: &crate::contracts::ExecutorCensusEntry,
    ) -> Result<(), OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.record_executor_census(entry),
            None => Ok(()),
        }
    }

    /// Census rows live at the given time.
    pub fn live_executor_census(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::contracts::ExecutorCensusEntry>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.live_executor_census(now),
            None => Ok(Vec::new()),
        }
    }

    /// Every recorded census row, live or not.
    pub fn recorded_executor_census(
        &self,
    ) -> Result<Vec<crate::contracts::ExecutorCensusEntry>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.recorded_executor_census(),
            None => Ok(Vec::new()),
        }
    }

    /// Inspect execution claims.
    pub fn inspect_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        self.claim_boundary()?.inspect_execution_claims()
    }

    /// Inspect claims whose phase protects a footprint.
    pub fn inspect_live_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        self.claim_boundary()?.inspect_live_execution_claims()
    }

    /// Inspect one task's claims.
    pub fn inspect_task_execution_claims(
        &self,
        task_id: &str,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        self.claim_boundary()?
            .inspect_task_execution_claims(task_id)
    }

    /// Logical storage diagnostic for admission and mutation receipts.
    pub fn inspect_admission_receipt_storage(
        &self,
    ) -> Result<crate::contracts::AdmissionReceiptStorage, OrbitError> {
        self.claim_boundary()?.inspect_admission_receipt_storage()
    }

    /// Settle an interrupted commit, then read execution claims.
    pub fn resolve_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        self.claim_boundary()?.resolve_execution_claims()
    }

    /// Settle an interrupted commit, then read footprint-protecting claims.
    pub fn resolve_live_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        self.claim_boundary()?.resolve_live_execution_claims()
    }

    /// Settle an interrupted commit, then read one task's claims.
    pub fn resolve_task_execution_claims(
        &self,
        task_id: &str,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        self.claim_boundary()?
            .resolve_task_execution_claims(task_id)
    }

    /// One claim and its lifecycle state, whatever its phase.
    pub fn execution_claim_state(
        &self,
        claim_id: &str,
    ) -> Result<Option<crate::contracts::ClaimInspection>, OrbitError> {
        self.coordination_boundary().map_or(Ok(None), |boundary| {
            boundary.execution_claim_state(claim_id)
        })
    }

    /// Record operator-observed leaf-run evidence for a settled claim.
    pub fn record_hydrated_leaf_evidence(
        &self,
        claim_id: &str,
        report: crate::contracts::LeafRunReport,
    ) -> Result<crate::contracts::LeafEvidenceWrite, OrbitError> {
        self.claim_boundary()?
            .record_hydrated_leaf_evidence(claim_id, report)
    }

    /// Record a follower's late report of a settled claim's leaf effort.
    pub fn record_late_leaf_report(
        &self,
        claim_id: &str,
        machine_id: &str,
        run_id: &str,
        effort: Option<String>,
        crew: Option<String>,
        model: Option<String>,
    ) -> Result<crate::contracts::LateLeafReportWrite, OrbitError> {
        self.claim_boundary()?
            .record_late_leaf_report(claim_id, machine_id, run_id, effort, crew, model)
    }

    /// Retain one task-branch CI receipt, or queue it for a protecting claim.
    pub fn record_deferred_branch_observation(
        &self,
        observation: &crate::contracts::DeferredBranchObservation,
    ) -> Result<crate::contracts::BranchObservationOutcome, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.record_deferred_branch_observation(observation),
            None => Err(OrbitError::Store(
                "deferred branch observations require coordination".into(),
            )),
        }
    }

    /// Admit an owner-local drain wave through the pull admission section.
    /// `owner_machine_id` is the owner's machine, so a second perf task on
    /// that host is withheld. `None` skips claim and owner-local lane holds.
    pub fn admit_owner_wave(
        &self,
        candidates: &[crate::contracts::OwnerWaveCandidate],
        owner_machine_id: Option<&str>,
    ) -> Result<Vec<crate::contracts::OwnerWaveAdmission>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.admit_owner_wave(candidates, owner_machine_id),
            None => Ok(candidates
                .iter()
                .map(|candidate| crate::contracts::OwnerWaveAdmission::admitted(&candidate.task_id))
                .collect()),
        }
    }

    /// Repairs still waiting for a repair leaf, with take eligibility.
    pub fn pending_repairs(
        &self,
        owner: &crate::contracts::AdmissionIdentity,
    ) -> Result<Vec<crate::contracts::PendingRepair>, OrbitError> {
        match self.coordination_boundary() {
            Some(boundary) => boundary.pending_repairs(owner),
            None => Ok(Vec::new()),
        }
    }

    /// Reconcile one admission receipt without creating or binding anything.
    pub fn lookup_admission(
        &self,
        identity: &crate::contracts::AdmissionIdentity,
        request_id: &str,
    ) -> Result<crate::contracts::AdmissionLookup, OrbitError> {
        self.claim_boundary()?
            .lookup_admission(identity, request_id)
    }

    /// Create a task under a durable key, reporting whether it replayed.
    pub fn create_task_idempotent(
        &self,
        params: TaskCreateParams,
        key: &str,
    ) -> Result<(Task, bool), OrbitError> {
        self.create_task_with_key(params, Some(key))
    }

    /// Published task for an automation action key, if readable.
    pub fn automation_task_for_key(&self, key: &str) -> Result<Option<Task>, OrbitError> {
        self.in_boundary(|| self.automation_task_for_key_bare(key))
    }

    /// Run `op` as one read section sharing a single boundary acquisition.
    pub fn in_read_section(
        &self,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        // The boundary's locks are re-entrant per thread, so each read inside
        // `op` runs under this acquisition.
        self.in_boundary(op)
    }

    /// Select metadata before hydration, retaining all-envelope index validation.
    pub fn task_candidates(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<crate::contracts::TaskCandidates, OrbitError> {
        self.in_boundary(|| self.task_candidates_bare(filter, limit))
    }

    /// Select a fully indexed page from the generated index alone, reading no
    /// envelope; `None` when the filter or the index cannot answer that way.
    pub fn task_candidate_keys(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<Option<crate::contracts::TaskCandidateKeys>, OrbitError> {
        self.in_boundary(|| self.task_candidate_keys_bare(filter, limit))
    }

    /// Every registered bundle id in newest-first listing order.
    pub fn registered_task_bundle_ids(&self) -> Result<Option<Vec<String>>, OrbitError> {
        self.in_boundary(|| self.registered_task_bundle_ids_bare())
    }

    pub fn query_task_rows(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
        residual: crate::contracts::TaskResidualFilter<'_>,
        hydration: crate::contracts::TaskListHydration,
    ) -> Result<crate::contracts::TaskPage, OrbitError> {
        self.in_boundary(|| self.query_task_rows_bare(filter, limit, residual, hydration))
    }

    /// [`Self::query_task_rows`] for several pages of one request, in request
    /// order, through one whole-workspace index freshness validation. Each
    /// page keeps its own selection, hydration and stale-row fallback; when
    /// the index cannot serve, every page takes the unbatched route.
    pub fn query_task_row_pages(
        &self,
        pages: &[(crate::contracts::TaskListFilter, usize)],
        residual: crate::contracts::TaskResidualFilter<'_>,
        hydration: crate::contracts::TaskListHydration,
    ) -> Result<Vec<crate::contracts::TaskPage>, OrbitError> {
        self.in_boundary(|| self.query_task_row_pages_bare(pages, residual, hydration))
    }

    /// Direct reads remain strict; list reads tolerate concurrent creation/deletion.
    pub fn get_task_row(
        &self,
        id: &str,
        list_read: bool,
    ) -> Result<Option<crate::contracts::TaskRow>, OrbitError> {
        self.in_boundary(|| self.get_task_row_bare(id, list_read))
    }

    /// Read one task row bypassing the listed-row cache.
    pub fn get_task_row_uncached(
        &self,
        id: &str,
    ) -> Result<Option<crate::contracts::TaskRow>, OrbitError> {
        self.in_boundary(|| TaskV2Store::get_task_row_uncached_bare(self, id))
    }

    /// Envelopes of the listed `ids`, one keyed read per id: no index
    /// validation and no body documents. An id with no settled bundle is absent.
    pub fn task_envelopes_for_ids(
        &self,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<orbit_types::task::TaskEnvelopeV2>, OrbitError> {
        self.in_boundary(|| self.task_envelopes_for_ids_bare(ids))
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        self.in_boundary(|| self.list_tasks_bare())
    }

    /// Status projection for every task.
    pub fn task_status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        self.in_boundary(|| self.task_status_index_bare())
    }

    /// Bounded status projection for one task's dependency targets.
    pub fn task_status_index_for(
        &self,
        workspace_id: &str,
        targets: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        self.in_boundary(|| TaskV2Store::task_status_index_for_bare(self, workspace_id, targets))
    }

    /// Bodyless scan identity inputs for every task.
    pub fn task_metadata_scan_stamps(
        &self,
    ) -> Result<Vec<crate::contracts::TaskChangeStamp>, OrbitError> {
        Ok(TaskV2Store::task_change_stamps(self)?.as_ref().clone())
    }

    /// Creation time and status for settled tasks whose current status
    /// is in `statuses` or whose `updated_at` is at or after `updated_since`.
    /// Backends may project this directly from a validated index.
    pub fn task_lifecycle_stamps_matching(
        &self,
        statuses: &[TaskStatus],
        updated_since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::contracts::TaskLifecycleStamp>, OrbitError> {
        self.in_boundary(|| self.task_lifecycle_stamps_matching_bare(statuses, updated_since))
    }

    /// Which of the given ids carry a tag.
    pub fn task_ids_with_tag(
        &self,
        tag: &str,
        task_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError> {
        self.in_boundary(|| TaskV2Store::task_ids_with_tag_bare(self, tag, task_ids))
    }

    /// Resolve one task through the owner its registry binds it to.
    pub fn registered_task(&self, id: &str) -> Result<RegisteredTaskResolution, OrbitError> {
        self.in_boundary(|| TaskV2Store::registered_task_bare(self, id))
    }

    /// Status history of a dependency target through its registered owner.
    pub fn registered_task_history(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        self.in_boundary(|| TaskV2Store::registered_task_history_bare(self, id))
    }

    /// Workspace partition the registry binds an id to, if any.
    pub fn registered_task_workspace(&self, id: &str) -> Result<Option<String>, OrbitError> {
        self.in_boundary(|| TaskV2Store::registered_task_workspace_bare(self, id))
    }

    /// List tasks carrying all of the given tags.
    pub fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.in_boundary(|| self.list_tasks_by_tags_bare(tags))
    }

    pub fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        self.in_boundary(|| {
            self.list_tasks_filtered_bare(
                status,
                priority,
                parent_id,
                job_run_id,
                external_ref,
                has_external_ref_system,
            )
        })
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>, OrbitError> {
        self.in_boundary(|| self.get_task_bare(id))
    }

    pub fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError> {
        self.in_boundary(|| self.search_tasks_bare(query))
    }

    /// Search tasks by query, filtered to those carrying all tags.
    pub fn search_tasks_filtered(
        &self,
        query: &str,
        tags: &[String],
    ) -> Result<Vec<Task>, OrbitError> {
        self.in_boundary(|| self.search_tasks_filtered_bare(query, tags))
    }

    /// Stream tasks matching a query to `visit` until it returns false.
    pub fn search_tasks_visit(
        &self,
        query: &str,
        tags: &[String],
        statuses: Option<&[TaskStatus]>,
        admit: &dyn Fn(&Task) -> bool,
        visit: &mut dyn FnMut(Task) -> bool,
    ) -> Result<(), OrbitError> {
        self.in_boundary(|| self.search_tasks_visit_bare(query, tags, statuses, admit, visit))
    }

    /// Run `op` while holding a task's write lock.
    pub fn with_task_write_lock(
        &self,
        id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        self.with_task_lock(id, op)
    }

    /// Run an operator comment-only update under a task's write lock.
    pub fn with_task_comment_write_lock(
        &self,
        id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        self.with_task_comment_lock(id, |_| op())
    }

    /// Status counts per complexity bucket from the task index.
    pub fn task_completion_by_complexity(
        &self,
    ) -> Result<Vec<crate::contracts::TaskCompletionByComplexity>, OrbitError> {
        self.in_boundary(|| self.task_completion_by_complexity_bare())
    }

    /// Complexity bucket per task id from the index.
    pub fn task_complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        self.in_boundary(|| self.task_complexity_by_id_bare())
    }

    /// Terminal tasks whose index `updated_at` is at or after `since`.
    pub fn task_terminal_transitions(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<crate::contracts::TaskTerminalTransition>, OrbitError> {
        self.in_boundary(|| self.task_terminal_transitions_bare(since))
    }

    /// `bug` tasks whose index `created_at` is at or after `since`.
    pub fn task_defect_filings(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<crate::contracts::TaskDefectFiling>, OrbitError> {
        self.in_boundary(|| self.task_defect_filings_bare(since))
    }

    /// Internal lifecycle seam; unavailable backends fail closed.
    pub fn mutate_execution_claim(
        &self,
        context: Option<&crate::contracts::ClaimInvocation>,
        mutation_id: &str,
        mutation: &crate::contracts::ClaimMutation,
    ) -> Result<crate::contracts::ClaimMutationResult, OrbitError> {
        self.mutate_execution_claim_outcome(context, mutation_id, mutation)
            .map(|outcome| outcome.result)
    }
}
