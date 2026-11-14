//! Out-of-run auto-fix: an `--auto-fix` owner drain repairs an Orbit pull
//! request that no run is watching.
//!
//! A delivery leaf's completion step repairs a failed required check or a
//! merge conflict only while it runs. Once the leaf ends without completing
//! (a forge timeout, a final-recovery escalation or requeue, a dead worker),
//! its pull request can turn red or conflict with its base while nothing
//! watches it. Each pass of a drain started with `--auto-fix` therefore projects
//! task bindings from stopped delivery history without an age window, reads
//! only those tasks' current metadata, and checks each task's newest own leaf.
//! When that pull request is blocked, the pass hands the task back to the
//! leaf's completion step: a
//! checkpoint resume rewound to `pr_complete` (`plan_completion_repair_resume`)
//! with `auto_fix` granted. The watcher there admits the repair against the
//! pull request's attempt ledger, dispatches the `workflow.purpose_crews.auto_fix`
//! crew, which first takes the task's footprint locks, pushes to the same
//! branch and completes the existing pull request. The resume keeps the leaf's
//! own `completion` grant, so the repair itself never merges, and a leaf
//! that never held `--complete` still leaves the task in review.
//!
//! A task's pull request is repaired only when all of these hold:
//!
//! - its newest `task_pr_pipeline` run carries only that task, ended
//!   `failed`, `timeout` or `interrupted` (a dead worker), and is not a
//!   claimed execution. For an operator's single-task Fix PR only, a leaf
//!   that succeeded without `--complete`, and so left its pull request in
//!   review with nothing to repair it [ORB-23003], qualifies too. A leaf that
//!   merged or was cancelled is not resumed;
//! - the task is in `review`, or in `blocked` or `backlog` because that run's
//!   lineage blocked or requeued it (a delivery failure handoff, an
//!   interruption or a final-recovery escalation or requeue). An operator's
//!   block or move, and a `no-auto-approve` tag, are never overridden;
//! - nothing of the task is live: no run, and no unsettled claim;
//! - the drain holds the `--complete` grant the run was started with;
//! - the run's `pr_open` opened the pull request the task records, on an
//!   `orbit/<task>-` branch, and the forge reports it open on that branch,
//!   naming the task and authored by a delivery identity;
//! - it conflicts with its base or a required check failed on its head, and
//!   the pull request's attempt ledger would admit a repair of that head and
//!   blocker (at most one per head and blocker, three per pull request);
//! - no earlier out-of-run auto-fix resumed delivery for that head and
//!   blocker, and fewer than the ledger's budget did for the pull request, so
//!   a resume that dies before it repairs cannot loop;
//! - checkpoint resume accepts the rewound run.
//!
//! A pass starts at most as many repairs as the drain has free slots after
//! adoption. A pull request found unblocked is not read again for
//! [`CLEAN_RECHECK`]. A forge read failure defers the pass's remaining reads;
//! GitHub's `UNKNOWN` mergeability answer skips only that pull request.
//! Nothing here ends the drain: each refusal is reported in the step output
//! and a later pass reconsiders it.
//!
//! A leaf that ended `held` on a delivery refusal a fresh forge read can
//! clear (`delivery_check_evidence_inconclusive`,
//! `delivery_completion_budget_expired`, `delivery_evidence_stale`,
//! `delivery_requirements_unsatisfied`, `delivery_base_checks_pending`,
//! `delivery_forge_observation_unavailable`) is handed back too [ORB-23809],
//! under every other rule above except the blocker: completion reads the
//! forge again, and repairs only what its own watcher then finds.
//! `delivery_check_runs_absent` is handed back the same way once the
//! unchanged head reports a pending or passed required check [ORB-24457].
//! Until that check exists the pass lists the hold under its code and spends
//! no hand-back. A held
//! leaf whose task a `completion.hold` rule keeps in review is not handed
//! back. Its pull request is repaired through the ledger when the forge
//! reports a blocker; otherwise the hand-back spends none of the ledger and
//! is bounded per held lineage by a backoff and a ceiling (`held`), after
//! which the pass reports the task under its hold code. Other holds keep
//! their own documented next step.
//!
//! The dashboard's Fix PR action starts the same repair for one task through
//! [`OrbitRuntime::start_task_pr_auto_fix`] (`task_auto_fix`): the same
//! verdict, plan, start and history record, with no drain behind it.

use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery, TaskListFilter};
use orbit_types::task::{NO_AUTO_APPROVE_TAG, TaskStatus, names_task_delivery_pull_request};
use orbit_types::workflow::{
    self, COMPLETION_HOLD_KEY, DEFAULT_BRANCH_PREFIX, DeliveryRefusal, JobRun, JobRunState,
    PipelineState, PrAutoFixBlocker, current_completion_hold,
};
use serde_json::{Value, json};

use super::super::orphan_adoption::AdoptionAuthority;
use super::super::resume::{OUT_OF_RUN_AUTO_FIX_KEY, shelving_run_id};
use super::super::{RunOwnerLiveness, run_owner_liveness};
use super::held::{
    HELD_HANDBACK_CEILING, handed_back_hold, held_backoff, held_backoff_until, held_handbacks,
    held_refusal,
};
use super::ledger::{OutOfRunStart, ledger_admits};
use crate::OrbitRuntime;
use crate::runtime::run_input::carried_task_ids;

/// Delivery leaves read for one candidate task, newest first, to find the
/// newest one that carries only that task. A leaf that carries several tasks
/// is no task's own, so a few are read past.
pub(super) const TASK_LEAF_SCAN: usize = 8;

/// How long a pull request found without a blocker is not read again.
pub(super) const CLEAN_RECHECK: Duration = Duration::from_secs(300);

/// A repair the pass decided to start.
pub(super) struct Repair {
    task_id: String,
    status: TaskStatus,
    pr_number: String,
    head_sha: String,
    /// What the repair resumes delivery for; `None` for a held hand-back on
    /// a pull request with no blocker.
    blocker: Option<PrAutoFixBlocker>,
    /// The delivery hold a held hand-back resumes completion from.
    hold: Option<DeliveryRefusal>,
    /// The run input record of the repair.
    marker: Value,
}

impl Repair {
    /// The decided repair of `run`'s pull request, before it starts.
    pub(super) fn summary(&self, run: &JobRun) -> Value {
        json!({
            "task_id": self.task_id,
            "source_run_id": run.run_id,
            "pr_number": self.pr_number,
            "head_sha": self.head_sha,
            "blocker": self.blocker.map(PrAutoFixBlocker::as_str),
            "hold_code": self.hold.map(DeliveryRefusal::error_code),
        })
    }
}

/// What one pass does with one task's newest delivery leaf.
pub(super) enum Verdict {
    /// Not a drain pass's to report: the task has no Orbit pull request at
    /// rest, or its leaf is live, finished or someone else's. The reason is
    /// reported only when one task's repair was asked for.
    Ignore(&'static str, String),
    Skip(&'static str, String),
    Repair(Box<Repair>),
    Started(Value),
}

/// Who starts a repair, and the completion authority it holds.
pub(super) struct RepairAuthority<'a> {
    /// The drain run, recorded as the resume's actor; `None` for a start
    /// that no drain makes.
    pub(super) drain_run_id: Option<&'a str>,
    /// The resume's actor when no drain makes it.
    pub(super) actor: Option<&'a str>,
    pub(super) completion_done: bool,
}

/// Reads one pass shares across its candidates.
#[derive(Default)]
pub(super) struct PassReads {
    /// Read the forge even for a pull request found unblocked within
    /// [`CLEAN_RECHECK`]: an operator asking about one task wants a fresh
    /// answer.
    fresh: bool,
    /// The first forge read failure of this pass.
    forge_error: Option<String>,
    /// The delivery identities, once read.
    authors: Option<Result<Vec<String>, String>>,
    /// The activity catalog's load result, once read.
    catalog: Option<Result<(), String>>,
}

/// Facts learned before a candidate is refused. A policy hold can precede
/// the forge read, so a missing blocker must remain unknown.
#[derive(Default)]
pub(super) struct RepairObservation {
    pub(super) pr_number: Option<String>,
    pub(super) blocker: Option<PrAutoFixBlocker>,
    /// The delivery hold of a held leaf the pass considered.
    pub(super) hold_code: Option<&'static str>,
}

impl PassReads {
    /// Reads for one operator request, which ignore [`CLEAN_RECHECK`].
    pub(super) fn fresh() -> Self {
        Self {
            fresh: true,
            ..Self::default()
        }
    }
}

pub(super) enum BlockerRead {
    /// The forge could read the pull request's blockers (or determined it is
    /// no longer open).
    Complete(Option<crate::runtime::task_pr_forge::PullRequestBlockers>),
    /// GitHub is still calculating this pull request's mergeability. This is
    /// local to that candidate and must not defer the rest of the pass.
    MergeabilityUnknown,
}

impl OrbitRuntime {
    /// Start an out-of-run auto-fix for each blocked Orbit pull request whose
    /// task has no live run, at most `budget` of them, within `authority`.
    /// Returns the repairs started and the candidates left alone, and the
    /// pass's own error when it could not read its candidates. Never fails.
    pub(crate) fn repair_out_of_run_pull_requests(
        &self,
        authority: &AdoptionAuthority<'_>,
        budget: usize,
    ) -> Value {
        if !authority.auto_fix {
            return json!({ "started": [], "skipped": [] });
        }
        if self.is_write_free() {
            return json!({ "started": [], "skipped": [], "hold_reason": "write_free" });
        }
        let throttled = self.resource_admission().throttle.is_some();
        let repair_authority = RepairAuthority {
            drain_run_id: Some(authority.drain_run_id),
            actor: Some(authority.drain_run_id),
            completion_done: authority.completion_done,
        };
        let tasks = {
            let _span = tracing::debug_span!("pr_repair_candidates").entered();
            // A repair needs a stopped or held delivery. Project task IDs
            // from all such leaves before reading envelopes, so unrelated task history
            // costs no reads and old stopped leaves remain discoverable. This
            // projection is fresh on each pass, with no age window or cache.
            let tasks = self
                .stores()
                .jobs()
                .job_run_task_ids(&JobRunQuery {
                    job_id: Some(workflow::TASK_PR_PIPELINE_JOB.to_string()),
                    states: vec![
                        JobRunState::Failed,
                        JobRunState::Timeout,
                        JobRunState::Interrupted,
                        JobRunState::Held,
                    ],
                    include_steps: false,
                    ..Default::default()
                })
                .and_then(|mut ids| {
                    // Malformed bindings in an unrelated historical run cannot
                    // name a task, and must not refuse the other candidates.
                    ids.retain(|id| orbit_types::task::is_valid_orb_task_id(id));
                    // Narrow by task state before reading envelopes: only a
                    // task now in review/blocked/backlog can be repaired, so
                    // intersect the projected ids with that bounded status
                    // population (answered from the generated index alone)
                    // and request metadata for at most those ids. A worker
                    // reads through its owner, which has no local index, so
                    // it keeps the keyed owner read.
                    if self.worker_invocation().is_none() {
                        let open_filter = TaskListFilter {
                            statuses: Some(vec![
                                TaskStatus::Review,
                                TaskStatus::Blocked,
                                TaskStatus::Backlog,
                            ]),
                            ..TaskListFilter::default()
                        };
                        match self.task_candidate_keys(&open_filter, usize::MAX) {
                            Ok(Some(keys)) => {
                                let open: std::collections::BTreeSet<String> =
                                    keys.items.into_iter().map(|key| key.id).collect();
                                ids.retain(|id| open.contains(id));
                            }
                            Ok(None) => {
                                // The index cannot serve this filter: read
                                // envelopes for the open statuses only (the
                                // bounded side) and keep the projected ones.
                                let candidates = self.task_candidates(&open_filter, usize::MAX)?;
                                return Ok(candidates
                                    .items
                                    .into_iter()
                                    .filter(|envelope| ids.contains(&envelope.id))
                                    .map(crate::application::task::metadata_task)
                                    .collect());
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    self.list_task_metadata_for_ids(&ids)
                });
            match tasks {
                Ok(tasks) => tasks,
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.drain",
                        error = %error,
                        "could not list tasks; this pass repairs no pull request",
                    );
                    return json!({
                        "started": [],
                        "skipped": [],
                        "pass_error": format!("list tasks: {error}"),
                    });
                }
            }
        };
        let mut started = Vec::new();
        let mut skipped = Vec::new();
        let mut reads = PassReads::default();
        // Re-read the newest own leaf, including live and finished ones, so a
        // newer delivery supersedes the stopped leaf that nominated this task.
        let leaf_span = tracing::debug_span!("pr_repair_leaf_lookup").entered();
        let mut leaves = Vec::new();
        for task in tasks.iter().filter(|task| {
            matches!(
                task.status,
                TaskStatus::Review | TaskStatus::Blocked | TaskStatus::Backlog
            ) && task.github_pr_number().is_some()
        }) {
            match self.newest_own_delivery_leaf(&task.id) {
                Ok(Some(run)) => leaves.push((task.id.clone(), run)),
                Ok(None) => {}
                Err(error) => skipped.push(json!({
                    "task_id": task.id,
                    "source_run_id": Value::Null,
                    "pr_number": task.github_pr_number(),
                    "blocker": Value::Null,
                    "hold_code": Value::Null,
                    "reason": "store_unreadable",
                    "detail": format!("read the task's delivery leaves: {error}"),
                })),
            }
        }
        drop(leaf_span);
        // Newest first, so a pass short of slots serves the freshest leaves.
        leaves.sort_by(|(left_task, left), (right_task, right)| {
            right
                .recency_at()
                .cmp(&left.recency_at())
                .then_with(|| left.run_id.cmp(&right.run_id))
                .then_with(|| left_task.cmp(right_task))
        });
        for (task_id, run) in leaves {
            let task_id = task_id.as_str();
            let hold = if throttled {
                Some((
                    "resource_throttled",
                    "host resource pressure holds this pass",
                ))
            } else if started.len() >= budget {
                Some((
                    "no_capacity",
                    "no free leaf slot remains after adoption and earlier repairs",
                ))
            } else {
                None
            };
            let mut observation = RepairObservation::default();
            let verdict = match self.repair_verdict(
                &run,
                task_id,
                &repair_authority,
                &mut reads,
                &mut observation,
                hold,
            ) {
                Verdict::Repair(repair) => match self.plan_repair(&run, *repair, &mut reads) {
                    Verdict::Repair(repair) => self.start_repair(&run, &repair_authority, *repair),
                    verdict => verdict,
                },
                verdict => verdict,
            };
            match verdict {
                Verdict::Ignore(..) | Verdict::Repair(_) => {}
                Verdict::Started(entry) => started.push(entry),
                Verdict::Skip(reason, detail) => {
                    tracing::debug!(
                        target: "orbit.core.drain",
                        run_id = %run.run_id,
                        task_id = %task_id,
                        reason,
                        detail = %detail,
                        "drain left a task's pull request to its own delivery",
                    );
                    skipped.push(json!({
                        "task_id": task_id,
                        "source_run_id": run.run_id,
                        "pr_number": observation.pr_number,
                        "blocker": observation.blocker,
                        "hold_code": observation.hold_code,
                        "reason": reason,
                        "detail": detail,
                    }));
                }
            }
        }
        json!({ "started": started, "skipped": skipped })
    }

    /// The newest `task_pr_pipeline` run that carries only `task_id`, whatever
    /// its state: a later finished run speaks for the task, so the verdict
    /// never resumes an older stopped one.
    pub(super) fn newest_own_delivery_leaf(
        &self,
        task_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(workflow::TASK_PR_PIPELINE_JOB.to_string()),
                task_id: Some(task_id.to_string()),
                order_by: JobRunOrder::Recency,
                limit: Some(TASK_LEAF_SCAN),
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .into_iter()
            .find(|run| {
                let carried = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
                carried.len() == 1 && carried.contains(task_id)
            }))
    }

    /// Whether the pull request of `task_id`'s newest leaf `run` is repaired,
    /// and if not, why. A repair still has to be planned
    /// ([`Self::plan_repair`]) before it starts.
    pub(super) fn repair_verdict(
        &self,
        run: &JobRun,
        task_id: &str,
        authority: &RepairAuthority<'_>,
        reads: &mut PassReads,
        observation: &mut RepairObservation,
        hold: Option<(&'static str, &str)>,
    ) -> Verdict {
        let skip = |reason, detail: String| Verdict::Skip(reason, detail);
        // A live leaf watches its own pull request; a finished or cancelled
        // one is not resumed.
        if !run.state.is_terminal() {
            return Verdict::Ignore(
                "live_run",
                format!("its delivery run {} is still {}", run.run_id, run.state),
            );
        }
        let input = run.input.clone().unwrap_or(Value::Null);
        // [ORB-23003] A leaf that succeeded without `completion: done` ended
        // at review with its pull request open and unwatched. An operator's
        // Fix PR (no drain) hands it back like a stopped one; a drain pass
        // still repairs only stopped leaves. A succeeded `--complete` leaf
        // merged.
        let ended_at_review = authority.drain_run_id.is_none()
            && run.state == JobRunState::Success
            && input.get("completion").and_then(Value::as_str) != Some("done");
        // [ORB-23809] A leaf held on a delivery refusal a fresh forge read
        // can clear is handed back like a stopped one. A host-manager stop
        // is an interruption recorded as a hold and is handed back the same
        // way. Every other hold keeps its own next step.
        let mut operator_stopped = false;
        let held = if run.state == JobRunState::Held {
            let refusal = match self.stores().jobs().get_job_run(&run.run_id) {
                Ok(Some(full)) => {
                    operator_stopped = orbit_types::workflow::is_operator_stopped(
                        full.state,
                        full.steps
                            .last()
                            .and_then(|step| step.error_code.as_deref()),
                    );
                    (!operator_stopped).then(|| held_refusal(&full)).flatten()
                }
                Ok(None) => None,
                Err(error) => {
                    return skip("store_unreadable", format!("read the run's steps: {error}"));
                }
            };
            match refusal {
                _ if operator_stopped => None,
                Some(refusal) if handed_back_hold(refusal) => Some(refusal),
                refusal => {
                    return Verdict::Ignore(
                        "leaf_held",
                        format!(
                            "its newest delivery run {} is held{}; only a hold a fresh forge read \
                             can clear is handed back, and this one keeps its own next step",
                            run.run_id,
                            refusal
                                .map(|refusal| format!(" as {}", refusal.error_code()))
                                .unwrap_or_default()
                        ),
                    );
                }
            }
        } else {
            None
        };
        observation.hold_code = held.map(DeliveryRefusal::error_code);
        if !ended_at_review
            && held.is_none()
            && !operator_stopped
            && !matches!(
                run.state,
                JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
            )
        {
            return Verdict::Ignore(
                "leaf_not_stopped",
                format!(
                    "its newest delivery run {} ended {}; only a delivery that failed, timed \
                     out, was interrupted, was stopped by the host manager, held on a \
                     re-observable delivery hold or ended in review is handed back",
                    run.run_id, run.state
                ),
            );
        }
        if input.get("claimed").and_then(Value::as_bool) == Some(true) {
            return Verdict::Ignore(
                "claimed_execution",
                "its newest delivery ran as a claimed execution on another machine".to_string(),
            );
        }
        let task = match self.get_task(task_id) {
            Ok(task) => task,
            Err(OrbitError::NotFound { .. }) => {
                return Verdict::Ignore("not_found", "the task does not exist".to_string());
            }
            Err(error) => return skip("store_unreadable", format!("read the task: {error}")),
        };
        if !matches!(
            task.status,
            TaskStatus::Review | TaskStatus::Blocked | TaskStatus::Backlog
        ) {
            return Verdict::Ignore(
                "task_status",
                format!(
                    "the task is {}; only a delivered task is repaired",
                    task.status
                ),
            );
        }
        let Some(pr_number) = task.github_pr_number().map(str::to_string) else {
            return Verdict::Ignore(
                "no_pull_request",
                "the task records no pull request".to_string(),
            );
        };
        observation.pr_number = Some(pr_number.clone());
        // Report held candidates without spending forge reads or admitting
        // repairs; the newest-leaf scan remains bounded even at capacity.
        if let Some((reason, detail)) = hold {
            return skip(reason, detail.to_string());
        }
        // From here the task records a pull request and its leaf has
        // stopped: every refusal is reported.
        if !self.task_run_is_local(&task) {
            return skip(
                "claimed_execution",
                "the task is bound to another machine's run".to_string(),
            );
        }
        if task.tags.iter().any(|tag| tag == NO_AUTO_APPROVE_TAG) {
            return skip(
                "opted_out",
                format!("the task is tagged {NO_AUTO_APPROVE_TAG}; a person decides its delivery"),
            );
        }
        let history = match self.get_task_history(task_id) {
            Ok(history) => history,
            Err(error) => return skip("store_unreadable", format!("read the history: {error}")),
        };
        if let Some(refusal) = held {
            if let Some(hold) = current_completion_hold(task.status, &history) {
                return skip(
                    "completion_held",
                    format!(
                        "a `{COMPLETION_HOLD_KEY}` rule keeps the task in review for a person \
                         ({hold}); its held delivery is not handed back"
                    ),
                );
            }
            // An operator asking about one task is not held to the drain's
            // backoff; the ceiling still binds both.
            if !reads.fresh
                && let Some(until) = held_backoff_until(
                    run.finished_at,
                    held_backoff(&history, &pr_number, refusal),
                    Utc::now(),
                )
            {
                return skip(
                    refusal.error_code(),
                    format!(
                        "its delivery run {} held as {}; it is handed back no earlier than {}",
                        run.run_id,
                        refusal.error_code(),
                        until.to_rfc3339()
                    ),
                );
            }
        }
        if task.status != TaskStatus::Review {
            let lineage = match self.resume_lineage_run_ids(run) {
                Ok((_, lineage)) => lineage,
                Err(error) => {
                    return skip(
                        "store_unreadable",
                        format!("read the run's lineage: {error}"),
                    );
                }
            };
            if !shelving_run_id(&history).is_some_and(|shelver| lineage.contains(shelver)) {
                return skip(
                    "operator_hold",
                    format!(
                        "the task is {} by a decision its delivery did not make; an operator \
                         releases it",
                        task.status
                    ),
                );
            }
        }
        if let Some(live) = self.live_task_delivery(&task) {
            return skip("live_run", live);
        }
        if run_owner_liveness(run) == RunOwnerLiveness::Alive
            && (run.state == JobRunState::Interrupted || operator_stopped)
        {
            return skip("live_run", "the run's worker is still alive".to_string());
        }
        match self.is_claimed_execution(&run.run_id) {
            Ok(false) => {}
            Ok(true) => {
                return skip(
                    "claimed_execution",
                    "a claimed leaf's release keeps its pull request for the task's next claim"
                        .to_string(),
                );
            }
            Err(error) => {
                return skip("store_unreadable", format!("read the run's claim: {error}"));
            }
        }
        if input.get("completion").and_then(Value::as_str) == Some("done")
            && !authority.completion_done
        {
            return skip(
                "authority",
                "the run was started with --complete; this drain was not".to_string(),
            );
        }
        let (number, head) = match self.read_run_state(&run.run_id) {
            Ok(Some(state)) => {
                let opened = state.pipeline.get("pr_open").unwrap_or(&Value::Null);
                match (
                    opened.get("pr_number").and_then(Value::as_str),
                    opened.get("head").and_then(Value::as_str),
                ) {
                    (Some(number), Some(head)) => (number.to_string(), head.to_string()),
                    _ => {
                        if Self::run_published_failure_handoff_pr(&state, task_id, &pr_number) {
                            return skip(
                                "failure_handoff",
                                "the run published its pull request through its failure handoff \
                                 instead of pr_open, so the drain cannot resume it; when the \
                                 published candidate is sound, run `orbit task requeue <task-id> \
                                 --reason '<why this candidate can continue>'` (the requeue can \
                                 refuse when the task specification changed or no handoff receipt \
                                 binds the candidate)"
                                    .to_string(),
                            );
                        }
                        return skip(
                            "no_pull_request",
                            "the run did not open a pull request".to_string(),
                        );
                    }
                }
            }
            Ok(None) => {
                return skip("no_pull_request", "the run kept no checkpoints".to_string());
            }
            Err(error) => {
                return skip(
                    "store_unreadable",
                    format!("read the run's checkpoints: {error}"),
                );
            }
        };
        if number != pr_number {
            return skip(
                "pull_request_not_recorded",
                format!("the run opened #{number}; the task records #{pr_number}"),
            );
        }
        let Ok(pull_number) = number.parse::<u64>() else {
            return skip(
                "no_pull_request",
                format!("the run recorded pull request '{number}'"),
            );
        };
        let branch_template = &self
            .context
            .settings()
            .pr_settings()
            .conventions
            .branch_name;
        if !branch_template.branch_names_task(&head, DEFAULT_BRANCH_PREFIX, task_id) {
            return skip(
                "not_orbit_pull_request",
                format!("'{head}' is not one of the task's Orbit branches"),
            );
        }
        if !reads.fresh
            && held.is_none()
            && self
                .pr_repair_checks
                .lock()
                .ok()
                .and_then(|checks| checks.get(&pull_number).copied())
                .is_some_and(|at| at.elapsed() < CLEAN_RECHECK)
        {
            return skip(
                "checked_recently",
                format!(
                    "#{number} had no blocker when last read; it is read again after {}s",
                    CLEAN_RECHECK.as_secs()
                ),
            );
        }
        let Some(blockers) = (match self.read_blockers(pull_number, reads) {
            Ok(BlockerRead::Complete(blockers)) => blockers,
            Ok(BlockerRead::MergeabilityUnknown) => {
                return skip(
                    "mergeability_unknown",
                    format!("GitHub is still calculating mergeability for #{number}"),
                );
            }
            Err(error) => return skip("forge_unreadable", error),
        }) else {
            return skip(
                "pull_request_not_open",
                format!("#{number} is not open, or the forge cannot report its blockers"),
            );
        };
        if blockers.pull.head_branch != head {
            return skip(
                "pull_request_not_open",
                format!("#{number} is no longer open on '{head}'"),
            );
        }
        if !names_task_delivery_pull_request(branch_template, &head, &blockers.pull.body, task_id) {
            return skip(
                "not_orbit_pull_request",
                format!("#{number} does not name the task"),
            );
        }
        let authors = reads.authors.get_or_insert_with(|| {
            let repo_root = self.context.paths().repo_root.clone();
            self.delivery_authors(self.task_pr_forge(), &repo_root)
                .map_err(|error| error.to_string())
        });
        match authors {
            Ok(authors) if authors.contains(&blockers.pull.author.to_ascii_lowercase()) => {}
            Ok(_) => {
                return skip(
                    "not_orbit_pull_request",
                    format!(
                        "#{number} was opened by '{}', not a delivery identity",
                        blockers.pull.author
                    ),
                );
            }
            Err(error) => {
                return skip(
                    "forge_unreadable",
                    format!("read the delivery identity: {error}"),
                );
            }
        }
        let blocker = if blockers.conflicting {
            Some(PrAutoFixBlocker::Conflict)
        } else if !blockers.failed_required_checks.is_empty() {
            Some(PrAutoFixBlocker::FailedChecks)
        } else {
            None
        };
        // A blocker is repaired through the ledger, whether the leaf stopped
        // or held. A held leaf with none is handed back for completion to
        // read the forge again, within its own ceiling.
        let hold = match (blocker, held) {
            (Some(blocker), _) => {
                observation.blocker = Some(blocker);
                if let Err(skipped) = ledger_admits(&history, &number, &blockers.head_sha, blocker)
                {
                    return skipped;
                }
                None
            }
            (None, Some(refusal)) => {
                // [ORB-24457] A head that still reports no required check run
                // has nothing for completion to watch. Keep the hold visible
                // under its code and do not spend a hand-back on it.
                if refusal == DeliveryRefusal::CheckRunsAbsent
                    && blockers.pending_required_checks.is_empty()
                    && blockers.passed_required_checks.is_empty()
                {
                    return skip(
                        refusal.error_code(),
                        format!(
                            "#{number} head {} still reports no required check run, so the {} \
                             hold stays until one does; this pass spends no hand-back",
                            blockers.head_sha,
                            refusal.error_code()
                        ),
                    );
                }
                let handed_back = held_handbacks(&history, &number, &blockers.head_sha, refusal);
                if handed_back >= HELD_HANDBACK_CEILING {
                    return skip(
                        refusal.error_code(),
                        format!(
                            "#{number} was handed back {handed_back} times on {} and its delivery \
                             held again as {}, the ceiling of {HELD_HANDBACK_CEILING} per head \
                             and hold; a person decides it now",
                            blockers.head_sha,
                            refusal.error_code()
                        ),
                    );
                }
                Some(refusal)
            }
            (None, None) => {
                if let Ok(mut checks) = self.pr_repair_checks.lock() {
                    checks.insert(pull_number, Instant::now());
                }
                return skip(
                    "no_blocker",
                    format!(
                        "#{number} does not conflict and no required check failed on {}",
                        blockers.head_sha
                    ),
                );
            }
        };
        let mut marker = json!({
            "task_id": task_id,
            "pr_number": number,
            "head_sha": blockers.head_sha,
            "blocker": blocker.map(PrAutoFixBlocker::as_str),
            "failed_checks": blockers.failed_required_checks,
            "source_run_id": run.run_id,
            "drain_run_id": authority.drain_run_id,
        });
        if let Some(hold) = hold {
            marker["hold_code"] = json!(hold.error_code());
        }
        Verdict::Repair(Box::new(Repair {
            task_id: task_id.to_string(),
            status: task.status,
            pr_number: number,
            head_sha: blockers.head_sha,
            blocker,
            hold,
            marker,
        }))
    }

    /// Whether `state` carries the failure handoff that published `pr_number`
    /// for `task_id` instead of a `pr_open` checkpoint: activity
    /// `pr_failure_handoff` with a blocked-PR decision naming this task and
    /// its recorded pull request. The number may be recorded as a string or
    /// a JSON number, as the handoff receipt accepts both.
    fn run_published_failure_handoff_pr(
        state: &PipelineState,
        task_id: &str,
        pr_number: &str,
    ) -> bool {
        let Some(checkpoint) = state.failure_activity_checkpoint.as_ref() else {
            return false;
        };
        if checkpoint.activity_name != "pr_failure_handoff" {
            return false;
        }
        let output = &checkpoint.output;
        if !matches!(
            output.get("decision").and_then(Value::as_str),
            Some("blocked_failure_pr" | "blocked_conflict_pr")
        ) {
            return false;
        }
        if output.get("task_id").and_then(Value::as_str) != Some(task_id) {
            return false;
        }
        let published = output
            .get("pr_number")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                output
                    .get("pr_number")
                    .and_then(Value::as_u64)
                    .map(|number| number.to_string())
            });
        if published.as_deref() != Some(pr_number) {
            return false;
        }
        true
    }

    /// Whether checkpoint resume accepts `run` rewound for `repair`.
    pub(super) fn plan_repair(
        &self,
        run: &JobRun,
        repair: Repair,
        reads: &mut PassReads,
    ) -> Verdict {
        // Resume planning reports an unloadable activity catalog as a
        // validation refusal; load it first so that case reads as one.
        let catalog = reads.catalog.get_or_insert_with(|| {
            self.v2_activity_catalog()
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        if let Err(error) = catalog {
            return Verdict::Skip(
                "resume_unplannable",
                format!("load the activity catalog: {error}"),
            );
        }
        match self.plan_completion_repair_resume(&run.run_id, &repair.marker) {
            Ok(_) => Verdict::Repair(Box::new(repair)),
            Err(error) if error.job_validation_message().is_some() => {
                Verdict::Skip("not_resumable", error.to_string())
            }
            Err(error) => Verdict::Skip("resume_unplannable", error.to_string()),
        }
    }

    /// The forge's answer for `number`, or why it was not read. A genuine
    /// forge failure is latched for the pass; unknown mergeability is local
    /// to one pull request.
    fn read_blockers(&self, number: u64, reads: &mut PassReads) -> Result<BlockerRead, String> {
        if let Some(error) = &reads.forge_error {
            return Err(format!(
                "not read after this pass's earlier forge failure: {error}"
            ));
        }
        let repo_root = self.context.paths().repo_root.clone();
        let _span = tracing::debug_span!("pr_repair_forge").entered();
        match self
            .task_pr_forge()
            .pull_request_blockers(&repo_root, number)
        {
            Ok(blockers) => Ok(BlockerRead::Complete(blockers)),
            Err(OrbitError::PullRequestMergeabilityUnknown { number: unknown })
                if unknown == number =>
            {
                Ok(BlockerRead::MergeabilityUnknown)
            }
            Err(error) => {
                let error = error.to_string();
                reads.forge_error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Resume `run` to repair `repair`, re-checking first that its task has
    /// not moved and nothing of it started since the verdict.
    pub(super) fn start_repair(
        &self,
        run: &JobRun,
        authority: &RepairAuthority<'_>,
        repair: Repair,
    ) -> Verdict {
        match self.get_task(&repair.task_id) {
            Ok(task) if task.status == repair.status => {
                if let Some(live) = self.live_task_delivery(&task) {
                    return Verdict::Skip("live_run", live);
                }
            }
            Ok(task) => {
                return Verdict::Skip(
                    "task_moved",
                    format!("the task moved to {} during the pass", task.status),
                );
            }
            Err(error) => {
                return Verdict::Skip("store_unreadable", format!("read the task: {error}"));
            }
        }
        let invoke =
            match self.submit_completion_repair_run(&run.run_id, &repair.marker, authority.actor) {
                Ok(invoke) => invoke,
                Err(error) => return Verdict::Skip("resume_failed", error.to_string()),
            };
        tracing::info!(
            target: "orbit.core.drain",
            source_run_id = %run.run_id,
            resumed_run_id = %invoke.run_id,
            task_id = %repair.task_id,
            pr = %repair.pr_number,
            blocker = repair.blocker.map(PrAutoFixBlocker::as_str),
            hold_code = repair.hold.map(DeliveryRefusal::error_code),
            drain_run_id = authority.drain_run_id,
            "handed a pull request back to its delivery for an auto-fix",
        );
        self.record_out_of_run_start(
            &repair.task_id,
            &OutOfRunStart {
                pr_number: repair.pr_number.clone(),
                head_sha: repair.head_sha.clone(),
                blocker: repair.blocker,
                hold_code: repair.hold.map(|hold| hold.error_code().to_string()),
                source_run_id: run.run_id.clone(),
                resumed_run_id: invoke.run_id.clone(),
                drain_run_id: authority.drain_run_id.map(str::to_string),
            },
        );
        Verdict::Started(json!({
            "task_id": repair.task_id,
            "source_run_id": run.run_id,
            "resumed_run_id": invoke.run_id,
            "pr_number": repair.pr_number,
            "head_sha": repair.head_sha,
            "blocker": repair.blocker.map(PrAutoFixBlocker::as_str),
            "hold_code": repair.hold.map(DeliveryRefusal::error_code),
        }))
    }

    /// The out-of-run auto-fixes live now, for readiness: each pending or
    /// running delivery leaf a drain resumed to repair its pull request.
    pub(crate) fn live_out_of_run_auto_fixes(&self) -> Result<Vec<Value>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs(workflow::TASK_PR_PIPELINE_JOB)?
            .into_iter()
            .filter_map(|run| {
                let input = run.input.as_ref()?;
                let repair = input.get(OUT_OF_RUN_AUTO_FIX_KEY)?;
                Some(json!({
                    "run_id": run.run_id,
                    "state": run.state,
                    "task_ids": carried_task_ids(input),
                    "pr_number": repair["pr_number"],
                    "blocker": repair["blocker"],
                    "hold_code": repair.get("hold_code").cloned().unwrap_or(Value::Null),
                    "head_sha": repair["head_sha"],
                    "source_run_id": repair["source_run_id"],
                    "drain_run_id": repair["drain_run_id"],
                }))
            })
            .collect())
    }
}
