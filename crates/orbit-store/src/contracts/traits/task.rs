use crate::contracts::params::*;
use crate::contracts::{TaskCompletionByComplexity, TaskDefectFiling, TaskTerminalTransition};
use orbit_common::OrbitError;
use orbit_types::identity::OrbitId;
use orbit_types::task::{
    ExternalRef, Task, TaskEnvelopeV2, TaskHistoryEntry, TaskPriority, TaskStatus,
    normalize_task_tags, task_matches_tags,
};
use std::collections::{BTreeMap, BTreeSet};

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

    /// Every handoff this owner accepted. A store without a claim journal has
    /// accepted none, which is an answer rather than a failure: delivery
    /// attribution reads it on every workspace.
    fn accepted_handoffs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::AcceptedHandoff>, OrbitError> {
        Ok(Vec::new())
    }

    /// [ORB-14603] The candidate the task's latest claim settlement kept, as
    /// offered to a run of it on `machine_id`, or why that run implements
    /// afresh. A store without a claim journal kept none.
    fn kept_claim_candidate(
        &self,
        _task_id: &str,
        _machine_id: &str,
    ) -> Result<Option<crate::contracts::KeptClaimCandidate>, OrbitError> {
        Ok(None)
    }

    /// The owner's landing attempts, one per handoff. Read-only inspection.
    fn landing_attempts(
        &self,
    ) -> Result<Vec<orbit_types::workflow::handoff::LandingAttempt>, OrbitError> {
        Err(OrbitError::Store("landing attempts unavailable".into()))
    }

    /// Durable operator retry receipt, if this backend supports owner landing.
    fn landing_retry_receipt(
        &self,
        _handoff_id: &str,
        _request_key: &str,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingRetryReceipt>, OrbitError> {
        Err(OrbitError::Store(
            "landing retry receipts unavailable".into(),
        ))
    }

    /// Current or archived attempt, preserving failure evidence after a retry.
    fn landing_attempt_record(
        &self,
        _handoff_id: &str,
        _attempt: u32,
    ) -> Result<Option<orbit_types::workflow::handoff::LandingAttempt>, OrbitError> {
        Err(OrbitError::Store(
            "landing attempt history unavailable".into(),
        ))
    }

    /// Internal lifecycle seam; unavailable backends fail closed.
    fn mutate_execution_claim(
        &self,
        _context: Option<&crate::contracts::ClaimInvocation>,
        _mutation_id: &str,
        _mutation: &crate::contracts::ClaimMutation,
    ) -> Result<crate::contracts::ClaimMutationResult, OrbitError> {
        Err(OrbitError::Store("claim lifecycle unavailable".into()))
    }
    /// Refuse a worker invocation whose claim no longer carries the authority
    /// its next claim update would need. Unavailable backends fail closed.
    fn verify_worker_claim(
        &self,
        _context: &crate::contracts::ClaimInvocation,
    ) -> Result<(), OrbitError> {
        Err(OrbitError::Store("claim lifecycle unavailable".into()))
    }
    /// A claimed leaf's mid-run steering records, refused as `stale_claim`
    /// unless its claim is still running. Unavailable backends fail closed.
    fn worker_claim_steering(
        &self,
        _context: &crate::contracts::ClaimInvocation,
    ) -> Result<crate::contracts::ClaimSteeringReading, OrbitError> {
        Err(OrbitError::Store("claim lifecycle unavailable".into()))
    }
    /// The mid-run steering watermark the task's most recent claim recorded.
    /// A store without a claim journal recorded none.
    fn task_claim_steering(
        &self,
        _task_id: &str,
    ) -> Result<Option<orbit_types::workflow::SteeringWatermark>, OrbitError> {
        Ok(None)
    }
    fn inspect_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        Err(OrbitError::Store("claim inspection unavailable".into()))
    }
    /// Repairing claim read: settles an interrupted commit before reading.
    /// Unavailable backends fail closed.
    fn resolve_execution_claims(
        &self,
    ) -> Result<Vec<crate::contracts::ClaimInspection>, OrbitError> {
        Err(OrbitError::Store("claim inspection unavailable".into()))
    }
    /// [ORB-15962] One claim and its lifecycle state, whatever its phase,
    /// read like [`Self::resolve_execution_claims`]. A store without a claim
    /// journal holds no claim.
    fn execution_claim_state(
        &self,
        _claim_id: &str,
    ) -> Result<Option<crate::contracts::ClaimInspection>, OrbitError> {
        Ok(None)
    }
    /// Retain one task-branch CI receipt, or queue it for a current protecting
    /// claim. The claim check and decision share the commit boundary.
    fn record_deferred_branch_observation(
        &self,
        _observation: &crate::contracts::DeferredBranchObservation,
    ) -> Result<crate::contracts::BranchObservationOutcome, OrbitError> {
        Err(OrbitError::Store(
            "deferred branch observations require coordination".into(),
        ))
    }
    /// Admit an owner-local drain wave through the pull admission section
    /// [ORB-15514]: a candidate a live execution claim holds, or whose
    /// footprint overlaps one, is refused, and every other candidate's
    /// footprint becomes visible to pull grants before the section ends. A
    /// store without a claim journal serves no pull grant, so it admits all.
    fn admit_owner_wave(
        &self,
        candidates: &[crate::contracts::OwnerWaveCandidate],
    ) -> Result<Vec<crate::contracts::OwnerWaveAdmission>, OrbitError> {
        Ok(candidates
            .iter()
            .map(|candidate| crate::contracts::OwnerWaveAdmission::admitted(&candidate.task_id))
            .collect())
    }
    /// Read-only receipt reconciliation. Creates no receipt, binds no run, and
    /// grants no execution authority; the caller authorizes the identity.
    fn lookup_admission(
        &self,
        _identity: &crate::contracts::AdmissionIdentity,
        _request_id: &str,
    ) -> Result<crate::contracts::AdmissionLookup, OrbitError> {
        Err(OrbitError::Store("admission lookup unavailable".into()))
    }

    /// Run `op` as one read section of this store: every read it makes
    /// through this backend on this thread shares one acquisition of the
    /// partition's commit boundary instead of taking it for each call. `op`
    /// must not wait on another thread's task reads or request an admission;
    /// a backend without a boundary runs it directly.
    fn in_read_section(
        &self,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        op()
    }

    /// Select metadata before hydration, retaining all-envelope index validation.
    fn task_candidates(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<crate::contracts::TaskCandidates, OrbitError>;
    /// Observe settled metadata without coordination, recovery, index repair or
    /// persisted freshness proofs. This is an informational snapshot, never
    /// an admission decision; concurrent publication may change its population.
    fn observe_task_candidates(
        &self,
        _filter: &crate::contracts::TaskListFilter,
        _limit: usize,
    ) -> Result<crate::contracts::TaskCandidates, OrbitError> {
        Err(OrbitError::Store(
            "observational task candidates unavailable".into(),
        ))
    }
    /// Select a fully indexed page from the generated index alone, reading no
    /// envelope; `None` when the filter or the index cannot answer that way.
    fn task_candidate_keys(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<Option<crate::contracts::TaskCandidateKeys>, OrbitError>;
    fn query_task_rows(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
        residual: crate::contracts::TaskResidualFilter<'_>,
    ) -> Result<crate::contracts::TaskPage, OrbitError>;
    /// Direct reads remain strict; list reads tolerate concurrent creation/deletion.
    fn get_task_row(
        &self,
        id: &str,
        list_read: bool,
    ) -> Result<Option<crate::contracts::TaskRow>, OrbitError>;
    /// Envelopes of the listed `ids`, one keyed read per id: no index
    /// validation and no body documents. An id with no settled bundle is absent.
    fn task_envelopes_for_ids(
        &self,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<TaskEnvelopeV2>, OrbitError>;
    fn create_task(&self, params: TaskCreateParams) -> Result<Task, OrbitError>;
    /// Durable key admission for automation, sharing ordinary bundle creation.
    /// Returns `(task, replayed)`: true only when a readable bundle already
    /// existed. A reserved key whose bundle is first published returns false.
    fn create_task_idempotent(
        &self,
        _params: TaskCreateParams,
        _key: &str,
    ) -> Result<(Task, bool), OrbitError> {
        Err(OrbitError::Store(
            "idempotent task creation unavailable".into(),
        ))
    }

    /// Read the published task for an automation action key in this workspace.
    /// A reservation without a readable bundle is not proof of a minted task.
    fn automation_task_for_key(&self, _key: &str) -> Result<Option<Task>, OrbitError> {
        Err(OrbitError::Store(
            "automation task lookup unavailable".into(),
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
    /// Which of `task_ids` carry `tag`, in any workspace on the host. Backends
    /// with a host-wide registry override this; the default answers from
    /// [`Self::list_tasks`].
    fn task_ids_with_tag(
        &self,
        tag: &str,
        task_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .filter(|task| {
                task_ids.contains(task.id.as_str()) && task.tags.iter().any(|t| t == tag)
            })
            .map(|task| task.id.to_string())
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
    /// Status history of a dependency target, read through its registered
    /// owner with the same authority as [`Self::registered_task`].
    ///
    /// Dependency satisfaction consults it only for archived targets, to tell
    /// a task archived after it reached `done` from abandoned work. `None`
    /// means no readable history, which keeps the archived edge a dead end;
    /// that is the default for a backend without task history.
    fn registered_task_history(
        &self,
        _id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        Ok(None)
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
    /// Stream the tasks matching `query` (all of `tags`, and one of
    /// `statuses` when given) in listing order to `visit` until it returns
    /// `false`.
    ///
    /// `admit` judges each candidate from its envelope alone, before its body
    /// is read; a backend that can avoid the read for a rejected task does,
    /// and one with a status index selects `statuses` without reading the
    /// envelopes of the tasks it leaves out. The result equals
    /// [`Self::search_tasks_filtered`] followed by the status filter and
    /// `admit`.
    fn search_tasks_visit(
        &self,
        query: &str,
        tags: &[String],
        statuses: Option<&[TaskStatus]>,
        admit: &dyn Fn(&Task) -> bool,
        visit: &mut dyn FnMut(Task) -> bool,
    ) -> Result<(), OrbitError> {
        for task in self.search_tasks_filtered(query, tags)? {
            if statuses.is_none_or(|statuses| statuses.contains(&task.status))
                && admit(&task)
                && !visit(task)
            {
                break;
            }
        }
        Ok(())
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

    /// [`Self::with_task_write_lock`] for an operator's comment-only update
    /// [ORB-16723]. The lock does not refuse an execution claim protecting
    /// the task; each write inside still checks it, so under a claim only a
    /// history write flagged `operator_comment` whose sole effect is one
    /// comment lands, and every other write is refused as usual.
    fn with_task_comment_write_lock(
        &self,
        _id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        op()
    }

    /// Read a stable dependency snapshot while another task is mutated.
    /// This grants no write authority over the dependency, including a task
    /// protected by an execution claim; every mutation retains its own guard.
    /// The lock is shared: it excludes writers of the task, not other readers.
    /// Never write the task inside it — a nested write lock does not upgrade
    /// it.
    fn with_task_read_lock(
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

    /// Payload-bound task creation using the existing permanent allocation action key.
    fn create_desktop_task(
        &self,
        _params: TaskCreateParams,
        _key: &str,
        _digest: &str,
    ) -> Result<(Task, bool), OrbitError> {
        Err(OrbitError::Store(
            "guarded desktop creation unsupported by backend".into(),
        ))
    }
    /// Reconcile an accepted creation; a reserved but unpublished bundle returns None.
    fn lookup_desktop_creation(
        &self,
        _key: &str,
        _digest: &str,
    ) -> Result<Option<Task>, OrbitError> {
        Err(OrbitError::Store(
            "guarded desktop creation unsupported by backend".into(),
        ))
    }
    /// Recover only the identity of a committed creation, without materializing
    /// mutable sidecars or artifact payloads. A bare action-key reservation is
    /// not a receipt; an inconsistent registered identity fails closed.
    fn lookup_committed_creation_id(
        &self,
        _key: &str,
        _digest: &str,
    ) -> Result<Option<String>, OrbitError> {
        Err(OrbitError::Store(
            "committed creation lookup unsupported by backend".into(),
        ))
    }
    /// Coherently read a task without claiming write authority or creating a write lock.
    fn read_desktop_task(&self, _id: &str) -> Result<DesktopTaskRead, OrbitError> {
        Err(OrbitError::Store(
            "coherent desktop reads unsupported by backend".into(),
        ))
    }
    /// Content revision including documents, history, comments and artifact manifest.
    fn desktop_task_revision(&self, _id: &str) -> Result<String, OrbitError> {
        Err(OrbitError::Store(
            "guarded desktop writes unsupported by backend".into(),
        ))
    }
    /// One locked bundle commit for edits, comments and review verdict/status.
    fn apply_desktop_task_mutation(
        &self,
        _id: &str,
        _params: &DesktopTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError> {
        Err(OrbitError::Store(
            "guarded desktop writes unsupported by backend".into(),
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

    /// Terminal tasks whose index `updated_at` is at or after `since`, oldest
    /// first. Default is empty; the v2 store answers from the SQLite index.
    fn task_terminal_transitions(
        &self,
        _since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TaskTerminalTransition>, OrbitError> {
        Ok(Vec::new())
    }

    /// `bug` tasks whose index `created_at` is at or after `since`, oldest
    /// first. Default is empty; the v2 store answers from the SQLite index.
    fn task_defect_filings(
        &self,
        _since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TaskDefectFiling>, OrbitError> {
        Ok(Vec::new())
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
