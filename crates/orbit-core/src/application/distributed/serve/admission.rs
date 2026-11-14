use std::cell::RefCell;
use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionOrdering, AdmissionRequest, AdmissionTaskGuidance,
    ExecutorCapability, JobRunQuery,
};
use orbit_types::task::{NO_DRAIN_TAG, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::tool::{
    CLAIMED_MODE_DENIED_TOOLS, is_claimed_leaf_reachable_tool, is_claimed_owner_tool,
};
use orbit_types::workflow::JobRunState;

use crate::OrbitRuntime;
use crate::application::distributed::{
    DistributedAdmissionPort, DistributedExperimentPort, owner_binary_version,
};
use crate::application::host_labels;
use crate::application::task::{PilotAdmissionHold, standing_baseline_hold_from_history};
use crate::runtime::run_input::recorded_task_ids;

/// The pull hold for a task assigned to a human crew: operator work no
/// follower or owner pull claims, matching the local `human_crew` exclusion.
/// The pull hold for a task tagged `no-drain`: no follower claims it.
fn no_drain_pull_hold(task: &Task) -> Option<String> {
    task.tags.iter().any(|tag| tag == NO_DRAIN_TAG).then(|| {
        format!("{NO_DRAIN_TAG}: tagged `{NO_DRAIN_TAG}`; no automatic drain or follower admits it")
    })
}

fn human_crew_pull_hold(runtime: &crate::OrbitRuntime, task: &Task) -> Option<String> {
    let human = runtime.human_crew(task.crew.as_deref())?;
    Some(format!(
        "human_crew: assigned to human crew `{}`; operator work no drain claims",
        human.name
    ))
}

/// A grant pins execution to the owner, intersected with its required labels.
/// Ungranted tasks keep the claimed implementer's existing restrictions.
fn claimed_tool_hold(runtime: &crate::OrbitRuntime, task: &Task) -> Option<String> {
    if let Some(grant) = host_labels::granted_tools_owner_hold(task) {
        return Some(runtime.task_host_labels_wait(task).unwrap_or(grant));
    }
    claimed_required_tool_hold(task)
}

/// A remote claimed implementer's restrictions, using the same deny set as
/// the engine and the same closed bridge as dispatch. Reachability is
/// [`is_claimed_leaf_reachable_tool`], derived from the claimed-owner list
/// the leaf-side refusal uses: workspace search and listing cross that
/// bridge, while every other owner workspace tool — coordination or not,
/// such as the codebase analytics a leaf's sandboxed owner route refuses
/// as `owner_route_unavailable` — cannot reach a remote leaf, so the task
/// stays in the backlog for the owner.
fn claimed_required_tool_hold(task: &Task) -> Option<String> {
    task.required_tools.iter().find_map(|tool| {
        let restriction = if CLAIMED_MODE_DENIED_TOOLS.contains(&tool.as_str()) {
            "denied in claimed mode; the implementer returns task state through its output"
        } else if is_claimed_leaf_reachable_tool(tool) {
            return None;
        } else if crate::runtime::is_coordination_tool(tool) && !is_claimed_owner_tool(tool) {
            "owner coordination outside the closed claimed-owner bridge"
        } else {
            "owner-only outside the closed claimed-owner bridge"
        };
        Some(format!("required tool `{tool}` is {restriction}"))
    })
}

impl OrbitRuntime {
    /// Explicit owner maintenance used by `doctor admission-receipts --compact`.
    /// Retains full receipts for unsettled claims and permanent tombstones for
    /// compacted requests; claim-mutation pruning remains independent.
    pub fn compact_admission_receipts(
        &self,
    ) -> Result<orbit_store::contracts::AdmissionReceiptMaintenance, OrbitError> {
        self.ensure_distributed_owner_workspace()?;
        self.admission_boundary()?.compact_admission_receipts()
    }

    /// One admission on this owner's commit boundary. Shared by the routed
    /// tool above and the owner-local drain adapter, so both reach the same
    /// transaction with the same owner version and repository roots.
    pub(crate) fn admit_pull_request(
        &self,
        boundary: &TaskCommitBoundary,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
    ) -> Result<AdmissionLookup, OrbitError> {
        // Do this before the admission section: reconciliation writes task
        // declarations, which that section deliberately fences.
        self.reconcile_pull_recovery_context()?;
        let mut admission_holds = self
            .live_local_delivery_runs()?
            .into_iter()
            .map(|(task, run)| {
                (
                    task,
                    format!("live local delivery run {run} is carrying it"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (task, runs) in
            crate::application::automation::preparation::active_task_pilot_preparations(self)?
        {
            admission_holds.entry(task).or_insert_with(|| {
                format!(
                    "active task-pilot preparation holds it: {}",
                    runs.into_iter().collect::<Vec<_>>().join(", ")
                )
            });
        }
        // Backlog holds are read before the admission section, which stalls
        // every task write on the host [ORB-14724]. The section re-checks
        // pilot holds only for the candidate it is about to claim, from the
        // bundle it read, so one applied after this read is still honoured.
        let backlog =
            self.list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)?;
        // A native-host finding holds only an executor whose OS or machine
        // cannot produce the evidence, so it is judged against the requesting
        // executor.
        let machine_id = Some(identity.location().machine_id.as_str());
        let now = chrono::Utc::now();
        // Each backlog task's bundle is read at most once in this pre-pass:
        // the lazy history loader below memoizes per task and the map it
        // fills feeds the provider and baseline holds after the loop.
        let mut history_by_task: BTreeMap<String, Vec<TaskHistoryEntry>> = BTreeMap::new();
        for task in &backlog {
            if let Some(hold) = no_drain_pull_hold(task) {
                admission_holds.insert(task.id.clone(), hold);
                continue;
            }
            // Operator work: no follower claims a human crew's task.
            if let Some(hold) = human_crew_pull_hold(self, task) {
                admission_holds.insert(task.id.clone(), hold);
                continue;
            }
            let history_cache: RefCell<Option<Vec<TaskHistoryEntry>>> = RefCell::new(None);
            let load_history = || -> Result<Vec<TaskHistoryEntry>, OrbitError> {
                if let Some(cached) = history_cache.borrow().as_ref() {
                    return Ok(cached.clone());
                }
                let history = self.get_task_history(&task.id)?;
                *history_cache.borrow_mut() = Some(history.clone());
                Ok(history)
            };
            // A task scheduled ahead is claimed by no follower until its
            // time; the first request after it may take it.
            if let Some(hold) =
                crate::application::task::schedule_hold_in(task, now, &load_history)?
            {
                admission_holds.insert(task.id.clone(), hold.detail());
                if let Some(cached) = history_cache.borrow().as_ref() {
                    history_by_task.insert(task.id.clone(), cached.clone());
                }
                continue;
            }
            // A task an auto-task job step minted is claimed only while its
            // step still admits it (ORB-20158).
            if let Some(hold) = crate::application::auto_tasks::job_step::step_bound_admission_hold(
                self,
                task,
                &load_history,
            ) {
                admission_holds.insert(task.id.clone(), hold);
                if let Some(cached) = history_cache.borrow().as_ref() {
                    history_by_task.insert(task.id.clone(), cached.clone());
                }
                continue;
            }
            let comments = self.get_task_comments(&task.id)?;
            match self.pilot_admission_hold_in(task, &comments, &load_history)? {
                Some(PilotAdmissionHold::StaleAssessment(detail)) => {
                    admission_holds.insert(task.id.clone(), detail);
                }
                Some(PilotAdmissionHold::HostOperational(hold)) => {
                    self.record_host_operational_hold(&task.id, &hold)?;
                    admission_holds.insert(task.id.clone(), hold.detail());
                }
                Some(PilotAdmissionHold::OperatorValidation(hold)) => {
                    self.record_operator_validation_hold(&task.id, &hold)?;
                    admission_holds.insert(task.id.clone(), hold.detail());
                }
                Some(PilotAdmissionHold::NativeOs(hold)) => {
                    if let Some(wait) = hold.wait_on(task, request.os, machine_id) {
                        admission_holds.insert(task.id.clone(), wait);
                    }
                }
                _ => {}
            }
            // Exclude unsatisfiable requirements before candidate bundle reads;
            // the transaction checks newly added tasks again below.
            // Owner-local activity admission keeps its existing authority.
            if identity.is_remote()
                && let Some(why) = claimed_tool_hold(self, task)
            {
                admission_holds.entry(task.id.clone()).or_insert(why);
            }
            // [ORB-15573] A standing provider hold keeps the task from an
            // executor that would run it as an excluded crew: its own, or the
            // executor's default for a task naming none. Another executor, or
            // this owner's own draw, takes it on a crew the hold leaves.
            match load_history() {
                Ok(history) => {
                    if let Some(why) = self.pull_provider_hold(task, request, machine_id, &history)
                    {
                        admission_holds.entry(task.id.clone()).or_insert(why);
                    }
                }
                Err(error) => {
                    tracing::warn!(task_id = %task.id, "could not read provider failure hold: {error}");
                }
            }
            if let Some(cached) = history_cache.borrow().as_ref() {
                history_by_task.insert(task.id.clone(), cached.clone());
            }
        }
        // Match local dispatch's expiry boost and its best-effort fallback:
        // unreadable deadlines affect ordering, never admission eligibility.
        let expiring_tasks = match crate::application::automation::expiring_frozen_batch_tasks(
            self,
            chrono::Utc::now(),
        ) {
            Ok(expiring) => expiring.into_keys().collect(),
            Err(error) => {
                tracing::warn!(
                    "could not read frozen batch deadlines; pull backlog keeps its ordinary order: {error}"
                );
                Default::default()
            }
        };
        // The owner's own repair pull takes a stopped landing's repair and
        // nothing from the backlog, which its local drain admits through the
        // owner wave [ORB-17303].
        let repairs_only = self.is_owner_repair_pull(identity, request)?;
        // [ORB-17304] The backlog is ordered against the live executor
        // census, the requester declaring itself by this request.
        let census = (!repairs_only)
            .then(|| {
                self.pull_requester_census(
                    &identity.location().machine_id,
                    ExecutorCapability::of_request(request),
                )
            })
            .flatten();
        #[cfg(feature = "test-support")]
        crate::application::distributed::admission_checkpoint::notify();
        boundary.admit_task_with_crew_guidance(
            identity,
            request,
            &AdmissionOrdering {
                affinity: orbit_types::task::ADMISSION_AFFINITY,
                census,
                expiring_tasks,
                gates: Some(self.owner_validation_gates()),
                tag_instructions: Some(self.captured_tag_instructions(&request.ship)),
                executor_host_os: self.executor_host_os_limits()?,
                repairs_only,
                crew_pools: self.dispatch_crew_pools(),
            },
            owner_binary_version(),
            &self.paths().repo_root,
            &self.data_root(),
            &admission_holds,
            &self.baseline_held_tasks(&backlog, &history_by_task),
            &|task, comments, history| {
                if let Some(hold) = no_drain_pull_hold(task) {
                    return Ok(Some(hold));
                }
                // Re-read under the section: the crew may have been changed
                // to a human crew since the backlog read.
                if let Some(hold) = human_crew_pull_hold(self, task) {
                    return Ok(Some(hold));
                }
                // Re-read under the section: a `not_before` set after the
                // backlog read above still holds the candidate.
                if let Some(hold) =
                    crate::application::task::schedule_hold_in(task, chrono::Utc::now(), &|| {
                        Ok(history.to_vec())
                    })?
                {
                    return Ok(Some(hold.detail()));
                }
                // Re-read under the section: the step may have expired or
                // its definition held since the backlog read.
                if let Some(hold) =
                    crate::application::auto_tasks::job_step::step_bound_admission_hold(
                        self,
                        task,
                        &|| Ok(history.to_vec()),
                    )
                {
                    return Ok(Some(hold));
                }
                if identity.is_remote()
                    && let Some(why) = claimed_tool_hold(self, task)
                {
                    return Ok(Some(why));
                }
                match self.pilot_admission_hold_in(task, comments, &|| Ok(history.to_vec()))? {
                    Some(PilotAdmissionHold::StaleAssessment(detail)) => Ok(Some(detail)),
                    Some(PilotAdmissionHold::HostOperational(hold)) => Ok(Some(hold.detail())),
                    Some(PilotAdmissionHold::OperatorValidation(hold)) => Ok(Some(hold.detail())),
                    Some(PilotAdmissionHold::NativeOs(hold)) => {
                        Ok(hold.wait_on(task, request.os, machine_id))
                    }
                    _ => Ok(None),
                }
            },
            &|task| self.fresh_claim_crew_task(task, request),
            &|task| {
                Ok(AdmissionTaskGuidance {
                    experiments: self.capture_pull_experiments(task, &request.ship)?,
                    artifacts: Vec::new(),
                })
            },
        )
    }

    /// Why a standing provider hold keeps `task` from the executor `request`
    /// comes from, or `None` when the crew it would run as is not excluded.
    /// [ORB-16379] The hold is read as the executor's `machine` sees it: an
    /// unavailable hold placed on another host excludes there only the crews
    /// it carries from an earlier standing hold.
    /// From history the caller already read, so the pre-pass does not
    /// read the task's bundle again for this hold.
    fn pull_provider_hold(
        &self,
        task: &Task,
        request: &AdmissionRequest,
        machine: Option<&str>,
        history: &[TaskHistoryEntry],
    ) -> Option<String> {
        let hold = self.provider_hold_on_from_history(task, history, machine)?;
        Self::pull_provider_hold_detail(task, request, hold)
    }

    fn pull_provider_hold_detail(
        task: &Task,
        request: &AdmissionRequest,
        hold: orbit_types::workflow::ProviderFailureHold,
    ) -> Option<String> {
        let crew = task
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|crew| !crew.is_empty())
            .or_else(|| request.crews.as_ref()?.default_crew.as_deref())?;
        hold.excludes(crew).then(|| {
            format!(
                "provider hold: run {} ended with {} on {}, so crews {} are excluded until {}; \
                 this executor would run it as `{crew}`",
                hold.run_id,
                hold.class.as_str(),
                hold.provider.as_deref().unwrap_or("its provider"),
                hold.excluded_crews.join(", "),
                hold.not_before.to_rfc3339()
            )
        })
    }

    /// Each `backlog` task a red base still holds, mapped to why
    /// [ORB-14258]. Only the clock tick's recorded verdicts are read, so no
    /// Git or validation command runs on pull admission [ORB-14739]; a hold
    /// that lifts a moment late only defers the task to the next request.
    /// Reuses histories the pre-pass already read; a task absent from
    /// `histories` is read here, once.
    fn baseline_held_tasks(
        &self,
        backlog: &[Task],
        histories: &BTreeMap<String, Vec<TaskHistoryEntry>>,
    ) -> BTreeMap<String, String> {
        let mut held = BTreeMap::new();
        for task in backlog {
            match histories.get(&task.id).map_or_else(
                || self.standing_baseline_hold(task),
                |history| Ok(standing_baseline_hold_from_history(task, history)),
            ) {
                Ok(Some(why)) => {
                    held.insert(task.id.clone(), why);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(task_id = %task.id, "could not read baseline red hold: {error}");
                }
            }
        }
        held
    }

    /// Each task a live run on this owner holds the delivery slot of, mapped
    /// to that run [ORB-13918].
    ///
    /// A local drain admits a task by dispatching a wrapper and gate that
    /// carry it in `input.task_ids`; the task stays `backlog`, and reserves
    /// nothing, until the gate gets its context locks. Those runs are the only
    /// record of the admission, so pull admission reads them — the same
    /// `spec.task_delivery` holders `orbit run ship` refuses a duplicate
    /// against — rather than handing the task to a follower as well.
    fn live_local_delivery_runs(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        let delivery_jobs = self.task_delivery_job_ids()?;
        let jobs = self.stores().jobs();
        let mut runs = jobs.list_job_runs_filtered(&JobRunQuery {
            active_only: true,
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        // `active_only` intentionally means pending/running across the store
        // API. Retrying is also a live run state, so include it separately
        // while the runner sleeps between attempts.
        runs.extend(jobs.list_job_runs_filtered(&JobRunQuery {
            state: Some(JobRunState::Retrying),
            include_steps: false,
            ..JobRunQuery::default()
        })?);
        let mut carried = BTreeMap::new();
        for run in runs
            .into_iter()
            .filter(|run| delivery_jobs.contains(&run.job_id))
        {
            let task_ids = run
                .input
                .as_ref()
                .map(recorded_task_ids)
                .unwrap_or_default();
            for task_id in task_ids {
                carried
                    .entry(task_id.to_string())
                    .or_insert_with(|| run.run_id.clone());
            }
        }
        Ok(carried)
    }
}
