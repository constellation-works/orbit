//! Local pull admissions: capacity allocation, the admission phase machine,
//! leaf creation and the failure breaker's streak.
//!
//! Durable caller admissions share the job database writer transaction.
//! Receipt replay never creates a second leaf, including after the first leaf
//! terminates.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contract::{
    AdmissionRequest, ClaimMutation, ClaimRepair, DrainLeafOccupancy, LocalPullAdmission,
    LocalPullMutation, LocalPullPhase, PullDestination, PullFailureStreak,
};
use orbit_store::{JobRunRepository, LocalPullRepository};
use orbit_types::workflow::{
    self, JobRun, JobRunState, PipelineState, REVIEW_ADMISSION_KEY, ReviewAdmission, ReviewTiming,
    RunIdRole,
};

use super::JobRuns;

/// Terminal `Idle` and `Refused` admissions one workspace keeps. A drain that
/// polls an owner with nothing ready leaves one such row per poll (about
/// 1,440 a day at the shipped 60 second idle sleep), and no reader needs an
/// old one: they hold no claim, and their request ids are random, never
/// reused. This keeps roughly a week of one drain's polls for diagnosis.
/// `Settled` rows are never pruned — the failure breaker and status
/// reporting read them.
const TERMINAL_ROWS_RETAINED: usize = 10_000;

/// How far the dispatch lineage walk follows records. A loop guard for a
/// malformed or cyclic dispatch chain, not a tuning knob.
const MAX_DISPATCH_LINEAGE_DEPTH: usize = 64;

/// Provenance label of a review admission seeded from a claim.
const CLAIM_SOURCE: &str = "claim";

/// The claimed leaf input naming the candidate a repair claim restores.
const CLAIM_REPAIR_KEY: &str = "claim_repair";

/// The loose-leaf wrapper the legacy drain dispatches. It occupies a slot on
/// behalf of the leaf beneath it, so it is only counted while nothing beneath
/// it is.
const LEGACY_WRAPPER_PIPELINE: &str = "task_auto_pipeline";

impl JobRuns {
    /// The local admission permanently bound to leaf run `run_id`,
    /// independent of owner connectivity.
    pub fn local_pull_for_run(
        &self,
        run_id: &str,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        self.store
            .read()?
            .local_pull()
            .get_by_leaf(&self.workspace_id, run_id)
    }

    /// Every local pull admission of the workspace, in admission order.
    pub fn local_pull_admissions(&self) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        self.store.read()?.local_pull().list(&self.workspace_id)
    }

    /// Admissions that still hold a slot — not idle, refused or settled — in
    /// admission order. A settle-only pass reads them from `orbit run cancel`
    /// and `orbit run auto --stop` in workspaces that may never have pulled
    /// [ORB-13663]; any follower process uses this to deliver settlements a
    /// cancelled drain left behind.
    pub fn unsettled_local_pull_admissions(&self) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        self.store
            .read()?
            .local_pull()
            .list_holding(&self.workspace_id)
    }

    /// Settled claims that may still have rebuildable build output, as leaf
    /// run ids in admission order. A leaf already noted clear by
    /// [`Self::note_settled_leaf_build_output_clear`] is omitted, and the
    /// omission survives a restart.
    pub fn settled_leaf_run_ids_pending_build_reclaim(&self) -> Result<Vec<String>, OrbitError> {
        self.store
            .read()?
            .local_pull()
            .list_settled_leaves_pending_reclaim(&self.workspace_id)
    }

    /// Remember that `leaf_run_id`'s settled claim is terminal and has no
    /// reclaimable build output, so a later pass does not read the run
    /// again. No-op when that admission is missing or not settled; the phase
    /// and the settlement are left as they are.
    pub fn note_settled_leaf_build_output_clear(
        &self,
        leaf_run_id: &str,
    ) -> Result<(), OrbitError> {
        self.store.write(|tx| {
            let pulls = tx.local_pull();
            let Some(mut record) = pulls.get_by_leaf(&self.workspace_id, leaf_run_id)? else {
                return Ok(());
            };
            if record.phase != LocalPullPhase::Settled
                || record.leaf_run_id.as_deref() != Some(leaf_run_id)
                || record.build_output_reclaim_complete
            {
                return Ok(());
            }
            record.build_output_reclaim_complete = true;
            pulls.put(&self.workspace_id, &record)
        })
    }

    /// The drain `run_id`'s most recent settled claims against `destination`
    /// that failed in a row: the failure breaker's streak.
    ///
    /// Only claims `run_id` admitted count, newest first, and the walk stops
    /// at the first one that did not fail, so the breaker that reads this on
    /// every pass never decodes older history. A claim closed obsolete
    /// (settled with an owner refusal) says nothing about this executor, so it
    /// neither extends nor resets the streak; nor does a released claim — a
    /// cancel, a provider, a red base, a review hold for owed evidence — or a
    /// failure that does not trip the breaker, such as Orbit's refusal of the
    /// implementer's handoff [ORB-15639].
    pub fn consecutive_failed_local_pull_settlements(
        &self,
        destination: &PullDestination,
        run_id: &str,
    ) -> Result<PullFailureStreak, OrbitError> {
        let mut streak = PullFailureStreak::default();
        self.store.read()?.local_pull().scan_settled_claims(
            &self.workspace_id,
            destination,
            run_id,
            |record| {
                // SQL only narrows; the record stays the authority.
                if record.destination != *destination
                    || record.request.run_context.run_id != run_id
                    || record.phase != LocalPullPhase::Settled
                    || record.refusal.is_some()
                {
                    return true;
                }
                match record.settlement {
                    Some(ClaimMutation::Fail(evidence)) => match evidence.failure {
                        Some(failure)
                            if orbit_types::workflow::is_contract_refusal(&failure.reason) =>
                        {
                            true
                        }
                        Some(failure) => {
                            streak.classes.push(failure.class.as_str().to_string());
                            true
                        }
                        None => {
                            streak.classes.push("untyped".to_string());
                            true
                        }
                    },
                    // A released claim says nothing about this executor.
                    Some(ClaimMutation::Release(_)) => true,
                    _ => false,
                }
            },
        )?;
        Ok(streak)
    }

    /// Every admission the drain `run_id` made that holds or held a claim,
    /// in admission order, whatever its phase, so a pull drain can derive
    /// its window's crew exclusions every pass [ORB-13941].
    pub fn local_pull_claims_admitted_by(
        &self,
        run_id: &str,
    ) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        self.store
            .read()?
            .local_pull()
            .list_claims_admitted_by(&self.workspace_id, run_id)
    }

    /// How much leaf capacity both admission paths are already using
    /// [ORB-12617]. Read-only: asking how full a drain is never writes.
    pub fn drain_leaf_occupancy(&self) -> Result<DrainLeafOccupancy, OrbitError> {
        let snapshot = self.store.read()?;
        occupancy(
            &snapshot.job_run(),
            &snapshot.local_pull(),
            &self.workspace_id,
            None,
        )
    }

    /// The shared capacity reading, with slots outside coordinator `run_id`'s
    /// lineage counted as inherited: workers of stopped coordinators,
    /// explicit deliveries and unsettled pull admissions.
    pub fn drain_leaf_occupancy_for_run(
        &self,
        run_id: &str,
    ) -> Result<DrainLeafOccupancy, OrbitError> {
        let snapshot = self.store.read()?;
        occupancy(
            &snapshot.job_run(),
            &snapshot.local_pull(),
            &self.workspace_id,
            Some(run_id),
        )
    }

    /// Reserve local capacity and persist the request before transport.
    /// `None` when the drain is finished, has stopped admissions, or is at its
    /// ceiling. A replayed request returns its stored admission.
    pub fn allocate_pull_request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
        ceiling: usize,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        if [
            &destination.owner_machine_id,
            &destination.owner_workspace_id,
            &destination.execution_machine_id,
            &destination.selector,
            &request.request_id,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
        {
            return Err(invalid(
                "pull destination and request identity are required",
            ));
        }
        if request.ship.mode == "local"
            && destination.owner_machine_id != destination.execution_machine_id
        {
            return Err(invalid("followers cannot execute owner-local leaves"));
        }
        if (request.ship.before_pr || request.ship.before_landing)
            && !(request.review_gate && request.ship.mode == "pr")
        {
            return Err(invalid(
                "an owner with review.timing = before_pr or before_landing admits only a PR leaf \
                 that runs the review gate",
            ));
        }
        let workspace = self.workspace_id.as_str();
        self.store.write(|tx| {
            let runs = tx.job_run();
            let pulls = tx.local_pull();
            if let Some(old) = pulls.get(workspace, destination, &request.request_id)? {
                if old.request != *request || old.destination != *destination {
                    return Err(invalid("pull request identity reused with changed input"));
                }
                return Ok(Some(old));
            }
            let parent = runs
                .get(workspace, &request.run_context.run_id)?
                .ok_or_else(|| invalid("pull drain run missing"))?;
            if parent.state.is_terminal() {
                return Ok(None);
            }
            let raw = runs.get_pipeline_state_json(workspace, &parent.run_id)?;
            let state: PipelineState =
                serde_json::from_str(&raw.ok_or_else(|| invalid("pull drain state missing"))?)
                    .map_err(codec_error)?;
            if state.admissions_stopped() {
                return Ok(None);
            }
            let ceiling = state
                .effective_max_active_leaf_runs(u32::try_from(ceiling).unwrap_or(u32::MAX))
                as usize;
            // The drain's worker limit is the only ceiling: the leaf
            // definitions declare no active-run limit of their own
            // [ORB-13893].
            if pull_admission_load(&occupancy(&runs, &pulls, workspace, None)?, ceiling) >= ceiling
            {
                return Ok(None);
            }
            let record = LocalPullAdmission {
                destination: destination.clone(),
                request: request.clone(),
                receipt: None,
                leaf_run_id: None,
                phase: LocalPullPhase::Requested,
                settlement: None,
                refusal: None,
                settlement_refusal: None,
                build_output_reclaim_complete: false,
            };
            pulls.put(workspace, &record)?;
            pulls.delete_terminal_beyond(workspace, TERMINAL_ROWS_RETAINED)?;
            Ok(Some(record))
        })
    }

    /// Advance one admission through its phase machine, creating or closing
    /// its leaf run in the same transaction.
    pub fn mutate_local_pull(
        &self,
        destination: &PullDestination,
        request_id: &str,
        mutation: &LocalPullMutation,
    ) -> Result<LocalPullAdmission, OrbitError> {
        let workspace = self.workspace_id.as_str();
        self.store.write(|tx| {
            let runs = tx.job_run();
            let pulls = tx.local_pull();
            let mut record = pulls
                .get(workspace, destination, request_id)?
                .ok_or_else(|| invalid("local pull request missing"))?;
            if record.destination != *destination {
                return Err(invalid("pull destination changed"));
            }
            match mutation {
                LocalPullMutation::Receive(receipt) => {
                    if let Some(old) = &record.receipt {
                        if old != receipt.as_ref() {
                            return Err(invalid("pull receipt changed"));
                        }
                        return Ok(record);
                    }
                    if record.phase != LocalPullPhase::Requested
                        || receipt.request != record.request
                        || receipt.machine_id != destination.execution_machine_id
                    {
                        return Err(invalid("pull receipt does not match pending request"));
                    }
                    if let Some(claim) = &receipt.claim {
                        if claim.executed_on.machine_id != destination.execution_machine_id
                            || claim.request_id != request_id
                            || receipt
                                .task
                                .as_ref()
                                .is_none_or(|task| task.id != claim.task_id)
                        {
                            return Err(invalid("pull receipt claim identity mismatch"));
                        }
                        record.phase = LocalPullPhase::Claimed;
                    } else {
                        if receipt.task.is_some() {
                            return Err(invalid("idle receipt contains a task"));
                        }
                        record.phase = LocalPullPhase::Idle;
                    }
                    record.receipt = Some(receipt.as_ref().clone());
                }
                LocalPullMutation::CreateLeaf => {
                    if record.leaf_run_id.is_some() {
                        return Ok(record);
                    }
                    if record.phase != LocalPullPhase::Claimed {
                        return Err(invalid("leaf creation requires a persisted claim"));
                    }
                    let now = Utc::now();
                    let run_id = runs.next_id(RunIdRole::Child, now)?;
                    let resource_class = {
                        let claim = record
                            .receipt
                            .as_ref()
                            .and_then(|r| r.claim.as_ref())
                            .ok_or_else(|| invalid("claim missing"))?;
                        tx.admission_load()
                            .latest_task_reservation(workspace, &claim.task_id)?
                    };
                    let (run, state) = claimed_leaf(&record, run_id.clone(), now, resource_class)?;
                    runs.put(workspace, &run, Some(&state))?;
                    record.leaf_run_id = Some(run_id);
                    record.phase = LocalPullPhase::Created;
                }
                LocalPullMutation::Bound => {
                    advance(&mut record, LocalPullPhase::Created, LocalPullPhase::Bound)?
                }
                LocalPullMutation::LaunchIntent => {
                    if record.phase != LocalPullPhase::Bound {
                        return Err(invalid(
                            "execution launch uncertain; deliberate recovery required",
                        ));
                    }
                    let id = record
                        .leaf_run_id
                        .as_deref()
                        .ok_or_else(|| invalid("bound leaf missing"))?;
                    let run = runs
                        .get(workspace, id)?
                        .ok_or_else(|| invalid("bound leaf disappeared"))?;
                    if run.state != JobRunState::Pending {
                        return Err(invalid("leaf is no longer queued; reconcile before launch"));
                    }
                    record.phase = LocalPullPhase::Launching;
                }
                LocalPullMutation::Launched => {
                    if !matches!(
                        record.phase,
                        LocalPullPhase::Settling | LocalPullPhase::Settled
                    ) {
                        advance(
                            &mut record,
                            LocalPullPhase::Launching,
                            LocalPullPhase::Launched,
                        )?;
                    }
                }
                LocalPullMutation::Settle(settlement) => {
                    if !matches!(
                        settlement.as_ref(),
                        ClaimMutation::Fail(_)
                            | ClaimMutation::Release(_)
                            | ClaimMutation::AcceptHandoff(_)
                    ) {
                        return Err(invalid("invalid leaf settlement"));
                    }
                    if let Some(old) = &record.settlement {
                        if old != settlement.as_ref() {
                            return Err(invalid("pending settlement is immutable"));
                        }
                        return Ok(record);
                    }
                    if matches!(
                        record.phase,
                        LocalPullPhase::Requested
                            | LocalPullPhase::Idle
                            | LocalPullPhase::Settled
                            | LocalPullPhase::Refused
                    ) {
                        return Err(invalid("settlement requires a claim"));
                    }
                    record.settlement = Some(settlement.as_ref().clone());
                    record.phase = LocalPullPhase::Settling;
                }
                LocalPullMutation::Settled | LocalPullMutation::SettleObsolete(_) => {
                    if let LocalPullMutation::SettleObsolete(reason) = mutation {
                        if record.phase == LocalPullPhase::Settled {
                            return Ok(record);
                        }
                        if record.phase != LocalPullPhase::Settling {
                            return Err(invalid(
                                "only a pending settlement can be closed as obsolete",
                            ));
                        }
                        record.refusal = Some(reason.clone());
                    }
                    advance(
                        &mut record,
                        LocalPullPhase::Settling,
                        LocalPullPhase::Settled,
                    )?;
                    // A refusal the owner answered earlier no longer stands.
                    record.settlement_refusal = None;
                    // A settled claim closes a leaf that never started. A leaf
                    // that is running is left alone: it finishes, or a forced
                    // cancel stops its process first; failing it here would
                    // orphan a live worker.
                    let closed = match record.settlement {
                        Some(ClaimMutation::Fail(_)) => Some(JobRunState::Failed),
                        Some(ClaimMutation::Release(_)) => Some(JobRunState::Cancelled),
                        _ => None,
                    };
                    if let Some(closed) = closed
                        && let Some(id) = &record.leaf_run_id
                    {
                        let mut run = runs
                            .get(workspace, id)?
                            .ok_or_else(|| invalid("bound leaf disappeared before settlement"))?;
                        if run.state == JobRunState::Pending {
                            run.state = closed;
                            run.finished_at = Some(Utc::now());
                            runs.put(workspace, &run, None)?;
                        }
                    }
                }
                LocalPullMutation::DeferSettlement(refusal) => {
                    if record.phase != LocalPullPhase::Settling {
                        return Err(invalid("only a pending settlement can be deferred"));
                    }
                    record.settlement_refusal = Some(refusal.clone());
                }
                LocalPullMutation::Refuse(reason) => {
                    if record.phase == LocalPullPhase::Refused {
                        return Ok(record);
                    }
                    if record.phase != LocalPullPhase::Requested || record.receipt.is_some() {
                        return Err(invalid(
                            "only an unanswered request can be closed as refused",
                        ));
                    }
                    record.phase = LocalPullPhase::Refused;
                    record.refusal = Some(reason.clone());
                }
            }
            pulls.put(workspace, &record)?;
            Ok(record)
        })
    }
}

fn codec_error(error: serde_json::Error) -> OrbitError {
    OrbitError::Store(error.to_string())
}

fn invalid(message: &str) -> OrbitError {
    OrbitError::JobValidation(message.into())
}

/// The claimed leaf run and its initial state for `record`'s persisted claim.
///
/// The claimed task lives in the owner's store, not this one, so the leaf
/// carries the owner's snapshot of it. Crew resolution
/// (`orbit-core` `task_crew_from_run_input`) reads `claimed_task` in place of
/// a local task lookup, so the leaf runs on the owner task's crew rather than
/// this host's `default_crew`.
fn claimed_leaf(
    record: &LocalPullAdmission,
    run_id: String,
    now: chrono::DateTime<Utc>,
    resource_class: Option<(String, Option<String>, String)>,
) -> Result<(JobRun, PipelineState), OrbitError> {
    let claim = record
        .receipt
        .as_ref()
        .and_then(|r| r.claim.as_ref())
        .ok_or_else(|| invalid("claim missing"))?;
    let job = pipeline(&record.request)?;
    let task = record
        .receipt
        .as_ref()
        .and_then(|r| r.task.as_ref())
        .filter(|task| task.id == claim.task_id)
        .ok_or_else(|| invalid("claimed task snapshot missing"))?;
    // [ORB-17773] `crew_source` and `complexity` ride the snapshot so the
    // follower can re-draw an automatic assignment under its own usage
    // pressure.
    let mut input = serde_json::json!({
        "task_ids": [claim.task_id],
        "base_branch": record.request.ship.base_branch,
        "base_sync": if job == workflow::TASK_CLAIMED_LOCAL_PIPELINE_JOB {"local"} else {"remote"},
        "claimed_task": {
            "id": task.id,
            "crew": task.crew,
            "crew_source": task.crew_source,
            "complexity": task.complexity,
        },
    });
    input["experiment_capture"] = serde_json::to_value(
        record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.experiments.as_ref()),
    )
    .map_err(|error| OrbitError::Store(format!("serialize experiment capture: {error}")))?;
    // Carry the profile key already chosen by admission into the queued
    // leaf. The host forecast then exchanges its reservation for a run with
    // the same price before the worker starts.
    if let Some((work_class, complexity, admitted_at)) = resource_class {
        input["resource_class"] = serde_json::json!({
            "task_id": claim.task_id, "work_class": work_class,
            "complexity": complexity, "admitted_at": admitted_at,
        });
    }
    // The leaf's review admission is the claim's captured owner decision,
    // including an explicit owner-off decision, never this host's settings
    // [ORB-13908].
    let review = claim_review_admission(record, now)?;
    if let Some(object) = input.as_object_mut() {
        object.insert(
            REVIEW_ADMISSION_KEY.into(),
            serde_json::to_value(review).map_err(codec_error)?,
        );
    }
    // [ORB-14257] The candidate an earlier claim preserved, for the leaf's
    // `resume_candidate` step to carry onto this base; a claimed-local leaf
    // continues one too [ORB-14338].
    if let (Some(candidate), Some(object)) = (&task.resume_candidate, input.as_object_mut()) {
        object.insert(
            "resume_candidate".into(),
            serde_json::to_value(candidate).map_err(codec_error)?,
        );
    }
    // A repair claim's leaf restores the candidate its stopped landing
    // preserved rather than implementing afresh [ORB-14261].
    if let (Some(repair), Some(object)) = (&claim.repair, input.as_object_mut()) {
        object.insert(
            CLAIM_REPAIR_KEY.into(),
            claim_repair_input(repair, &record.request.ship),
        );
    }
    let run = JobRun {
        build: Some(orbit_common::process::build::current_orbit_build()),
        run_id: run_id.clone(),
        job_id: job.into(),
        attempt: 1,
        state: JobRunState::Pending,
        scheduled_at: now,
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: Some(input.clone()),
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: vec![],
        executed_on: Some(claim.executed_on.clone()),
    };
    let state = PipelineState::new(run_id, job.into(), input);
    Ok((run, state))
}

fn advance(
    record: &mut LocalPullAdmission,
    from: LocalPullPhase,
    to: LocalPullPhase,
) -> Result<(), OrbitError> {
    if record.phase == to {
        return Ok(());
    }
    if record.phase != from {
        return Err(invalid("invalid local pull phase transition"));
    }
    record.phase = to;
    Ok(())
}

/// Every run a wrapper or coordinator dispatched, transitively. Terminal
/// dispatch records still identify live descendants.
fn dispatch_lineage<A>(
    runs: &JobRunRepository<'_, A>,
    workspace: &str,
    root_state: Option<&String>,
) -> Result<BTreeSet<String>, OrbitError> {
    let mut frontier = vec![root_state.cloned()];
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_DISPATCH_LINEAGE_DEPTH {
        let mut next = Vec::new();
        for raw in frontier.into_iter().flatten() {
            let state: PipelineState = serde_json::from_str(&raw).map_err(codec_error)?;
            for child in state.child_dispatches {
                if seen.insert(child.child_run_id.clone()) {
                    next.push(runs.get_pipeline_state_json(workspace, &child.child_run_id)?);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(seen)
}

/// The one capacity reading both admission paths allocate against
/// [ORB-12617].
///
/// Legacy wrappers are counted once and replaced by whichever descendant is
/// actually carrying their work: a live leaf run of any of the four leaf
/// definitions, or a pull admission whose bound leaf has gone terminal but has
/// not settled yet. An admission with no live run of its own — never created,
/// or created and since terminal — holds its own slot instead, so a slot is
/// released exactly when the claim settles and not before. Claimed leaves in
/// their before-landing review are counted again, apart, as `reviewing`.
fn occupancy<A>(
    runs: &JobRunRepository<'_, A>,
    pulls: &LocalPullRepository<'_, A>,
    workspace: &str,
    coordinator: Option<&str>,
) -> Result<DrainLeafOccupancy, OrbitError> {
    let active = runs.list_active_with_pipeline_state(workspace)?;
    // Admissions that still hold capacity. Read before the wrapper walk
    // because a wrapper whose lineage reaches one of their leaves is
    // represented by that admission, terminal leaf or not.
    let pending = pulls.list_holding(workspace)?;
    let admitted_runs: BTreeSet<String> = pending
        .iter()
        .filter_map(|record| record.leaf_run_id.clone())
        .collect();

    let mut pipelines: BTreeMap<String, usize> = BTreeMap::new();
    let leaves: BTreeSet<String> = active
        .iter()
        .filter(|(_, job, _)| workflow::is_delivery_leaf_job(job))
        .map(|(id, _, _)| id.clone())
        .collect();
    let mut slots: BTreeSet<String> = leaves.iter().cloned().collect();
    // [ORB-15194] Live claimed leaves past their before-landing review
    // admission. A state that does not decode reads as implementing, so it
    // never earns an extra admission.
    let reviewing = active
        .iter()
        .filter(|(_, job, state)| {
            job == workflow::TASK_CLAIMED_PR_PIPELINE_JOB
                && state
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<PipelineState>(raw).ok())
                    .is_some_and(|state| state.in_landing_review())
        })
        .count();
    for (id, job, state) in &active {
        if workflow::is_delivery_leaf_job(job) {
            *pipelines.entry(job.clone()).or_insert(0) += 1;
        } else if job == LEGACY_WRAPPER_PIPELINE {
            let seen = dispatch_lineage(runs, workspace, state.as_ref())?;
            if seen.is_disjoint(&leaves) && seen.is_disjoint(&admitted_runs) {
                slots.insert(id.clone());
            }
        }
    }

    // Dispatch history remains authoritative even after a wrapper or an old
    // coordinator finishes. Walking it avoids counting a wrapper and its
    // delivery twice, or attributing our detached delivery to another drain.
    let mut owned = if let Some(run_id) = coordinator {
        let raw = runs.get_pipeline_state_json(workspace, run_id)?;
        dispatch_lineage(runs, workspace, raw.as_ref())?
    } else {
        BTreeSet::new()
    };
    let mut owned_unrepresented = 0;
    let mut occupied = slots.len();
    for record in pending {
        if record
            .leaf_run_id
            .as_ref()
            .is_none_or(|id| !slots.contains(id))
        {
            occupied += 1;
            if coordinator == Some(record.request.run_context.run_id.as_str()) {
                owned_unrepresented += 1;
            }
        }
        if coordinator == Some(record.request.run_context.run_id.as_str())
            && let Some(id) = record.leaf_run_id.as_ref()
        {
            owned.insert(id.clone());
        }
        if record
            .leaf_run_id
            .as_ref()
            .is_none_or(|id| !active.iter().any(|(active_id, _, _)| active_id == id))
        {
            *pipelines
                .entry(pipeline(&record.request)?.to_string())
                .or_insert(0) += 1;
        }
    }
    Ok(DrainLeafOccupancy {
        occupied,
        per_pipeline: pipelines,
        inherited: coordinator.map(|_| {
            occupied.saturating_sub(slots.intersection(&owned).count() + owned_unrepresented)
        }),
        reviewing,
    })
}

/// The slots a new pull admission competes for under `ceiling`
/// [ORB-15194].
///
/// Pull-only review credit starts after the persisted before-landing
/// admission has `applies=true`; validation and publishing before that
/// boundary remain implementing. The leaf then stays reviewing through
/// revalidation and handoff while its claim settles. Admission counts
/// occupied minus `min(reviewing, ceiling)`, so credit is capped at `ceiling`
/// but the reviewing population can exceed it as replacements reach review.
/// With an unchanged ceiling and no pre-existing excess, occupied leaves stay
/// at most twice the ceiling and implementing leaves at most the ceiling.
/// Lowering the ceiling preserves running leaves, so existing excess may
/// remain. Owner/local drains continue counting every in-flight child against
/// their ceiling. Review rebuilds still compete for the repository build
/// budget.
fn pull_admission_load(occupancy: &DrainLeafOccupancy, ceiling: usize) -> usize {
    occupancy
        .occupied
        .saturating_sub(occupancy.reviewing.min(ceiling))
}

/// The leaf definitions a claim may select. They are the handoff-only
/// claimed variants, never the merge-capable legacy pipelines: a pulled
/// claim settles through the owner, so a leaf that could merge or complete on
/// its own would bypass the lifecycle the claim exists to enforce.
fn pipeline(request: &AdmissionRequest) -> Result<&'static str, OrbitError> {
    match request.ship.mode.as_str() {
        "pr" => Ok(workflow::TASK_CLAIMED_PR_PIPELINE_JOB),
        "local" => Ok(workflow::TASK_CLAIMED_LOCAL_PIPELINE_JOB),
        _ => Err(invalid("unsupported pulled leaf mode")),
    }
}

/// The review admission a claimed leaf runs under: the owner's captured
/// contract and timing, including an explicit `none` timing when the owner
/// disabled review before landing [ORB-13908] [ORB-14849]. Fails closed when a
/// required review timing arrives without a review contract.
fn claim_review_admission(
    record: &LocalPullAdmission,
    now: chrono::DateTime<Utc>,
) -> Result<ReviewAdmission, OrbitError> {
    let ship = &record.request.ship;
    let timing = ship.review_timing();
    match (&ship.review, timing) {
        (Some(review), _) => Ok(ReviewAdmission {
            contract_version: review.contract_version,
            // The claim's contract is resolved by the owner and carries no
            // operation policy version of its own.
            policy_version: 0,
            timing,
            timing_source: CLAIM_SOURCE.into(),
            crew: review.crew.clone(),
            crew_source: CLAIM_SOURCE.into(),
            // The owner resolved its pool to the one crew the contract carries.
            crew_pool: Vec::new(),
            budget: review.budget,
            required_validation_commands: review.required_validation_commands.clone(),
            baseline_commands: review.baseline_commands.clone(),
            host_evidence: review.host_evidence.clone(),
            selective_before_landing: None,
            captured_at: now,
        }),
        (None, ReviewTiming::None) => {
            let gates = record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.gates.as_ref());
            Ok(ReviewAdmission {
                contract_version: workflow::REVIEW_CONTRACT_VERSION,
                policy_version: 0,
                timing: ReviewTiming::None,
                timing_source: CLAIM_SOURCE.into(),
                crew: None,
                crew_source: CLAIM_SOURCE.into(),
                crew_pool: Vec::new(),
                budget: workflow::ReviewBudget::default(),
                required_validation_commands: gates
                    .map(|gates| gates.required_validation_commands.clone()),
                baseline_commands: gates
                    .map(|gates| gates.baseline_commands.clone())
                    .unwrap_or_default(),
                host_evidence: Vec::new(),
                selective_before_landing: None,
                captured_at: now,
            })
        }
        (None, required) => Err(invalid(&format!(
            "claimed leaf requires review timing `{}` but the ship contract carries no review \
             contract",
            required.as_str()
        ))),
    }
}

/// What a repair claim's leaf needs to restore its preserved candidate: the
/// published branch and head, the base it was validated on, why its landing
/// stopped, and the owner's captured review timing for the repair claim.
fn claim_repair_input(
    repair: &ClaimRepair,
    ship: &orbit_store::contract::AdmissionShipContract,
) -> serde_json::Value {
    let owner_review_timing = repair
        .owner_review_timing
        .unwrap_or_else(|| ship.review_timing());
    serde_json::json!({
        "repairs_claim_id": repair.repairs_claim_id,
        "handoff_id": repair.handoff_id,
        "branch": repair.candidate.source_branch,
        "head_sha": repair.candidate.candidate.commit,
        "base_sha": repair.candidate.base.commit,
        "stop_evidence": repair.stop_evidence,
        "owner_review_timing": owner_review_timing.as_str(),
    })
}
