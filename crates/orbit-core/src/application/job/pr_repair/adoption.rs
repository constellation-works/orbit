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
//!   claimed execution. A leaf that finished or was cancelled is not resumed;
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

use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{JobRunOrder, JobRunQuery};
use orbit_types::task::{
    NO_AUTO_APPROVE_TAG, TaskHistoryEntry, TaskStatus, names_task_delivery_pull_request,
};
use orbit_types::workflow::{JobRun, JobRunState, PrAutoFixBlocker};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::orphan_adoption::AdoptionAuthority;
use super::super::resume::{OUT_OF_RUN_AUTO_FIX_KEY, shelving_run_id};
use super::super::{RunOwnerLiveness, run_owner_liveness};
use super::ledger::{ledger_admits, run_recency};
use crate::OrbitRuntime;
use crate::application::task::SYSTEM_ACTOR_LABEL;
use crate::runtime::run_input::carried_task_ids;

/// Task history event recording one out-of-run auto-fix the drain started.
pub(crate) const PR_AUTO_FIX_OUT_OF_RUN_EVENT: &str = "pr_auto_fix_out_of_run";

/// The delivery leaf that opens and watches a pull request.
pub(super) const DELIVERY_LEAF_JOB: &str = "task_pr_pipeline";

/// Delivery leaves read for one candidate task, newest first, to find the
/// newest one that carries only that task. A leaf that carries several tasks
/// is no task's own, so a few are read past.
pub(super) const TASK_LEAF_SCAN: usize = 8;

/// How long a pull request found without a blocker is not read again.
pub(super) const CLEAN_RECHECK: Duration = Duration::from_secs(300);

/// One out-of-run auto-fix, as recorded in the task's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OutOfRunStart {
    pub(super) pr_number: String,
    pub(super) head_sha: String,
    pub(super) blocker: PrAutoFixBlocker,
    pub(super) source_run_id: String,
    pub(super) resumed_run_id: String,
    pub(super) drain_run_id: String,
}

/// Every out-of-run auto-fix recorded for `pr_number`.
pub(super) fn out_of_run_starts(
    history: &[TaskHistoryEntry],
    pr_number: &str,
) -> Vec<OutOfRunStart> {
    history
        .iter()
        .filter(|entry| entry.event == PR_AUTO_FIX_OUT_OF_RUN_EVENT)
        .filter_map(|entry| serde_json::from_str::<OutOfRunStart>(entry.note.as_deref()?).ok())
        .filter(|start| start.pr_number == pr_number)
        .collect()
}

/// A repair the pass decided to start.
pub(super) struct Repair {
    task_id: String,
    status: TaskStatus,
    pr_number: String,
    head_sha: String,
    blocker: PrAutoFixBlocker,
    /// The run input record of the repair.
    marker: Value,
}

/// What one pass does with one task's newest delivery leaf.
pub(super) enum Verdict {
    /// Not this pass's to report: the task has no Orbit pull request at
    /// rest, or its leaf is live, finished or someone else's.
    Ignore,
    Skip(&'static str, String),
    Repair(Box<Repair>),
    Started(Value),
}

/// Reads one pass shares across its candidates.
#[derive(Default)]
pub(super) struct PassReads {
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
    pr_number: Option<String>,
    blocker: Option<PrAutoFixBlocker>,
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
        let tasks = {
            let _span = tracing::debug_span!("pr_repair_candidates").entered();
            // A repair needs a stopped delivery. Project task IDs from all
            // stopped leaves before reading envelopes, so unrelated task history
            // costs no reads and old stopped leaves remain discoverable. This
            // projection is fresh on each pass, with no age window or cache.
            let tasks = self
                .stores()
                .jobs()
                .job_run_task_ids(&JobRunQuery {
                    job_id: Some(DELIVERY_LEAF_JOB.to_string()),
                    states: vec![
                        JobRunState::Failed,
                        JobRunState::Timeout,
                        JobRunState::Interrupted,
                    ],
                    include_steps: false,
                    ..Default::default()
                })
                .and_then(|mut ids| {
                    // Malformed bindings in an unrelated historical run cannot
                    // name a task, and must not refuse the other candidates.
                    ids.retain(|id| orbit_types::task::is_valid_orb_task_id(id));
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
                    "reason": "store_unreadable",
                    "detail": format!("read the task's delivery leaves: {error}"),
                })),
            }
        }
        drop(leaf_span);
        // Newest first, so a pass short of slots serves the freshest leaves.
        leaves.sort_by(|(left_task, left), (right_task, right)| {
            run_recency(right)
                .cmp(&run_recency(left))
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
                authority,
                &mut reads,
                &mut observation,
                hold,
            ) {
                Verdict::Repair(repair) => self.start_repair(&run, authority, *repair),
                verdict => verdict,
            };
            match verdict {
                Verdict::Ignore | Verdict::Repair(_) => {}
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
    fn newest_own_delivery_leaf(&self, task_id: &str) -> Result<Option<JobRun>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(DELIVERY_LEAF_JOB.to_string()),
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
    /// and if not, why.
    fn repair_verdict(
        &self,
        run: &JobRun,
        task_id: &str,
        authority: &AdoptionAuthority<'_>,
        reads: &mut PassReads,
        observation: &mut RepairObservation,
        hold: Option<(&'static str, &str)>,
    ) -> Verdict {
        let skip = |reason, detail: String| Verdict::Skip(reason, detail);
        // A live leaf watches its own pull request; a finished or cancelled
        // one is not resumed.
        if !matches!(
            run.state,
            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
        ) {
            return Verdict::Ignore;
        }
        let input = run.input.clone().unwrap_or(Value::Null);
        if input.get("claimed").and_then(Value::as_bool) == Some(true) {
            return Verdict::Ignore;
        }
        let task = match self.get_task(task_id) {
            Ok(task) => task,
            Err(OrbitError::NotFound { .. }) => return Verdict::Ignore,
            Err(error) => return skip("store_unreadable", format!("read the task: {error}")),
        };
        if !matches!(
            task.status,
            TaskStatus::Review | TaskStatus::Blocked | TaskStatus::Backlog
        ) {
            return Verdict::Ignore;
        }
        let Some(pr_number) = task.github_pr_number().map(str::to_string) else {
            return Verdict::Ignore;
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
            && run.state == JobRunState::Interrupted
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
        if !head.starts_with(&format!("orbit/{task_id}-")) {
            return skip(
                "not_orbit_pull_request",
                format!("'{head}' is not one of the task's Orbit branches"),
            );
        }
        if self
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
        if !names_task_delivery_pull_request(&head, &blockers.pull.body, task_id) {
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
            PrAutoFixBlocker::Conflict
        } else if !blockers.failed_required_checks.is_empty() {
            PrAutoFixBlocker::FailedChecks
        } else {
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
        };
        observation.blocker = Some(blocker);
        if let Err(skipped) = ledger_admits(&history, &number, &blockers.head_sha, blocker) {
            return skipped;
        }
        // Resume planning reports an unloadable activity catalog as a
        // validation refusal; load it first so that case reads as one.
        let catalog = reads.catalog.get_or_insert_with(|| {
            self.v2_activity_catalog()
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        if let Err(error) = catalog {
            return skip(
                "resume_unplannable",
                format!("load the activity catalog: {error}"),
            );
        }
        let marker = json!({
            "task_id": task_id,
            "pr_number": number,
            "head_sha": blockers.head_sha,
            "blocker": blocker.as_str(),
            "failed_checks": blockers.failed_required_checks,
            "source_run_id": run.run_id,
            "drain_run_id": authority.drain_run_id,
        });
        match self.plan_completion_repair_resume(&run.run_id, &marker) {
            Ok(_) => Verdict::Repair(Box::new(Repair {
                task_id: task_id.to_string(),
                status: task.status,
                pr_number: number,
                head_sha: blockers.head_sha,
                blocker,
                marker,
            })),
            Err(error @ OrbitError::JobValidation(_)) => skip("not_resumable", error.to_string()),
            Err(error) => skip("resume_unplannable", error.to_string()),
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
    fn start_repair(
        &self,
        run: &JobRun,
        authority: &AdoptionAuthority<'_>,
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
        let invoke = match self.submit_completion_repair_run(
            &run.run_id,
            &repair.marker,
            Some(authority.drain_run_id),
        ) {
            Ok(invoke) => invoke,
            Err(error) => return Verdict::Skip("resume_failed", error.to_string()),
        };
        tracing::info!(
            target: "orbit.core.drain",
            source_run_id = %run.run_id,
            resumed_run_id = %invoke.run_id,
            task_id = %repair.task_id,
            pr = %repair.pr_number,
            blocker = repair.blocker.as_str(),
            "drain handed a blocked pull request back to its delivery for an auto-fix",
        );
        self.record_out_of_run_start(
            &repair,
            &OutOfRunStart {
                pr_number: repair.pr_number.clone(),
                head_sha: repair.head_sha.clone(),
                blocker: repair.blocker,
                source_run_id: run.run_id.clone(),
                resumed_run_id: invoke.run_id.clone(),
                drain_run_id: authority.drain_run_id.to_string(),
            },
        );
        Verdict::Started(json!({
            "task_id": repair.task_id,
            "source_run_id": run.run_id,
            "resumed_run_id": invoke.run_id,
            "pr_number": repair.pr_number,
            "head_sha": repair.head_sha,
            "blocker": repair.blocker.as_str(),
        }))
    }

    /// Record `start` in the task's history, which bounds later passes. A
    /// lost record is logged: the resumed run's own ledger admission still
    /// bounds its repair.
    fn record_out_of_run_start(&self, repair: &Repair, start: &OutOfRunStart) {
        let result = serde_json::to_string(start)
            .map_err(|error| OrbitError::Execution(error.to_string()))
            .and_then(|note| {
                self.apply_task_automation_update(
                    &repair.task_id,
                    TaskAutomationUpdate {
                        append_history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: SYSTEM_ACTOR_LABEL.to_string(),
                            event: PR_AUTO_FIX_OUT_OF_RUN_EVENT.to_string(),
                            note: Some(note),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..TaskAutomationUpdate::default()
                    },
                )
                .map(|_| ())
            });
        if let Err(error) = result {
            tracing::warn!(
                target: "orbit.core.drain",
                task_id = %repair.task_id,
                resumed_run_id = %start.resumed_run_id,
                error = %error,
                "could not record the out-of-run auto-fix in task history",
            );
        }
    }

    /// The out-of-run auto-fixes live now, for readiness: each pending or
    /// running delivery leaf a drain resumed to repair its pull request.
    pub(crate) fn live_out_of_run_auto_fixes(&self) -> Result<Vec<Value>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs(DELIVERY_LEAF_JOB)?
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
                    "head_sha": repair["head_sha"],
                    "source_run_id": repair["source_run_id"],
                    "drain_run_id": repair["drain_run_id"],
                }))
            })
            .collect())
    }
}
