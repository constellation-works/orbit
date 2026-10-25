//! The backstop's runtime operations: the clock tick, preparation, application and recovery checkouts.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::git::{GIT_CHECKOUT_TIMEOUT, run_git, run_git_within};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    FinalRecoveryDecision, JobRun, JobRunState, JobRunTrigger, MAX_DECISION_TEXT_CHARS,
};
use serde_json::{Value, json};

use super::super::final_recovery::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
};
use super::super::helpers::SYSTEM_ACTOR_LABEL;
use super::super::retained_candidate::RetainedCandidate;
use super::episode::{
    BlockEpisode, BlockSource, EpisodeDisposition, FinalRecoveryRecord, episode_disposition,
};
use super::input::{
    BlockedRecoveryInput, BlockedRecoveryPreparation, BlockedRecoveryTick, BlockedRecoveryView,
    PreparedBlockedRecovery,
};
use super::{
    BLOCKED_TASK_RECOVERY_JOB, EPISODE_KEY_FIELD, MAX_ACTIVE_BLOCKED_RECOVERIES,
    MAX_EPISODE_AGE_HOURS, RUN_SCAN_LIMIT, TRIGGER_CONSUMER, TRIGGER_NAME, recovery_checkout_path,
};
use crate::OrbitRuntime;
use crate::application::contracts::RetryKey;
use crate::application::task::TaskJobPort;

impl OrbitRuntime {
    /// Why the backstop does not run on this runtime, if it does not.
    pub fn blocked_task_recovery_disabled_reason(&self) -> Option<String> {
        if self.worker_invocation().is_some() {
            return Some("a claimed worker never recovers tasks; its owner does".to_string());
        }
        if let Some(owner) = self.coordination_write_owner() {
            return Some(format!(
                "this replica checkout does not own its task records; machine '{owner}' recovers them"
            ));
        }
        if self.context.settings().final_recovery_crews().is_empty() {
            return Some("`workflow.purpose_crews.final_recovery` is empty".to_string());
        }
        None
    }

    /// Every blocked task whose block the backstop owns, with what it does
    /// about each.
    pub fn blocked_recovery_view(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<BlockedRecoveryView>, OrbitError> {
        let mut views = Vec::new();
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?
        {
            let history = self.get_task_history(&task.id)?;
            let Some(episode) = BlockEpisode::current(&task, &history) else {
                continue;
            };
            let comments = self.get_task_comments(&task.id)?;
            let artifacts = self.get_task_artifact_manifest(&task.id)?;
            let disposition = episode_disposition(
                &episode,
                task.updated_at,
                &history,
                &comments,
                &artifacts,
                now,
            );
            views.push(BlockedRecoveryView {
                episode,
                disposition,
            });
        }
        views.sort_by_key(|view| view.episode.blocked_at);
        Ok(views)
    }

    /// One backstop tick: settle recovery runs that ended without a decision,
    /// then dispatch recoveries for eligible episodes, at most
    /// [`MAX_ACTIVE_BLOCKED_RECOVERIES`] live at once.
    pub fn run_blocked_task_recovery_tick(
        &self,
        now: DateTime<Utc>,
    ) -> Result<BlockedRecoveryTick, OrbitError> {
        self.run_blocked_task_recovery_tick_with(now, &mut |input| {
            self.submit_routine_run(
                BLOCKED_TASK_RECOVERY_JOB,
                input,
                SYSTEM_ACTOR_LABEL,
                Some(RetryKey {
                    field: EPISODE_KEY_FIELD,
                    scan_limit: RUN_SCAN_LIMIT,
                }),
                JobRunTrigger::state_routine(TRIGGER_NAME, TRIGGER_CONSUMER),
            )
        })
    }

    /// [`Self::run_blocked_task_recovery_tick`] with the run submission
    /// injected, so the selection is testable without spawning workers.
    pub(crate) fn run_blocked_task_recovery_tick_with(
        &self,
        now: DateTime<Utc>,
        submit: &mut dyn FnMut(Value) -> Result<String, OrbitError>,
    ) -> Result<BlockedRecoveryTick, OrbitError> {
        let mut tick = BlockedRecoveryTick::default();
        if let Some(reason) = self.blocked_task_recovery_disabled_reason() {
            tick.skipped = Some(reason);
            return Ok(tick);
        }
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(BLOCKED_TASK_RECOVERY_JOB.to_string()),
            created_since: Some(now - Duration::hours(MAX_EPISODE_AGE_HOURS * 2)),
            limit: Some(RUN_SCAN_LIMIT),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        for run in &runs {
            match self.settle_undecided_recovery_run(run, &runs) {
                Ok(true) => tick.settled.push(run_task_id(run).unwrap_or_default()),
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    target: "orbit.core.blocked_recovery",
                    run_id = %run.run_id,
                    "failed to settle a recovery run that ended without a decision: {error}"
                ),
            }
        }

        let active = runs
            .iter()
            .filter(|run| !run.state.is_terminal() && run.state != JobRunState::Skipped)
            .count();
        let mut capacity = MAX_ACTIVE_BLOCKED_RECOVERIES.saturating_sub(active);
        if capacity == 0 {
            return Ok(tick);
        }
        for view in self.blocked_recovery_view(now)? {
            if capacity == 0 {
                break;
            }
            if view.disposition != EpisodeDisposition::Eligible {
                continue;
            }
            let key = view.episode.key();
            if runs
                .iter()
                .any(|run| run_input_field(run, EPISODE_KEY_FIELD) == Some(key.as_str()))
            {
                continue;
            }
            let task = self.get_task(&view.episode.task_id)?;
            let input = BlockedRecoveryInput {
                task_id: task.id.clone(),
                episode_key: key,
                block_source: view.episode.source.as_str().to_string(),
                failed_run_id: view.episode.failed_run_id.clone(),
                observed: self.final_recovery_revision(&task)?,
            };
            match submit(input.to_json()) {
                Ok(run_id) => {
                    tick.dispatched.push((task.id.clone(), run_id));
                    capacity -= 1;
                }
                Err(error) => tracing::warn!(
                    target: "orbit.core.blocked_recovery",
                    task_id = %task.id,
                    "failed to dispatch blocked-task recovery: {error}"
                ),
            }
        }
        Ok(tick)
    }

    /// Whether `input`'s episode is still the task's current, untouched block.
    /// `Err` names why the recovery must not act.
    pub(crate) fn blocked_recovery_still_current(
        &self,
        input: &BlockedRecoveryInput,
    ) -> Result<Result<BlockEpisode, String>, OrbitError> {
        let task = self.get_task(&input.task_id)?;
        if self
            .final_recovery_revision_after(&input.observed, &task)?
            .changed_since(&input.observed)
        {
            return Ok(Err(format!(
                "task changed after the recovery was dispatched (was {} at {}, now {} at {})",
                input.observed.status,
                input.observed.updated_at.to_rfc3339(),
                task.status,
                task.updated_at.to_rfc3339()
            )));
        }
        let history = self.get_task_history(&task.id)?;
        match BlockEpisode::current(&task, &history) {
            Some(episode) if episode.key() == input.episode_key => Ok(Ok(episode)),
            _ => Ok(Err(
                "the block this recovery was dispatched for is no longer current".to_string(),
            )),
        }
    }

    /// A recovery run's first step: re-check that the episode is still the
    /// task's untouched block, gather the failure, resolve the base, and
    /// create the agent's checkout.
    pub(crate) fn prepare_blocked_task_recovery(
        &self,
        input: &BlockedRecoveryInput,
        recovery_run_id: &str,
    ) -> Result<BlockedRecoveryPreparation, OrbitError> {
        let episode = match self.blocked_recovery_still_current(input)? {
            Ok(episode) => episode,
            Err(reason) => return Ok(BlockedRecoveryPreparation::Skip { reason }),
        };
        let task = self.get_task(&input.task_id)?;
        let failed_run = match &episode.failed_run_id {
            Some(run_id) if self.task_run_is_local(&task) => self.stored_job_run(run_id)?,
            _ => None,
        };
        let failed_step = failed_run.as_ref().and_then(|run| {
            run.steps
                .iter()
                .rev()
                .find(|step| step.error_code.is_some() || step.error_message.is_some())
        });
        // A claimed leaf lives on its follower. Its failure settlement leaves
        // the diagnostic on the owner's task, without copying the run state.
        let settled_failure = if episode.source == BlockSource::ClaimFailed && failed_run.is_none()
        {
            Some(task.execution_summary.clone())
        } else {
            None
        };
        let diagnostic = failed_step
            .and_then(|step| step.error_message.clone())
            .filter(|message| !message.trim().is_empty())
            .or_else(|| settled_failure.filter(|message| !message.trim().is_empty()))
            .unwrap_or_else(|| episode.note.clone());
        let error_message = match &task.job_run_machine {
            Some(location) if !self.task_run_is_local(&task) => format!(
                "Failed run executed on {} ({}); its run state is on that machine.\n\n{diagnostic}",
                location
                    .machine_name
                    .as_deref()
                    .unwrap_or(&location.machine_id),
                location.machine_id,
            ),
            _ => diagnostic,
        };
        let retained_candidate =
            self.retained_candidate(&task, episode.failed_run_id.as_deref(), failed_run.as_ref());
        let (base_ref, base_sha) = self.recovery_base()?;
        let checkout = self.create_recovery_checkout(recovery_run_id, &base_sha)?;
        Ok(BlockedRecoveryPreparation::Ready(Box::new(
            PreparedBlockedRecovery {
                checkout,
                base_ref,
                base_sha,
                failed_run_id: episode
                    .failed_run_id
                    .clone()
                    .unwrap_or_else(|| recovery_run_id.to_string()),
                failed_step_id: failed_step
                    .map(|step| step.target_id.to_string())
                    .unwrap_or_else(|| episode.source.as_str().to_string()),
                job_id: failed_run.as_ref().map(|run| run.job_id.to_string()),
                error_message,
                retained_candidate,
            },
        )))
    }

    /// The retained candidate of `input`'s failed run, read now.
    fn episode_candidate(&self, input: &BlockedRecoveryInput) -> RetainedCandidate {
        let task = match self.get_task(&input.task_id) {
            Ok(task) => task,
            Err(error) => {
                return RetainedCandidate::Absent {
                    reason: format!("the task could not be read: {error}"),
                };
            }
        };
        let failed_run = match &input.failed_run_id {
            Some(run_id) if self.task_run_is_local(&task) => {
                self.stored_job_run(run_id).unwrap_or_default()
            }
            _ => None,
        };
        self.retained_candidate(&task, input.failed_run_id.as_deref(), failed_run.as_ref())
    }

    /// The workspace base the agent inspects and a `complete_no_diff` commit
    /// is checked against: `origin/<base>` when it resolves, else `<base>`.
    /// Nothing is fetched; a lagging ref makes a landed commit unprovable,
    /// which escalates rather than completes.
    fn recovery_base(&self) -> Result<(String, String), OrbitError> {
        let branch = self.workspace_base_branch().trim().to_string();
        if branch.is_empty() || branch.starts_with('-') {
            return Err(OrbitError::InvalidInput(format!(
                "workspace base branch '{branch}' cannot be resolved"
            )));
        }
        for candidate in [format!("origin/{branch}"), branch.clone()] {
            let spec = format!("{candidate}^{{commit}}");
            let output = run_git(
                &self.paths().repo_root,
                &["rev-parse", "--verify", "--quiet", &spec],
            )?;
            let sha = output.stdout.trim();
            if output.success && !sha.is_empty() {
                return Ok((candidate, sha.to_string()));
            }
        }
        Err(OrbitError::Execution(format!(
            "neither 'origin/{branch}' nor '{branch}' resolves to a commit"
        )))
    }

    /// Apply a recovery run's final-recovery result to its task.
    ///
    /// `output` is the decision object the agent returned, or `None` when it
    /// returned none, which escalates. `resume` is escalated: this backstop
    /// restarts work only by `requeue`. Every escalation names the failed
    /// run's retained candidate. The decision comment names
    /// `recovery_run_id`. A verified `complete_no_diff` moves the task to
    /// `review`, never `done`.
    pub(crate) fn apply_blocked_task_recovery(
        &self,
        input: &BlockedRecoveryInput,
        recovery_run_id: &str,
        base_ref: &str,
        output: Option<&Value>,
    ) -> Result<FinalRecoveryOutcome, OrbitError> {
        let decision = match without_resume(FinalRecoveryDecision::from_output(output), input) {
            FinalRecoveryDecision::Escalate {
                diagnosis,
                human_action,
            } => FinalRecoveryDecision::Escalate {
                diagnosis,
                human_action: with_line(&human_action, &self.episode_candidate(input).summary()),
            },
            decision => decision,
        };
        let output = serde_json::to_value(&decision).map_err(|error| {
            OrbitError::Execution(format!("serialize final recovery decision: {error}"))
        })?;
        let request = FinalRecoveryRequest {
            task_id: input.task_id.clone(),
            run_id: recovery_run_id.to_string(),
            observed: input.observed.clone(),
            repo_root: self.paths().repo_root.clone(),
            base_ref: base_ref.to_string(),
            completion: FinalRecoveryCompletion::Review,
            requeue_bound: FinalRecoveryRequeueBound::default(),
        };
        self.apply_final_recovery(&request, Some(&output))
    }

    /// Create the detached checkout of `base_sha` a recovery run's agent
    /// inspects, replacing a leftover from an earlier attempt of the same run.
    /// Prepare its ignored `.orbit` deny root before any sandboxed launch.
    pub(crate) fn create_recovery_checkout(
        &self,
        recovery_run_id: &str,
        base_sha: &str,
    ) -> Result<PathBuf, OrbitError> {
        let path = recovery_checkout_path(&self.paths().state_dir, recovery_run_id)?;
        self.remove_recovery_checkout(recovery_run_id)?;
        if let Some(parent) = path.parent() {
            orbit_common::fs::io::create_private_dir_all(parent).map_err(|error| {
                OrbitError::Execution(format!(
                    "create recovery checkout directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let target = path.to_string_lossy().to_string();
        let output = run_git_within(
            &self.paths().repo_root,
            &["worktree", "add", "--detach", "--quiet", &target, base_sha],
            GIT_CHECKOUT_TIMEOUT,
        )?;
        if !output.success {
            return Err(OrbitError::Execution(format!(
                "create recovery checkout {target} at {base_sha}: {}",
                output.stderr.trim()
            )));
        }
        // Managed task worktrees already create this root through scratch
        // setup. Bubblewrap needs an existing inode for the read-only bind,
        // and Seatbelt can deny the same subtree. Keep it empty here; the
        // launcher prepares scratch, but no task/runtime stores are copied.
        let denied_root = path.join(".orbit");
        orbit_common::fs::io::create_private_dir(&denied_root).map_err(|error| {
            OrbitError::Execution(format!(
                "prepare recovery checkout deny root {}: {error}",
                denied_root.display()
            ))
        })?;
        path.canonicalize().map_err(|error| {
            OrbitError::Execution(format!("resolve recovery checkout {target}: {error}"))
        })
    }

    /// Remove a recovery run's checkout, if it exists.
    pub(crate) fn remove_recovery_checkout(&self, recovery_run_id: &str) -> Result<(), OrbitError> {
        let path = recovery_checkout_path(&self.paths().state_dir, recovery_run_id)?;
        if !path.exists() {
            return Ok(());
        }
        let target = path.to_string_lossy().to_string();
        let output = run_git_within(
            &self.paths().repo_root,
            &["worktree", "remove", "--force", &target],
            GIT_CHECKOUT_TIMEOUT,
        )?;
        if output.success {
            return Ok(());
        }
        // Not a registered worktree (an interrupted `worktree add`): the
        // directory is this run's own scratch, so remove it and prune.
        std::fs::remove_dir_all(&path).map_err(|error| {
            OrbitError::Execution(format!("remove recovery checkout {target}: {error}"))
        })?;
        let _ = run_git(&self.paths().repo_root, &["worktree", "prune"]);
        Ok(())
    }

    /// Escalate the episode of a recovery run that ended without applying a
    /// decision. Returns whether a decision was recorded. A run of the same
    /// episode that is still live or succeeded (a resumed attempt) owns the
    /// decision instead, and an episode that already has one is left alone.
    fn settle_undecided_recovery_run(
        &self,
        run: &JobRun,
        runs: &[JobRun],
    ) -> Result<bool, OrbitError> {
        if !run.state.is_terminal() || run.state == JobRunState::Success {
            return Ok(false);
        }
        let Some(input) = run.input.as_ref() else {
            return Ok(false);
        };
        let input = BlockedRecoveryInput::from_json(input).map_err(OrbitError::InvalidInput)?;
        self.remove_recovery_checkout(&run.run_id)?;
        let sibling_owns_decision = runs.iter().any(|other| {
            other.run_id != run.run_id
                && run_input_field(other, EPISODE_KEY_FIELD) == Some(input.episode_key.as_str())
                && (other.state == JobRunState::Success
                    || (!other.state.is_terminal() && other.state != JobRunState::Skipped))
        });
        if sibling_owns_decision {
            return Ok(false);
        }
        let episode = match self.blocked_recovery_still_current(&input)? {
            Ok(episode) => episode,
            Err(reason) => {
                tracing::debug!(
                    target: "orbit.core.blocked_recovery",
                    run_id = %run.run_id,
                    task_id = %input.task_id,
                    "recovery run ended without a decision; not settling: {reason}"
                );
                return Ok(false);
            }
        };
        if FinalRecoveryRecord::newest_since(
            &self.get_task_history(&input.task_id)?,
            &self.get_task_comments(&input.task_id)?,
            episode.blocked_at,
        )
        .is_some()
        {
            return Ok(false);
        }
        let escalation = json!({
            "decision": "escalate",
            "diagnosis": format!(
                "blocked-task recovery run {} ended {} before it applied a decision",
                run.run_id, run.state
            ),
            "human_action": format!(
                "Inspect `orbit run show {}`, then move the task by hand.",
                run.run_id
            ),
        });
        let base_ref = self.workspace_base_branch().to_string();
        self.apply_blocked_task_recovery(&input, &run.run_id, &base_ref, Some(&escalation))?;
        Ok(true)
    }
}

/// `decision` with `resume` replaced by an escalation naming the failed run.
fn without_resume(
    decision: FinalRecoveryDecision,
    input: &BlockedRecoveryInput,
) -> FinalRecoveryDecision {
    let FinalRecoveryDecision::Resume { step_id, rationale } = decision else {
        return decision;
    };
    let step = step_id.trim();
    let human_action = match &input.failed_run_id {
        Some(run_id) => format!(
            "Resume `{run_id}` from `{step}` with `orbit job resume {run_id}` if the diagnosis \
             holds, or requeue the task."
        ),
        None => "Requeue the task once the diagnosis is addressed.".to_string(),
    };
    FinalRecoveryDecision::Escalate {
        diagnosis: format!(
            "final recovery proposed resuming from `{step}`, which the blocked-task backstop \
             never does: {}",
            rationale.trim()
        ),
        human_action,
    }
}

/// `text` followed by `line`, shortening both so the result stays within a
/// decision field's bound and the applier does not refuse it as malformed.
fn with_line(text: &str, line: &str) -> String {
    let line: String = line.chars().take(MAX_DECISION_TEXT_CHARS / 2).collect();
    let room = MAX_DECISION_TEXT_CHARS.saturating_sub(line.chars().count() + 2);
    let text = text.trim();
    if text.chars().count() <= room {
        return format!("{text}\n{line}");
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!("{kept}…\n{line}")
}

/// The `key=value` token in a history note.
pub(super) fn note_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let start = text.find(key)? + key.len();
    let value = text[start..].split([',', ' ', ';', '\n']).next()?;
    (!value.is_empty() && value != "-").then_some(value)
}

fn run_input_field<'a>(run: &'a JobRun, field: &str) -> Option<&'a str> {
    run.input.as_ref()?.get(field)?.as_str()
}

fn run_task_id(run: &JobRun) -> Option<String> {
    run_input_field(run, "task_id").map(str::to_string)
}
