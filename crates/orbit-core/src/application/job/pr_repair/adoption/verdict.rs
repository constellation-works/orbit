use super::super::held::{
    HELD_HANDBACK_CEILING, auto_fix_stopped, handed_back_hold, held_backoff, held_backoff_until,
    held_handbacks, held_refusal, unrepaired_check_repair,
};
use super::super::ledger::{budget_note, ledger_admits};
use super::pass_types::{BlockerRead, CLEAN_RECHECK, Repair};
use super::{PassReads, RepairAuthority, RepairObservation, Verdict};
use crate::OrbitRuntime;
use crate::application::job::resume::shelving_run_id;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::application::task::SYSTEM_ACTOR_LABEL;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{
    NO_AUTO_APPROVE_TAG, TaskHistoryEntry, TaskStatus, names_task_delivery_pull_request,
};
use orbit_types::workflow::{
    COMPLETION_HOLD_KEY, DEFAULT_BRANCH_PREFIX, DeliveryRefusal, JobRun, JobRunState,
    PR_AUTO_FIX_PUSHED_EVENT, PipelineState, PrAutoFixBlocker, current_completion_hold,
    pr_auto_fix_pushes,
};
use serde_json::{Value, json};
use std::time::Instant;

impl OrbitRuntime {
    /// `detail` of a refusal, followed by the attempts `observation`'s pull
    /// request has spent and has left, or why ledger evidence is unavailable
    /// when the task ledger cannot be read. A pull request with nothing to
    /// repair (`no_blocker`, `checked_recently`) is reported as it is.
    pub(in crate::application::job::pr_repair) fn skip_detail_with_budget(
        &self,
        task_id: &str,
        observation: &RepairObservation,
        reason: &str,
        detail: &str,
    ) -> String {
        if matches!(reason, "no_blocker" | "checked_recently") {
            return detail.to_string();
        }
        let Some(number) = observation.pr_number.as_deref() else {
            return detail.to_string();
        };
        let history = self.get_task_history(task_id);
        let note = budget_note(history.as_deref(), number);
        format!("{detail}; {note}")
    }

    /// Whether the pull request of `task_id`'s newest leaf `run` is repaired,
    /// and if not, why. A repair still has to be planned
    /// ([`Self::plan_repair`]) before it starts.
    pub(in crate::application::job::pr_repair) fn repair_verdict(
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
        let mut auto_fix_hold = false;
        let mut unrepaired_repair_hold = false;
        let held = if run.state == JobRunState::Held {
            let refusal = match self.stores().jobs().get_job_run(&run.run_id) {
                Ok(Some(full)) => {
                    auto_fix_hold = auto_fix_stopped(&full);
                    unrepaired_repair_hold = unrepaired_check_repair(&full);
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
                    if observation.pr_number.is_none()
                        && let Ok(task) = self.get_task(task_id)
                    {
                        observation.pr_number = task.github_pr_number().map(str::to_string);
                    }
                    return skip("store_unreadable", format!("read the run's steps: {error}"));
                }
            };
            match refusal {
                _ if operator_stopped => None,
                Some(refusal) if handed_back_hold(refusal) => Some(refusal.error_code()),
                _ if auto_fix_hold => Some("auto_fix_stopped"),
                _ if unrepaired_repair_hold => Some(DeliveryRefusal::RepairDispatched.error_code()),
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
        observation.hold_code = held;
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
            // backoff; the ceiling still binds both. Auto-fix stops and
            // unrepaired check-repair holds do not use the held-leaf backoff:
            // stops only clear on green checks and check repairs are bounded
            // by the PR's repair budget.
            if !reads.fresh
                && !auto_fix_hold
                && !unrepaired_repair_hold
                && let Some(until) = held_backoff_until(
                    run.finished_at,
                    held_backoff(&history, &pr_number, refusal),
                    Utc::now(),
                )
            {
                return skip(
                    refusal,
                    format!(
                        "its delivery run {} held as {}; it is handed back no earlier than {}",
                        run.run_id,
                        refusal,
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
        let (number, head, published_sha) = match self.read_run_state(&run.run_id) {
            Ok(Some(state)) => {
                let opened = state.pipeline.get("pr_open").unwrap_or(&Value::Null);
                match (
                    opened.get("pr_number").and_then(Value::as_str),
                    opened.get("head").and_then(Value::as_str),
                ) {
                    (Some(number), Some(head)) => (
                        number.to_string(),
                        head.to_string(),
                        Self::pipeline_published_sha(&state),
                    ),
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
        // An auto-fix stop, unrepaired check-repair hold or absent-check hold
        // is reconsidered only on the held run's own published head: the
        // newest publication its completion rounds recorded, advanced through
        // its lineage's auto-fix pushes. A replacement head on the same
        // branch, red, conflicting or green, is not the held run's to repair
        // or hand back.
        let check_runs_absent_hold = held == Some(DeliveryRefusal::CheckRunsAbsent.error_code());
        let published_sha = if auto_fix_hold || unrepaired_repair_hold || check_runs_absent_hold {
            match self.resolve_held_published_sha(run, &history, &number, published_sha) {
                Ok(sha) => sha,
                Err(error) => {
                    return skip(
                        "store_unreadable",
                        format!("read the run's lineage: {error}"),
                    );
                }
            }
        } else {
            published_sha
        };
        if (auto_fix_hold || unrepaired_repair_hold || check_runs_absent_hold)
            && published_sha.as_deref() != Some(blockers.head_sha.as_str())
        {
            return skip(
                "publication_identity_changed",
                format!("#{number} no longer matches the held run's published head"),
            );
        }
        // An auto-fix stop is reconsidered only after positive, complete check
        // evidence. Red, pending, unreported or absent checks must not trigger
        // another repair: a configured required check that has not reported
        // on the head is not green, even when its siblings passed.
        if auto_fix_hold
            && (blockers.requires_status_checks.is_none()
                || blockers.conflicting
                || !blockers.failed_required_checks.is_empty()
                || !blockers.pending_required_checks.is_empty()
                || !blockers.unreported_required_checks.is_empty()
                || (blockers.passed_required_checks.is_empty()
                    && blockers.requires_status_checks != Some(false)))
        {
            return skip(
                "auto_fix_stopped",
                format!("#{number} does not yet have all required checks green"),
            );
        }
        // An unrepaired check repair retries through the ledger while a blocker
        // and budget remain; if no blocker remains, it hands back only once
        // the published head reports every configured required check green.
        if unrepaired_repair_hold
            && blocker.is_none()
            && (blockers.requires_status_checks.is_none()
                || !blockers.pending_required_checks.is_empty()
                || !blockers.unreported_required_checks.is_empty()
                || (blockers.passed_required_checks.is_empty()
                    && blockers.requires_status_checks != Some(false)))
        {
            return skip(
                DeliveryRefusal::RepairDispatched.error_code(),
                format!("#{number} does not yet have all required checks green"),
            );
        }
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
                if refusal == DeliveryRefusal::CheckRunsAbsent.error_code()
                    && blockers.pending_required_checks.is_empty()
                    && blockers.passed_required_checks.is_empty()
                    && blockers.requires_status_checks != Some(false)
                {
                    return skip(
                        refusal,
                        format!(
                            "#{number} head {} still reports no required check run, so the {} \
                             hold stays until one does; this pass spends no hand-back",
                            blockers.head_sha, refusal
                        ),
                    );
                }
                let handed_back = held_handbacks(&history, &number, &blockers.head_sha, refusal);
                if handed_back >= HELD_HANDBACK_CEILING {
                    return skip(
                        refusal,
                        format!(
                            "#{number} was handed back {handed_back} times on {} and its delivery \
                             held again as {}, the ceiling of {HELD_HANDBACK_CEILING} per head \
                             and hold; a person decides it now",
                            blockers.head_sha, refusal
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
            marker["hold_code"] = json!(hold);
            if hold == "auto_fix_stopped" || unrepaired_repair_hold {
                marker["reason"] = json!("checks_green_after_hold");
            }
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

    /// The latest non-null publication SHA recorded in `state.pipeline`.
    fn pipeline_published_sha(state: &PipelineState) -> Option<String> {
        ["re_push_2", "re_push", "landing_push", "push"]
            .into_iter()
            .find_map(|step| {
                state
                    .pipeline
                    .get(step)
                    .filter(|push| !push.is_null())
                    .and_then(|push| push.get("local_sha"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
    }

    /// Advance `published_sha` through contiguous `pr_auto_fix_pushed` records
    /// written by `system` within `run`'s retry ancestry.
    fn resolve_held_published_sha(
        &self,
        run: &JobRun,
        history: &[TaskHistoryEntry],
        pr_number: &str,
        published_sha: Option<String>,
    ) -> Result<Option<String>, OrbitError> {
        let Some(mut sha) = published_sha else {
            return Ok(None);
        };
        if !history
            .iter()
            .any(|entry| entry.event == PR_AUTO_FIX_PUSHED_EVENT)
        {
            return Ok(Some(sha));
        }
        let (ancestors, _) = self.resume_lineage_run_ids(run)?;
        let system_history: Vec<_> = history
            .iter()
            .filter(|entry| entry.by == SYSTEM_ACTOR_LABEL)
            .cloned()
            .collect();
        for push in pr_auto_fix_pushes(&system_history, pr_number) {
            if ancestors.contains(&push.run_id) && push.from_head_sha == sha {
                sha = push.to_head_sha;
            }
        }
        Ok(Some(sha))
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
            Ok(blockers) => Ok(BlockerRead::Complete(blockers.map(Box::new))),
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
}
